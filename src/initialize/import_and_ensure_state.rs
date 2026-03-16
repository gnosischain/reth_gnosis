use crate::cli::import_era::ERA_IMPORTED_FLAG;
use crate::initialize::download_init_state::{
    ensure_state, DownloadStateSpec, COMPRESSED_STATE_FILE,
};
use crate::{spec::gnosis_spec::GnosisChainSpecParser, GnosisNode};
use alloy_consensus::BlockHeader;
use alloy_genesis::GenesisAccount;
use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_rlp::Decodable;
use gnosis_primitives::header::GnosisHeader;
use reth_cli_commands::common::{AccessRights, Environment, EnvironmentArgs};
use reth_cli_commands::init_state::without_evm;
use reth_codecs::Compact;
use reth_db::table::{Decompress, Table};
use reth_db::tables;
use reth_db_api::{
    cursor::{DbCursorRW, DbDupCursorRW},
    models::{AccountBeforeTx, BlockNumberAddress},
    transaction::DbTxMut,
};
use reth_db_common::init::insert_history;
use reth_db_common::DbTool;
use reth_etl::Collector;
use reth_primitives::{SealedHeader, StaticFileSegment};
use reth_primitives_traits::{Account, Bytecode, StorageEntry};
use reth_provider::{
    BlockHashReader, BlockNumReader, DBProvider, DatabaseProviderFactory,
    HeaderProvider, HistoryWriter, NodePrimitivesProvider, RocksDBProviderFactory,
    StageCheckpointWriter, StaticFileProviderFactory, StaticFileWriter,
    StorageSettingsCache, TrieWriter,
};
use reth_stages_types::{StageCheckpoint, StageId};
use reth_trie::{IntermediateStateRootState, StateRoot as StateRootComputer, StateRootProgress};
use reth_trie_db::DatabaseStateRoot;
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tokio::runtime::Runtime;
use tracing::info;
use zstd::Decoder;

const IMPORTED_FLAG: &str = "imported.flag";

/// Soft limit for the number of flushed updates after which to log progress summary.
const SOFT_LIMIT_COUNT_FLUSHED_UPDATES: usize = 1_000_000;

/// ETL collector buffer size. 64 MB keeps peak RSS low during parse phase.
const ETL_BUFFER_BYTES: usize = 64 * 1024 * 1024;

/// Maximum accounts per DB write batch.
const MAX_ACCOUNTS_PER_BATCH: usize = 10_000;

/// Maximum total storage entries across all accounts in a DB write batch.
const MAX_STORAGE_PER_BATCH: usize = 200_000;

/// Sub-batch size for hashed storage writes. Hashed storage must be sorted by
/// keccak256, so we collect into a Vec, sort, and write. This limits that Vec
/// to ~48 MB instead of potentially gigabytes for contracts with millions of slots.
const HASHED_STORAGE_CHUNK: usize = 500_000;

/// Commit the MDBX write transaction after this many storage entries have been
/// written. This releases dirty pages back to the OS, preventing unbounded
/// memory growth from MDBX's copy-on-write page tracking.
const COMMIT_STORAGE_THRESHOLD: usize = 2_000_000;

/// Get an instance of key for given table
fn table_key<T: Table>(key: &str) -> Result<T::Key, eyre::Error> {
    serde_json::from_str(key).map_err(|e| eyre::eyre!(e))
}

