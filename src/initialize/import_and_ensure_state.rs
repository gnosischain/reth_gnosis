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
    cursor::{DbCursorRO, DbCursorRW, DbDupCursorRW},
    models::{storage_sharded_key::StorageShardedKey, AccountBeforeTx, BlockNumberAddress, ShardedKey},
    transaction::{DbTx, DbTxMut},
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

    // Verify state root from dump
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

    // ── Phase 2: Parse JSONL → write plain tables + populate ETL collectors ─
    //
    // Stream from the compressed state dump. For each account:
    //   1. Upsert to PlainAccountState, PlainStorageState, Bytecodes
    //   2. Insert hashed entries into ETL collectors (for Phases 4 & 5)
    //
    // By populating the hashed ETL collectors HERE we avoid re-reading
    // the entire PlainStorageState table later, which would pull ~7 GB
    // of mmap pages back into memory.
    info!(target: "reth::cli", "Phase 2: Writing plain state from compressed dump");

    let mut hashed_account_collector: Collector<B256, Account> =
        Collector::new(ETL_BUFFER_BYTES, None);
    let mut hashed_storage_collector: Collector<B256, StorageEntry> =
        Collector::new(ETL_BUFFER_BYTES, None);

    let mut provider_rw = provider_factory.database_provider_rw()?;
    let mut storage_since_commit: usize = 0;
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

        // Write account + storage in a single cursor scope to avoid
        // creating 100M+ cursors (one per storage entry).
        let account = {
            let tx = provider_rw.tx_ref();

            // PlainAccountState + Bytecodes
            let bytecode_hash = if let Some(code) = &ga.code {
                let bytecode = Bytecode::new_raw_checked(code.clone())
                    .map_err(|e| eyre::eyre!("bad bytecode for {address}: {e}"))?;
                let hash = bytecode.hash_slow();
                tx.cursor_write::<tables::Bytecodes>()?
                    .upsert(hash, &bytecode)?;
                Some(hash)
            } else {
                None
            };
            let account = Account {
                nonce: ga.nonce.unwrap_or_default(),
                balance: ga.balance,
                bytecode_hash,
            };
            tx.cursor_write::<tables::PlainAccountState>()?
                .upsert(address, &account)?;

            // PlainStorageState - one cursor for all entries of this account
            if let Some(storage) = &ga.storage {
                let mut cursor =
                    tx.cursor_dup_write::<tables::PlainStorageState>()?;
                for (&key, &value) in storage {
                    let value = U256::from_be_bytes(value.0);
                    if !value.is_zero() {
                        cursor.upsert(address, &StorageEntry { key, value })?;
                        storage_since_commit += 1;
                        total_storage += 1;
                    }
                }
            }

            account
        };

        // Populate hashed ETL collectors (spill to disk at 64 MB)
        let hashed_addr = keccak256(address);
        hashed_account_collector.insert(hashed_addr, account)?;
        if let Some(storage) = &ga.storage {
            for (&key, &value) in storage {
                let value = U256::from_be_bytes(value.0);
                if !value.is_zero() {
                    hashed_storage_collector.insert(
                        hashed_addr,
                        StorageEntry {
                            key: keccak256(key),
                            value,
                        },
                    )?;
                }
            }
        }

        parsed_count += 1;
        if parsed_count % 500_000 == 0 {
            info!(target: "reth::cli",
                parsed_count,
                total_storage,
                storage_since_commit,
                "Phase 2 progress"
            );
        }

        // Commit periodically to release MDBX dirty pages
        if storage_since_commit >= COMMIT_STORAGE_THRESHOLD {
            info!(target: "reth::cli",
                storage_since_commit,
                parsed_count,
                "Committing to release memory"
            );
            provider_rw.commit()?;
            provider_rw = provider_factory.database_provider_rw()?;
            storage_since_commit = 0;
        }
    }
    provider_rw.commit()?;
    drop(reader);

    info!(target: "reth::cli",
        parsed_count,
        total_storage,
        "Phase 2 complete: plain state written"
    );

    // ── Phase 3: Derive changesets from plain tables ────────────────────
    //
    // PlainAccountState is sorted by address in MDBX, so we can use
    // append_dup for AccountChangeSets. StorageChangeSets are split across
    // multiple commits to avoid unbounded dirty page accumulation.
    info!(target: "reth::cli", "Phase 3: Writing changesets");

    // AccountChangeSets (small, single transaction)
    {
        let provider_rw = provider_factory.database_provider_rw()?;
        let tx = provider_rw.tx_ref();
        let mut read_cursor = tx.cursor_read::<tables::PlainAccountState>()?;
        let mut cs_cursor = tx.cursor_dup_write::<tables::AccountChangeSets>()?;
        let mut entry = read_cursor.first()?;
        while let Some((address, _)) = entry {
            cs_cursor.append_dup(
                block,
                AccountBeforeTx {
                    address,
                    info: None,
                },
            )?;
            entry = read_cursor.next()?;
        }
        drop(read_cursor);
        drop(cs_cursor);
        provider_rw.commit()?;
    }

    // StorageChangeSets (large, periodic commits)
    // append_dup works across commits because we iterate PlainStorageState
    // in (address, key) order, so new entries are always > committed ones.
    {
        let mut provider_rw = provider_factory.database_provider_rw()?;
        let mut entries_since_commit: usize = 0;
        let mut total_cs: usize = 0;
        let mut resume_from: Option<(Address, B256)> = None;

        loop {
            let tx = provider_rw.tx_ref();
            let mut read_cursor = tx.cursor_read::<tables::PlainStorageState>()?;
            let mut cs_cursor =
                tx.cursor_dup_write::<tables::StorageChangeSets>()?;

            // Position cursor at resume point
            let mut entry_opt = if let Some((addr, last_key)) = resume_from {
                let first = read_cursor.seek(addr)?;
                // Skip entries we already processed
                let mut e = first;
                while let Some((a, se)) = e {
                    if a != addr || se.key > last_key {
                        break;
                    }
                    e = read_cursor.next()?;
                }
                e
            } else {
                read_cursor.first()?
            };

            let mut need_commit = false;
            while let Some((address, se)) = entry_opt {
                cs_cursor.append_dup(
                    BlockNumberAddress((block, address)),
                    StorageEntry {
                        key: se.key,
                        value: U256::ZERO,
                    },
                )?;
                entries_since_commit += 1;
                total_cs += 1;

                if entries_since_commit >= COMMIT_STORAGE_THRESHOLD {
                    resume_from = Some((address, se.key));
                    need_commit = true;
                    break;
                }
                entry_opt = read_cursor.next()?;
            }

            let done = !need_commit && entry_opt.is_none();

            drop(read_cursor);
            drop(cs_cursor);

            if entries_since_commit > 0 {
                info!(target: "reth::cli",
                    entries_since_commit,
                    total_cs,
                    "Committing StorageChangeSets"
                );
            }
            provider_rw.commit()?;

            if done {
                break;
            }
            provider_rw = provider_factory.database_provider_rw()?;
            entries_since_commit = 0;
        }
    }
    info!(target: "reth::cli", "Phase 3 complete: changesets written");

    // ── Phase 4: Write HashedAccounts from ETL collector ────────────────
    //
    // The collector was populated during Phase 2 (no MDBX re-read needed).
    info!(target: "reth::cli",
        total = hashed_account_collector.len(),
        "Phase 4: Writing hashed accounts"
    );
    {
        let provider_rw = provider_factory.database_provider_rw()?;
        let tx = provider_rw.tx_ref();
        let mut cursor = tx.cursor_write::<tables::HashedAccounts>()?;
        for entry in hashed_account_collector.iter()? {
            let (raw_key, raw_value) = entry?;
            let (hashed_addr, _) = B256::from_compact(raw_key.as_slice(), raw_key.len());
            let (account, _) = Account::from_compact(raw_value.as_slice(), raw_value.len());
            cursor.upsert(hashed_addr, &account)?;
        }
        drop(cursor);
        provider_rw.commit()?;
    }
    drop(hashed_account_collector);
    info!(target: "reth::cli", "Phase 4 complete: hashed accounts written");

    // ── Phase 5: Write HashedStorages from ETL collector ─────────────────
    //
    // The collector was populated during Phase 2 (no MDBX re-read needed).
    // ETL collector output is sorted by hashed address. We buffer entries
    // per address, sort sub-keys, then use append_dup for sequential page
    // writes. This avoids random B-tree access that keeps the entire table
    // in mmap (the cause of 22+ GB RSS with upsert).
    info!(target: "reth::cli",
        total = hashed_storage_collector.len(),
        "Phase 5: Writing hashed storage"
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

            // When the address changes, flush the buffered sub-entries
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
                    // Release memory from large groups (e.g. 18M-entry contracts)
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
            drop(sub_entries);
        }
        provider_rw.commit()?;

        info!(target: "reth::cli",
            total_written,
            "Phase 5 complete: hashed storage written"
        );
    }
    drop(hashed_storage_collector);

    // ── Phase 6: History indices ────────────────────────────────────────
    //
    // Write AccountsHistory and StoragesHistory by iterating the plain
    // tables. Each account/storage entry gets a single-block history entry.
    info!(target: "reth::cli", "Phase 6: Writing history indices");
    {
        let list = BlockNumberList::new([block]).expect("single block always fits");

        // Account history (small, single transaction)
        {
            let provider_rw = provider_factory.database_provider_rw()?;
            let tx = provider_rw.tx_ref();
            let mut read_cursor = tx.cursor_read::<tables::PlainAccountState>()?;
            let mut hist_cursor = tx.cursor_write::<tables::AccountsHistory>()?;
            let mut entry = read_cursor.first()?;
            while let Some((address, _)) = entry {
                hist_cursor.upsert(ShardedKey::last(address), &list)?;
                entry = read_cursor.next()?;
            }
            drop(read_cursor);
            drop(hist_cursor);
            provider_rw.commit()?;
        }

        info!(target: "reth::cli", "Account history written");

        // Storage history (large, periodic commits)
        {
            let mut provider_rw = provider_factory.database_provider_rw()?;
            let mut entries_since_commit: usize = 0;
            let mut total_written: usize = 0;
            // Track resume position for after commits
            let mut resume_addr: Option<(Address, B256)> = None;

            loop {
                let tx = provider_rw.tx_ref();
                let mut read_cursor =
                    tx.cursor_read::<tables::PlainStorageState>()?;
                let mut hist_cursor =
                    tx.cursor_write::<tables::StoragesHistory>()?;

                // Position cursor
                let first = if let Some((addr, _last_key)) = resume_addr {
                    read_cursor.seek(addr)?
                } else {
                    read_cursor.first()?
                };

                // Skip already-processed entries after resume
                let mut entry_opt = first;
                if let Some((_addr, last_key)) = resume_addr {
                    while let Some((a, se)) = entry_opt {
                        if a != _addr || se.key > last_key {
                            break;
                        }
                        entry_opt = read_cursor.next()?;
                    }
                }

                let mut done = false;
                while let Some((addr, se)) = entry_opt {
                    hist_cursor.upsert(
                        StorageShardedKey::last(addr, se.key),
                        &list,
                    )?;
                    entries_since_commit += 1;
                    total_written += 1;

                    if entries_since_commit >= COMMIT_STORAGE_THRESHOLD {
                        resume_addr = Some((addr, se.key));
                        break;
                    }
                    entry_opt = read_cursor.next()?;
                }

                if entry_opt.is_none() && entries_since_commit < COMMIT_STORAGE_THRESHOLD {
                    done = true;
                }

                drop(read_cursor);
                drop(hist_cursor);

                if total_written > 0 {
                    info!(target: "reth::cli",
                        entries_since_commit,
                        total_written,
                        "Committing storage history"
                    );
                }

                provider_rw.commit()?;

                if done {
                    break;
                }
                provider_rw = provider_factory.database_provider_rw()?;
                entries_since_commit = 0;
            }

            info!(target: "reth::cli",
                total_written,
                "Phase 6 complete: history indices written"
            );
        }
    }

    // ── Phase 7: Compute state root (new transaction) ───────────────────
    info!(target: "reth::cli", "Phase 7: Computing state root");
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
