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
    models::{storage_sharded_key::StorageShardedKey, AccountBeforeTx, BlockNumberAddress, ShardedKey},
    transaction::DbTxMut,
    BlockNumberList,
};
use reth_db_common::DbTool;
use reth_etl::Collector;
use reth_primitives::{SealedHeader, StaticFileSegment};
use reth_primitives_traits::{Account, Bytecode, StorageEntry};
use reth_provider::{
    BlockHashReader, BlockNumReader, DBProvider, DatabaseProviderFactory,
    HeaderProvider,
    StageCheckpointWriter, StaticFileProviderFactory, StaticFileWriter,
    TrieWriter,
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

/// ETL collector buffer size. 64 MB keeps peak RSS low.
const ETL_BUFFER_BYTES: usize = 64 * 1024 * 1024;

/// Commit the MDBX write transaction after this many storage entries have been
/// written. This releases dirty pages back to the OS, preventing unbounded
/// memory growth from MDBX's copy-on-write page tracking.
const COMMIT_STORAGE_THRESHOLD: usize = 500_000;

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

    // ── Setup header ────────────────────────────────────────────────────
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

    // ── Step 1: Stream zst → ETL collectors (no MDBX writes) ────────────
    //
    // Parse the compressed state dump in a single pass. Every piece of data
    // goes into an ETL collector that sorts it for the target table. Nothing
    // is written to MDBX here, so memory is just the ETL buffers (5×64 MB)
    // plus the JSON parse of the current line.
    info!(target: "reth::cli", "Step 1: Parsing compressed state into ETL collectors");

    let mut account_collector: Collector<Address, Account> =
        Collector::new(ETL_BUFFER_BYTES, None);
    let mut storage_collector: Collector<Address, StorageEntry> =
        Collector::new(ETL_BUFFER_BYTES, None);
    let mut code_collector: Collector<B256, Bytecode> =
        Collector::new(ETL_BUFFER_BYTES, None);
    let mut hashed_account_collector: Collector<B256, Account> =
        Collector::new(ETL_BUFFER_BYTES, None);
    let mut hashed_storage_collector: Collector<B256, StorageEntry> =
        Collector::new(ETL_BUFFER_BYTES, None);

    {
        let file = File::open(&compressed_state)?;
        let decoder = Decoder::new(BufReader::new(file))?;
        let mut reader = BufReader::new(decoder);
        let mut line = String::new();

        // First line is state root
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

        let mut parsed_count: usize = 0;
        let mut total_storage: usize = 0;

        loop {
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                break;
            }
            let entry: GenesisAccountWithAddress = serde_json::from_str(&line)?;
            line.clear();

            let address = entry.address;
            let ga = entry.genesis_account;

            let bytecode_hash = if let Some(code) = &ga.code {
                let bytecode = Bytecode::new_raw_checked(code.clone())
                    .map_err(|e| eyre::eyre!("bad bytecode for {address}: {e}"))?;
                let hash = bytecode.hash_slow();
                code_collector.insert(hash, bytecode)?;
                Some(hash)
            } else {
                None
            };

            let account = Account {
                nonce: ga.nonce.unwrap_or_default(),
                balance: ga.balance,
                bytecode_hash,
            };

            let hashed_addr = keccak256(address);
            account_collector.insert(address, account)?;
            hashed_account_collector.insert(hashed_addr, account)?;

            if let Some(storage) = &ga.storage {
                for (&key, &value) in storage {
                    let value = U256::from_be_bytes(value.0);
                    if !value.is_zero() {
                        storage_collector
                            .insert(address, StorageEntry { key, value })?;
                        hashed_storage_collector.insert(
                            hashed_addr,
                            StorageEntry {
                                key: keccak256(key),
                                value,
                            },
                        )?;
                        total_storage += 1;
                    }
                }
            }

            parsed_count += 1;
            if parsed_count % 500_000 == 0 {
                info!(target: "reth::cli", parsed_count, total_storage, "Parsing progress");
            }
        }

        info!(target: "reth::cli", parsed_count, total_storage, "Step 1 complete");
    }

    // ── Step 2: Write all tables from sorted ETL collectors ─────────────
    //
    // Each collector produces sorted output. We use append/append_dup for
    // sequential page fills (no random B-tree access). For DupSort tables
    // we buffer entries per primary key, sort sub-keys, then append_dup.

    // Clear tables that setup_without_evm may have pre-populated,
    // so our sorted append/append_dup calls don't conflict.
    {
        let provider_rw = provider_factory.database_provider_rw()?;
        let tx = provider_rw.tx_ref();
        tx.clear::<tables::PlainAccountState>()?;
        tx.clear::<tables::PlainStorageState>()?;
        tx.clear::<tables::Bytecodes>()?;
        tx.clear::<tables::AccountChangeSets>()?;
        tx.clear::<tables::StorageChangeSets>()?;
        tx.clear::<tables::AccountsHistory>()?;
        tx.clear::<tables::StoragesHistory>()?;
        tx.clear::<tables::HashedAccounts>()?;
        tx.clear::<tables::HashedStorages>()?;
        tx.clear::<tables::AccountsTrie>()?;
        tx.clear::<tables::StoragesTrie>()?;
        provider_rw.commit()?;
    }

    // 2a: Bytecodes (deduplicated, sorted by code hash)
    info!(target: "reth::cli", "Step 2a: Writing bytecodes");
    {
        let provider_rw = provider_factory.database_provider_rw()?;
        let tx = provider_rw.tx_ref();
        let mut cursor = tx.cursor_write::<tables::Bytecodes>()?;
        let mut prev_hash: Option<B256> = None;
        for entry in code_collector.iter()? {
            let (raw_key, raw_value) = entry?;
            let (hash, _) = B256::from_compact(raw_key.as_slice(), raw_key.len());
            if prev_hash.as_ref() == Some(&hash) {
                continue; // skip duplicate code hashes
            }
            let (bytecode, _) = Bytecode::from_compact(raw_value.as_slice(), raw_value.len());
            cursor.append(hash, &bytecode)?;
            prev_hash = Some(hash);
        }
        drop(cursor);
        provider_rw.commit()?;
    }
    drop(code_collector);

    // 2b: Accounts → PlainAccountState + AccountChangeSets + AccountsHistory
    info!(target: "reth::cli",
        total = account_collector.len(),
        "Step 2b: Writing accounts + changesets + history"
    );
    {
        let list = BlockNumberList::new([block]).expect("single block always fits");
        let provider_rw = provider_factory.database_provider_rw()?;
        let tx = provider_rw.tx_ref();
        let mut acct_cursor = tx.cursor_write::<tables::PlainAccountState>()?;
        let mut cs_cursor = tx.cursor_dup_write::<tables::AccountChangeSets>()?;
        let mut hist_cursor = tx.cursor_write::<tables::AccountsHistory>()?;

        for entry in account_collector.iter()? {
            let (raw_key, raw_value) = entry?;
            let (address, _) = Address::from_compact(raw_key.as_slice(), raw_key.len());
            let (account, _) = Account::from_compact(raw_value.as_slice(), raw_value.len());

            acct_cursor.append(address, &account)?;
            cs_cursor.append_dup(
                block,
                AccountBeforeTx {
                    address,
                    info: None,
                },
            )?;
            hist_cursor.append(ShardedKey::last(address), &list)?;
        }

        drop(acct_cursor);
        drop(cs_cursor);
        drop(hist_cursor);
        provider_rw.commit()?;
    }
    drop(account_collector);

    // 2c: Storage → PlainStorageState + StorageChangeSets + StoragesHistory
    //
    // The collector is sorted by address. We buffer entries per address,
    // sort sub-keys, then write all three tables together with append_dup.
    info!(target: "reth::cli",
        total = storage_collector.len(),
        "Step 2c: Writing storage + changesets + history"
    );
    {
        let list = BlockNumberList::new([block]).expect("single block always fits");
        let mut provider_rw = provider_factory.database_provider_rw()?;
        let mut entries_since_commit: usize = 0;
        let mut total_written: usize = 0;
        let mut current_addr: Option<Address> = None;
        let mut sub_entries: Vec<StorageEntry> = Vec::new();

        for entry in storage_collector.iter()? {
            let (raw_key, raw_value) = entry?;
            let (address, _) = Address::from_compact(raw_key.as_slice(), raw_key.len());
            let (storage_entry, _) =
                StorageEntry::from_compact(raw_value.as_slice(), raw_value.len());

            if current_addr.as_ref() != Some(&address) {
                if let Some(addr) = current_addr.take() {
                    sub_entries.sort_unstable_by_key(|e| e.key);
                    {
                        let tx = provider_rw.tx_ref();
                        let mut plain_cursor =
                            tx.cursor_dup_write::<tables::PlainStorageState>()?;
                        let mut cs_cursor =
                            tx.cursor_dup_write::<tables::StorageChangeSets>()?;
                        let mut hist_cursor =
                            tx.cursor_write::<tables::StoragesHistory>()?;
                        let block_addr = BlockNumberAddress((block, addr));

                        for se in &sub_entries {
                            plain_cursor.append_dup(addr, se.clone())?;
                            cs_cursor.append_dup(
                                block_addr,
                                StorageEntry {
                                    key: se.key,
                                    value: U256::ZERO,
                                },
                            )?;
                            hist_cursor.append(
                                StorageShardedKey::last(addr, se.key),
                                &list,
                            )?;
                        }
                    }
                    entries_since_commit += sub_entries.len();
                    total_written += sub_entries.len();
                    sub_entries = Vec::new();

                    if entries_since_commit >= COMMIT_STORAGE_THRESHOLD {
                        info!(target: "reth::cli",
                            entries_since_commit,
                            total_written,
                            "Committing storage tables"
                        );
                        provider_rw.commit()?;
                        provider_rw = provider_factory.database_provider_rw()?;
                        entries_since_commit = 0;
                    }
                }
                current_addr = Some(address);
            }
            sub_entries.push(storage_entry);
        }

        // Flush last address group
        if let Some(addr) = current_addr.take() {
            sub_entries.sort_unstable_by_key(|e| e.key);
            {
                let tx = provider_rw.tx_ref();
                let mut plain_cursor =
                    tx.cursor_dup_write::<tables::PlainStorageState>()?;
                let mut cs_cursor =
                    tx.cursor_dup_write::<tables::StorageChangeSets>()?;
                let mut hist_cursor =
                    tx.cursor_write::<tables::StoragesHistory>()?;
                let block_addr = BlockNumberAddress((block, addr));

                for se in &sub_entries {
                    plain_cursor.append_dup(addr, se.clone())?;
                    cs_cursor.append_dup(
                        block_addr,
                        StorageEntry {
                            key: se.key,
                            value: U256::ZERO,
                        },
                    )?;
                    hist_cursor.upsert(
                        StorageShardedKey::last(addr, se.key),
                        &list,
                    )?;
                }
            }
            total_written += sub_entries.len();
        }
        provider_rw.commit()?;

        info!(target: "reth::cli", total_written, "Step 2c complete");
    }
    drop(storage_collector);

    // 2d: HashedAccounts (sorted by keccak256(address))
    info!(target: "reth::cli",
        total = hashed_account_collector.len(),
        "Step 2d: Writing hashed accounts"
    );
    {
        let provider_rw = provider_factory.database_provider_rw()?;
        let tx = provider_rw.tx_ref();
        let mut cursor = tx.cursor_write::<tables::HashedAccounts>()?;
        for entry in hashed_account_collector.iter()? {
            let (raw_key, raw_value) = entry?;
            let (hashed_addr, _) = B256::from_compact(raw_key.as_slice(), raw_key.len());
            let (account, _) = Account::from_compact(raw_value.as_slice(), raw_value.len());
            cursor.append(hashed_addr, &account)?;
        }
        drop(cursor);
        provider_rw.commit()?;
    }
    drop(hashed_account_collector);

    // 2e: HashedStorages (sorted by keccak256(address), sub-keys sorted per group)
    info!(target: "reth::cli",
        total = hashed_storage_collector.len(),
        "Step 2e: Writing hashed storage"
    );
    {
        let mut provider_rw = provider_factory.database_provider_rw()?;
        let mut entries_since_commit: usize = 0;
        let mut total_written: usize = 0;
        let mut current_addr: Option<B256> = None;
        let mut sub_entries: Vec<StorageEntry> = Vec::new();

        for entry in hashed_storage_collector.iter()? {
            let (raw_key, raw_value) = entry?;
            let (hashed_addr, _) = B256::from_compact(raw_key.as_slice(), raw_key.len());
            let (storage_entry, _) =
                StorageEntry::from_compact(raw_value.as_slice(), raw_value.len());

            if current_addr.as_ref() != Some(&hashed_addr) {
                if let Some(addr) = current_addr.take() {
                    sub_entries.sort_unstable_by_key(|e| e.key);
                    {
                        let mut cursor = provider_rw
                            .tx_ref()
                            .cursor_dup_write::<tables::HashedStorages>()?;
                        for se in &sub_entries {
                            cursor.append_dup(addr, se.clone())?;
                        }
                    }
                    entries_since_commit += sub_entries.len();
                    total_written += sub_entries.len();
                    sub_entries = Vec::new();

                    if entries_since_commit >= COMMIT_STORAGE_THRESHOLD {
                        info!(target: "reth::cli",
                            entries_since_commit,
                            total_written,
                            "Committing hashed storage"
                        );
                        provider_rw.commit()?;
                        provider_rw = provider_factory.database_provider_rw()?;
                        entries_since_commit = 0;
                    }
                }
                current_addr = Some(hashed_addr);
            }
            sub_entries.push(storage_entry);
        }

        // Flush last address group
        if let Some(addr) = current_addr.take() {
            sub_entries.sort_unstable_by_key(|e| e.key);
            {
                let mut cursor = provider_rw
                    .tx_ref()
                    .cursor_dup_write::<tables::HashedStorages>()?;
                for se in &sub_entries {
                    cursor.append_dup(addr, se.clone())?;
                }
            }
            total_written += sub_entries.len();
        }
        provider_rw.commit()?;

        info!(target: "reth::cli", total_written, "Step 2e complete");
    }
    drop(hashed_storage_collector);

    // ── Step 3: Compute state root ──────────────────────────────────────
    info!(target: "reth::cli", "Step 3: Computing state root");
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