/// Reads the header RLP from a file and returns the Header.
fn read_header_from_file(path: PathBuf) -> Result<GnosisHeader, eyre::Error> {
    let mut file = File::open(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let header = GnosisHeader::decode(&mut &buf[..])?;
    Ok(header)
}

/// Type to deserialize state root from the first line of the state dump.
#[derive(Debug, Deserialize)]
struct StateRoot {
    root: B256,
}

/// An account entry as it appears in the state dump JSONL file.
#[derive(Debug, Deserialize)]
struct GenesisAccountWithAddress {
    #[serde(flatten)]
    genesis_account: GenesisAccount,
    address: Address,
}

/// Write a batch of sorted accounts directly to all required DB tables.
///
/// Bypasses the upstream `insert_state` / `insert_genesis_hashes` which create
/// multiple redundant copies of storage data (BundleState, ExecutionOutcome,
/// nested BTreeMaps). Hashed storage writes are sub-batched to avoid
/// multi-GB sort buffers for contracts with millions of storage slots.
fn flush_batch_direct<Provider>(
    batch: &[(Address, GenesisAccount)],
    provider_rw: &Provider,
    block: u64,
) -> Result<(), eyre::Error>
where
    Provider: DBProvider<Tx: DbTxMut>
        + HistoryWriter
        + StorageSettingsCache
        + RocksDBProviderFactory
        + NodePrimitivesProvider,
{
    let tx = provider_rw.tx_ref();

    // ── PlainAccountState + Bytecodes ──
    {
        let mut acct_cursor = tx.cursor_write::<tables::PlainAccountState>()?;
        let mut code_cursor = tx.cursor_write::<tables::Bytecodes>()?;
        for (address, ga) in batch {
            let bytecode_hash = if let Some(code) = &ga.code {
                let bytecode = Bytecode::new_raw_checked(code.clone())
                    .map_err(|e| eyre::eyre!("bad bytecode for {address}: {e}"))?;
                let hash = bytecode.hash_slow();
                code_cursor.upsert(hash, &bytecode)?;
                Some(hash)
            } else {
                None
            };
            let account = Account {
                nonce: ga.nonce.unwrap_or_default(),
                balance: ga.balance,
                bytecode_hash,
            };
            acct_cursor.upsert(*address, &account)?;
        }
    }

    // ── PlainStorageState (streaming, no collection) ──
    {
        let mut cursor = tx.cursor_dup_write::<tables::PlainStorageState>()?;
        for (address, ga) in batch {
            if let Some(storage) = &ga.storage {
                for (&key, &value) in storage {
                    let value = U256::from_be_bytes(value.0);
                    if !value.is_zero() {
                        cursor.upsert(*address, &StorageEntry { key, value })?;
                    }
                }
            }
        }
    }

    // ── AccountChangeSets ──
    {
        let mut cursor = tx.cursor_dup_write::<tables::AccountChangeSets>()?;
        for (address, _) in batch {
            cursor.append_dup(
                block,
                AccountBeforeTx {
                    address: *address,
                    info: None,
                },
            )?;
        }
    }

    // ── StorageChangeSets (streaming, no collection) ──
    {
        let mut cursor = tx.cursor_dup_write::<tables::StorageChangeSets>()?;
        for (address, ga) in batch {
            if let Some(storage) = &ga.storage {
                let block_addr = BlockNumberAddress((block, *address));
                for (&key, _) in storage {
                    cursor.append_dup(
                        block_addr,
                        StorageEntry {
                            key,
                            value: U256::ZERO,
                        },
                    )?;
                }
            }
        }
    }

    // ── HashedAccounts (sorted by keccak256(address)) ──
    {
        let mut hashed: Vec<_> = batch
            .iter()
            .map(|(addr, ga)| {
                let bytecode_hash = ga.code.as_ref().and_then(|code| {
                    Bytecode::new_raw_checked(code.clone())
                        .ok()
                        .map(|bc| bc.hash_slow())
                });
                (
                    keccak256(*addr),
                    Account {
                        nonce: ga.nonce.unwrap_or_default(),
                        balance: ga.balance,
                        bytecode_hash,
                    },
                )
            })
            .collect();
        hashed.sort_unstable_by_key(|(h, _)| *h);

        let mut cursor = tx.cursor_write::<tables::HashedAccounts>()?;
        for (hash, account) in &hashed {
            cursor.upsert(*hash, account)?;
        }
    }

    // ── HashedStorages (sub-batched: collect HASHED_STORAGE_CHUNK entries,
    //    sort by (keccak256(addr), keccak256(key)), write, repeat) ──
    {
        let mut cursor = tx.cursor_dup_write::<tables::HashedStorages>()?;
        let mut chunk: Vec<(B256, StorageEntry)> = Vec::with_capacity(HASHED_STORAGE_CHUNK);

        for (address, ga) in batch {
            if let Some(storage) = &ga.storage {
                let hashed_addr = keccak256(*address);
                for (&key, &value) in storage {
                    let value = U256::from_be_bytes(value.0);
                    if !value.is_zero() {
                        chunk.push((
                            hashed_addr,
                            StorageEntry {
                                key: keccak256(key),
                                value,
                            },
                        ));
                    }

                    if chunk.len() >= HASHED_STORAGE_CHUNK {
                        chunk.sort_unstable_by(|a, b| {
                            a.0.cmp(&b.0).then(a.1.key.cmp(&b.1.key))
                        });
                        for (ha, entry) in &chunk {
                            cursor.upsert(*ha, entry)?;
                        }
                        chunk.clear();
                    }
                }
            }
        }

        // Flush remaining
        if !chunk.is_empty() {
            chunk.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.key.cmp(&b.1.key)));
            for (ha, entry) in &chunk {
                cursor.upsert(*ha, entry)?;
            }
        }
    }

    // ── History indices ──
    insert_history(
        provider_rw,
        batch.iter().map(|(a, g)| (a, g)),
        block,
    )?;

    Ok(())
}

/// Compute the state root from scratch by walking the entire hashed state.
fn compute_state_root_from_scratch<Provider>(provider: &Provider) -> Result<B256, eyre::Error>
where
    Provider: DBProvider<Tx: DbTxMut> + TrieWriter,
{
    let tx = provider.tx_ref();
    let mut intermediate_state: Option<IntermediateStateRootState> = None;
    let mut total_flushed_updates: usize = 0;

    loop {
        let state_root =
            StateRootComputer::from_tx(tx).with_intermediate_state(intermediate_state);

        match state_root.root_with_progress()? {
            StateRootProgress::Progress(state, _, updates) => {
                let updated_len = provider.write_trie_updates(updates)?;
                total_flushed_updates += updated_len;

                if total_flushed_updates.is_multiple_of(SOFT_LIMIT_COUNT_FLUSHED_UPDATES) {
                    info!(target: "reth::cli",
                        total_flushed_updates,
                        "Flushing trie updates"
                    );
                }

                intermediate_state = Some(*state);
            }
            StateRootProgress::Complete(root, _, updates) => {
                let updated_len = provider.write_trie_updates(updates)?;
                total_flushed_updates += updated_len;

                info!(target: "reth::cli",
                    %root,
                    total_flushed_updates,
                    "State root computation complete"
                );

                return Ok(root);
            }
        }
    }
}

fn import_state(
    env: &EnvironmentArgs<GnosisChainSpecParser>,
    compressed_state: PathBuf,
    header: PathBuf,
    header_hash: &str,
) -> Result<(), eyre::Error> {
    let Environment {
        provider_factory, ..
    } = env.init::<GnosisNode>(AccessRights::RW)?;

    // ── Phase 1: Setup header (own transaction) ─────────────────────────
    let block = {
        let static_file_provider = provider_factory.static_file_provider();
        let provider_rw = provider_factory.database_provider_rw()?;
        let header = read_header_from_file(header)?;
        let header_hash = B256::from_str(header_hash)?;
        let last_block_number = provider_rw.last_block_number()?;

        if last_block_number == 0 {
            without_evm::setup_without_evm(
                &provider_rw,
                SealedHeader::new(header, header_hash),
                |number| GnosisHeader {
                    number,
                    ..Default::default()
                },
            )?;
            static_file_provider.commit()?;
        } else if last_block_number > 0 && last_block_number < header.number {
            return Err(eyre::eyre!(
                "Data directory should be empty when calling init-state with --without-evm-history."
            ));
        }

        let block = provider_rw.last_block_number()?;
        provider_rw.commit()?;
        block
    };

    info!(target: "reth::cli", "Initiating state import (streaming from compressed file)");

    // ── Phase 2: Parse state dump into ETL collector ────────────────────
    let (hash, expected_state_root) = {
        let provider_rw = provider_factory.database_provider_rw()?;
        let hash = provider_rw
            .block_hash(block)?
            .ok_or_else(|| eyre::eyre!("Block hash not found for block {block}"))?;
        let header = provider_rw
            .header_by_number(block)?
            .map(reth_primitives_traits::SealedHeader::seal_slow)
            .ok_or_else(|| eyre::eyre!("Header not found for block {block}"))?;
        (hash, header.state_root())
    };

    let file = File::open(&compressed_state)?;
    let decoder = Decoder::new(BufReader::new(file))?;
    let mut reader = BufReader::new(decoder);

    // First line is state root
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let dump_state_root: StateRoot = serde_json::from_str(&line)?;
    if dump_state_root.root != expected_state_root {
        return Err(eyre::eyre!(
            "State root from dump ({:?}) does not match header ({:?})",
            dump_state_root.root,
            expected_state_root
        ));
    }
    line.clear();

    info!(target: "reth::cli", "Parsing accounts from compressed state dump");
    let mut collector: Collector<Address, GenesisAccount> =
        Collector::new(ETL_BUFFER_BYTES, None);
    let mut parsed_count: usize = 0;

    loop {
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let entry: GenesisAccountWithAddress = serde_json::from_str(&line)?;
        collector.insert(entry.address, entry.genesis_account)?;
        parsed_count += 1;
        line.clear();

        if parsed_count % 500_000 == 0 {
            info!(target: "reth::cli", parsed_count, "Parsing accounts");
        }
    }
    drop(reader);

    info!(target: "reth::cli", parsed_count, "All accounts parsed, writing to database");

    // ── Phase 3: Write sorted accounts to DB with periodic commits ──────
    //
    // CRITICAL: We commit the MDBX transaction periodically to release dirty
    // pages. Without this, MDBX accumulates all copy-on-write pages for the
    // entire 27 GB import in memory, causing 20+ GB RSS.
    let total_accounts = collector.len();
    let mut accounts: Vec<(Address, GenesisAccount)> =
        Vec::with_capacity(MAX_ACCOUNTS_PER_BATCH);
    let mut batch_storage_count: usize = 0;
    let mut total_inserted: usize = 0;
    let mut storage_since_commit: usize = 0;
    let mut provider_rw = provider_factory.database_provider_rw()?;

    for (index, entry) in collector.iter()?.enumerate() {
        let (raw_address, raw_account) = entry?;
        let (address, _) = Address::from_compact(raw_address.as_slice(), raw_address.len());
        let (account, _) =
            GenesisAccount::from_compact(raw_account.as_slice(), raw_account.len());

        let account_storage = account.storage.as_ref().map_or(0, |s| s.len());

        // Flush current batch BEFORE adding if this account would exceed limits
        if !accounts.is_empty()
            && (accounts.len() >= MAX_ACCOUNTS_PER_BATCH
                || batch_storage_count + account_storage > MAX_STORAGE_PER_BATCH)
        {
            total_inserted += accounts.len();
            info!(target: "reth::cli",
                batch_accounts = accounts.len(),
                batch_storage_count,
                total_inserted,
                total_accounts,
                "Writing account batch to database"
            );
            flush_batch_direct(&accounts, &provider_rw, block)?;
            storage_since_commit += batch_storage_count;
            accounts.clear();
            batch_storage_count = 0;

            // Periodic commit to release MDBX dirty pages
            if storage_since_commit >= COMMIT_STORAGE_THRESHOLD {
                info!(target: "reth::cli", storage_since_commit, "Committing transaction to release memory");
                provider_rw.commit()?;
                provider_rw = provider_factory.database_provider_rw()?;
                storage_since_commit = 0;
            }
        }

        batch_storage_count += account_storage;
        accounts.push((address, account));

        // Flush on last account
        if index == total_accounts - 1 && !accounts.is_empty() {
            total_inserted += accounts.len();
            info!(target: "reth::cli",
                batch_accounts = accounts.len(),
                batch_storage_count,
                total_inserted,
                total_accounts,
                "Writing final account batch to database"
            );
            flush_batch_direct(&accounts, &provider_rw, block)?;
            accounts.clear();
        }
    }

    provider_rw.commit()?;

    info!(target: "reth::cli",
        total_inserted,
        "All accounts written, starting state root computation"
    );

    // ── Phase 4: Compute state root (new transaction) ───────────────────
    let provider_rw = provider_factory.database_provider_rw()?;
    let computed_state_root = compute_state_root_from_scratch(&provider_rw)?;
    if computed_state_root != expected_state_root {
        return Err(eyre::eyre!(
            "Computed state root ({:?}) does not match expected ({:?})",
            computed_state_root,
            expected_state_root
        ));
    }

    info!(target: "reth::cli", ?computed_state_root, "State root verified successfully");

    for stage in StageId::STATE_REQUIRED {
        provider_rw.save_stage_checkpoint(stage, StageCheckpoint::new(block))?;
    }
    provider_rw.commit()?;

    info!(target: "reth::cli", hash = ?hash, "Genesis block written");
    Ok(())
}

pub fn download_and_import_init_state(
    chain: &str,
    download_spec: DownloadStateSpec,
    env: EnvironmentArgs<GnosisChainSpecParser>,
) {
    let datadir = env.datadir.clone().resolve_datadir(env.chain.chain());
    let datadir = datadir.data_dir();
    let db_dir = datadir.join("db");

    if datadir.exists() && db_dir.exists() {
        let imported_flag_path = datadir.join(IMPORTED_FLAG);
        let era_imported_flag_path = datadir.join(ERA_IMPORTED_FLAG);
        if imported_flag_path.exists() {
            println!("✅ State is imported, skipping import.");
            return;
        } else if !era_imported_flag_path.exists() {
            println!("❌ State looks misconfigured, please delete the following directory and try again:");
            println!("{datadir:?}");
            std::process::exit(1);
        }
    }

    let state_path_str = format!("./{chain}-state");
    let state_path = Path::new(&state_path_str);

    let runtime = Runtime::new().expect("Unable to build runtime");
    let _guard = runtime.enter();

    if let Err(e) = runtime.block_on(ensure_state(state_path, chain)) {
        eprintln!("state setup failed: {e}");
        std::process::exit(1);
    }

    reth_cli_util::sigsegv_handler::install();

    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
    }

    let compressed_state_file: PathBuf = state_path.join(COMPRESSED_STATE_FILE);
    let header_file: PathBuf = state_path.join("header.rlp");

    import_state(
        &env,
        compressed_state_file,
        header_file.clone(),
        download_spec.header_hash,
    )
    .unwrap();

    let Environment {
        provider_factory, ..
    } = env.init::<GnosisNode>(AccessRights::RO).unwrap();
    let tool = DbTool::new(provider_factory).unwrap();
    let (key, mask): (u64, usize) = (
        table_key::<tables::Headers>(download_spec.block_num).unwrap(),
        5,
    );
    let content = tool
        .provider_factory
        .static_file_provider()
        .find_static_file(StaticFileSegment::Headers, |provider| {
            let mut cursor = provider.cursor()?;
            cursor.get(key.into(), mask).map(|result| {
                result.map(|vec| vec.iter().map(|slice| slice.to_vec()).collect::<Vec<_>>())
            })
        })
        .unwrap();

    match content {
        Some(content) => match StaticFileSegment::Headers {
            StaticFileSegment::Headers => {
                let header = GnosisHeader::decompress(content[0].as_slice()).unwrap();
                let state_root = header.state_root.to_string();
                if state_root != download_spec.expected_state_root {
                    eprintln!(
                        "reth::cli: Header hash mismatch, expected {}, got {}",
                        download_spec.expected_state_root, state_root
                    );
                    std::process::exit(1);
                }
            }
            _ => {
                eprintln!("reth::cli: No content for the given table key.");
            }
        },
        None => {
            eprintln!("reth::cli: No content for the given table key.");
        }
    };

    let imported_flag_path = datadir.join(IMPORTED_FLAG);
    if let Err(e) = std::fs::File::create(imported_flag_path) {
        eprintln!("Failed to create {IMPORTED_FLAG} file: {e}");
        std::process::exit(1);
    }
    println!("✅ State imported successfully.");

    if let Err(e) = std::fs::remove_dir_all(state_path) {
        eprintln!("Failed to delete state directory: {e}");
        std::process::exit(1);
    }
    println!("✅ State directory deleted successfully.");
}
