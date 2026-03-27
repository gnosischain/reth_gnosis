use crate::cli::import_era::ERA_IMPORTED_FLAG;
use crate::initialize::download_init_state::{ensure_state, DownloadStateSpec};
use crate::{spec::gnosis_spec::GnosisChainSpecParser, GnosisNode};
use alloy_genesis::GenesisAccount;
use alloy_primitives::{keccak256, Address, U256};
use alloy_rlp::Decodable;
use gnosis_primitives::header::GnosisHeader;
use reth_cli_commands::common::{AccessRights, Environment, EnvironmentArgs};
use reth_cli_commands::init_state::without_evm;
use reth_codecs::Compact;
use reth_db::table::{Decompress, Table};
use reth_db::tables;
use reth_db_api::cursor::DbCursorRW;
use reth_db_api::cursor::DbDupCursorRW;
use reth_db_api::models::{
    storage_sharded_key::StorageShardedKey, AccountBeforeTx, BlockNumberAddress, IntegerList,
    ShardedKey, StorageSettings,
};
use reth_db_api::transaction::DbTxMut;
use reth_db_common::DbTool;
use reth_etl::Collector;
use reth_primitives_traits::{Account, Bytecode, SealedHeader, StorageEntry};
use reth_provider::{
    BlockHashReader, BlockNumReader, DBProvider, DatabaseProviderFactory, HeaderProvider,
    MetadataWriter, StageCheckpointWriter, StaticFileProviderFactory, StaticFileWriter,
    StorageSettingsCache, TrieWriter,
};
use reth_stages_types::{StageCheckpoint, StageId};
use reth_static_file_types::StaticFileSegment;
use reth_trie::{IntermediateStateRootState, StateRoot as StateRootComputer, StateRootProgress};
use reth_trie_db::{
    DatabaseHashedCursorFactory, DatabaseStateRoot, DatabaseTrieCursorFactory, LegacyKeyAdapter,
};
use revm_primitives::B256;
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tracing::info;

const IMPORTED_FLAG: &str = "imported.flag";

/// Max number of storage "units" (1 per account + 1 per storage slot) before committing.
/// Prevents MDBX dirty page accumulation from blowing up memory.
const STORAGE_COMMIT_THRESHOLD: usize = 500_000;

/// Type to deserialize state root from the first line of the state dump file.
#[derive(Debug, Deserialize)]
struct DumpStateRoot {
    root: B256,
}

/// An account entry from the state dump file.
#[derive(Debug, Deserialize)]
struct GenesisAccountWithAddress {
    #[serde(flatten)]
    genesis_account: GenesisAccount,
    address: Address,
}

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

fn import_state(
    env: &EnvironmentArgs<GnosisChainSpecParser>,
    state: PathBuf,
    header: PathBuf,
    header_hash: &str,
    runtime: reth::tasks::Runtime,
) -> Result<(), eyre::Error> {
    let Environment {
        provider_factory, ..
    } = env.init::<GnosisNode>(AccessRights::RW, runtime)?;

    // Force v1 storage layout for the state import.
    provider_factory.set_storage_settings_cache(StorageSettings::v1());
    {
        let provider_rw = provider_factory.database_provider_rw()?;
        provider_rw.write_storage_settings(StorageSettings::v1())?;
        provider_rw.commit()?;
    }

    let static_file_provider = provider_factory.static_file_provider();
    let header = read_header_from_file(header)?;
    let header_hash = B256::from_str(header_hash)?;
    let block = header.number;

    // Setup header in its own transaction so we can commit and release MDBX dirty pages.
    {
        let provider_rw = provider_factory.database_provider_rw()?;
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
        } else if last_block_number > 0 && last_block_number < block {
            return Err(eyre::eyre!(
                "Data directory should be empty when calling init-state with --without-evm-history."
            ));
        }
        provider_rw.commit()?;
    }

    info!(target: "reth::cli", "Initiating chunked state dump");

    // Verify state root from dump matches header
    let mut reader = BufReader::new(reth_fs_util::open(&state)?);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let dump_state_root = serde_json::from_str::<DumpStateRoot>(&first_line)?.root;

    {
        let provider_rw = provider_factory.database_provider_rw()?;
        let expected_state_root = provider_rw
            .header_by_number(block)?
            .map(|h| h.state_root)
            .ok_or_else(|| eyre::eyre!("Header not found for block {block}"))?;

        if dump_state_root != expected_state_root {
            return Err(eyre::eyre!(
                "State root mismatch: dump has {dump_state_root}, header has {expected_state_root}"
            ));
        }
    }

    // Phase 1: Parse all accounts into an ETL collector which sorts by Address.
    // Uses serde_json streaming to avoid loading multi-GB JSON lines into memory.
    const ETL_BUFFER_SIZE: usize = 100 * 1024 * 1024; // 100 MB
    info!(target: "reth::cli", "Parsing accounts into ETL collector (sorting by address)...");
    let mut collector: Collector<Address, GenesisAccount> = Collector::new(ETL_BUFFER_SIZE, None);
    {
        let mut parsed = 0usize;
        let deserializer = serde_json::Deserializer::from_reader(reader);
        for result in deserializer.into_iter::<GenesisAccountWithAddress>() {
            let entry = result?;
            collector.insert(entry.address, entry.genesis_account)?;
            parsed += 1;
            if parsed % 500_000 == 0 {
                info!(target: "reth::cli", parsed, "Parsing accounts...");
            }
        }
        info!(target: "reth::cli", total = parsed, "Finished parsing accounts");
    }

    // Phase 2: Write accounts directly to DB tables, one at a time.
    //
    // We bypass insert_state/insert_genesis_hashes/insert_history because they build
    // enormous intermediary structures (BundleStateInit, RevertsInit, ExecutionOutcome,
    // BTreeMaps for hashing) that duplicate all storage data 2-3x in memory. For accounts
    // with millions of storage entries, this causes OOM on 15GB machines.
    //
    // Instead, we write directly to each table using cursor operations, processing storage
    // entries one at a time without duplication.
    let accounts_len = collector.len();
    let mut total_accounts: usize = 0;
    let mut storage_units: usize = 0; // track accumulated storage for commit decisions

    let mut provider_rw = provider_factory.database_provider_rw()?;

    for entry in collector.iter()? {
        let (address_raw, account_raw) = entry?;
        let (address, _) = Address::from_compact(address_raw.as_slice(), address_raw.len());
        let (account, _) = GenesisAccount::from_compact(account_raw.as_slice(), account_raw.len());

        let account_storage_len = account.storage.as_ref().map_or(0, |s| s.len());
        let account_units = 1 + account_storage_len;

        // If this single account would push us over the threshold, commit first
        if storage_units > 0 && storage_units + account_units > STORAGE_COMMIT_THRESHOLD {
            provider_rw.commit()?;
            provider_rw = provider_factory.database_provider_rw()?;
            info!(target: "reth::cli", total_accounts, accounts_len, storage_units, "Committed chunk");
            storage_units = 0;
        }

        write_account_to_db(provider_rw.tx_ref(), &address, &account, block)?;

        total_accounts += 1;
        storage_units += account_units;

        if total_accounts % 100_000 == 0 {
            info!(target: "reth::cli", total_accounts, accounts_len, "Writing accounts...");
        }
    }

    // Commit final batch
    provider_rw.commit()?;

    info!(target: "reth::cli",
        total_accounts,
        "All accounts written to database, starting state root computation"
    );

    // Compute state root with periodic commits
    {
        let provider_rw = provider_factory.database_provider_rw()?;
        provider_rw.tx_ref().clear::<tables::AccountsTrie>()?;
        provider_rw.tx_ref().clear::<tables::StoragesTrie>()?;
        provider_rw.commit()?;
    }

    let computed_state_root = compute_state_root_chunked(&provider_factory, dump_state_root)?;

    if computed_state_root != dump_state_root {
        return Err(eyre::eyre!(
            "Computed state root {computed_state_root} does not match expected {dump_state_root}"
        ));
    }

    info!(target: "reth::cli", ?computed_state_root, "State root matches");

    // Save stage checkpoints
    {
        let provider_rw = provider_factory.database_provider_rw()?;
        for stage in StageId::STATE_REQUIRED {
            provider_rw.save_stage_checkpoint(stage, StageCheckpoint::new(block))?;
        }
        provider_rw.commit()?;
    }

    let hash = provider_factory
        .database_provider_rw()?
        .block_hash(block)?
        .ok_or_else(|| eyre::eyre!("Block hash not found for block {block}"))?;
    info!(target: "reth::cli", ?hash, "Genesis block written");
    Ok(())
}

/// Write a single account to all required DB tables directly, without building
/// intermediary structures like ExecutionOutcome that duplicate storage data.
fn write_account_to_db<TX: DbTxMut>(
    tx: &TX,
    address: &Address,
    genesis_account: &GenesisAccount,
    block: u64,
) -> Result<(), eyre::Error> {
    let bytecode_hash = if let Some(code) = &genesis_account.code {
        let bytecode = Bytecode::new_raw_checked(code.clone())
            .map_err(|e| eyre::eyre!("Invalid bytecode for {address}: {e}"))?;
        let hash = bytecode.hash_slow();
        tx.put::<tables::Bytecodes>(hash, bytecode)?;
        Some(hash)
    } else {
        None
    };

    let account = Account {
        nonce: genesis_account.nonce.unwrap_or_default(),
        balance: genesis_account.balance,
        bytecode_hash,
    };

    let hashed_address = keccak256(address);

    // PlainAccountState
    tx.put::<tables::PlainAccountState>(*address, account)?;

    // HashedAccounts
    tx.put::<tables::HashedAccounts>(hashed_address, account)?;

    // AccountChangeSets (genesis: previous was None)
    tx.put::<tables::AccountChangeSets>(
        block,
        AccountBeforeTx {
            address: *address,
            info: None,
        },
    )?;

    // AccountsHistory
    tx.put::<tables::AccountsHistory>(
        ShardedKey::new(*address, u64::MAX),
        IntegerList::new([block])?,
    )?;

    // Storage entries
    if let Some(storage) = &genesis_account.storage {
        let mut hashed_storage_cursor = tx.cursor_dup_write::<tables::HashedStorages>()?;

        for (&key, &value) in storage {
            let value_u256 = U256::from_be_bytes(value.0);

            // PlainStorageState
            tx.put::<tables::PlainStorageState>(
                *address,
                StorageEntry {
                    key,
                    value: value_u256,
                },
            )?;

            // HashedStorages
            let hashed_key = keccak256(key);
            hashed_storage_cursor.upsert(
                hashed_address,
                &StorageEntry {
                    key: hashed_key,
                    value: value_u256,
                },
            )?;

            // StorageChangeSets (genesis: previous was zero)
            tx.put::<tables::StorageChangeSets>(
                BlockNumberAddress((block, *address)),
                StorageEntry {
                    key,
                    value: U256::ZERO,
                },
            )?;

            // StoragesHistory
            tx.put::<tables::StoragesHistory>(
                StorageShardedKey::new(*address, key, u64::MAX),
                IntegerList::new([block])?,
            )?;
        }
    }

    Ok(())
}

/// Compute state root with periodic commits to avoid OOM from trie update dirty pages.
fn compute_state_root_chunked<PF>(
    provider_factory: &PF,
    _expected: B256,
) -> Result<B256, eyre::Error>
where
    PF: DatabaseProviderFactory,
    PF::ProviderRW: DBProvider<Tx: DbTxMut> + TrieWriter + StorageSettingsCache,
{
    type A = LegacyKeyAdapter;
    type DbStateRoot<'a, TX> = StateRootComputer<
        DatabaseTrieCursorFactory<&'a TX, A>,
        DatabaseHashedCursorFactory<&'a TX>,
    >;

    let mut intermediate_state: Option<IntermediateStateRootState> = None;
    let mut total_flushed_updates: usize = 0;

    loop {
        let provider_rw = provider_factory.database_provider_rw()?;
        let tx = provider_rw.tx_ref();

        let state_root =
            DbStateRoot::from_tx(tx).with_intermediate_state(intermediate_state.take());

        match state_root.root_with_progress()? {
            StateRootProgress::Progress(state, _, updates) => {
                let updated_len = provider_rw.write_trie_updates(updates)?;
                total_flushed_updates += updated_len;

                info!(target: "reth::cli",
                    last_account_key = %state.account_root_state.last_hashed_key,
                    updated_len,
                    total_flushed_updates,
                    "Flushing trie updates (committing to free memory)"
                );

                intermediate_state = Some(*state);
                provider_rw.commit()?;
            }
            StateRootProgress::Complete(root, _, updates) => {
                let updated_len = provider_rw.write_trie_updates(updates)?;
                total_flushed_updates += updated_len;

                info!(target: "reth::cli",
                    %root,
                    updated_len,
                    total_flushed_updates,
                    "State root computation complete"
                );

                provider_rw.commit()?;
                return Ok(root);
            }
        }
    }
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
            println!(
                "❌ State looks misconfigured, please delete the following directory and try again:"
            );
            println!("{datadir:?}");
            std::process::exit(1);
        }
    }

    let state_path_str = format!("./{chain}-state");
    let state_path = Path::new(&state_path_str);

    let reth_runtime = reth::tasks::RuntimeBuilder::new(Default::default())
        .build()
        .expect("Unable to build reth runtime");
    let _guard = reth_runtime.handle().enter();

    if let Err(e) = reth_runtime
        .handle()
        .block_on(ensure_state(state_path, chain))
    {
        eprintln!("state setup failed: {e}");
        std::process::exit(1);
    }

    reth_cli_util::sigsegv_handler::install();

    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
    }

    let state_file: PathBuf = state_path.join("state.jsonl");
    let header_file: PathBuf = state_path.join("header.rlp");

    import_state(
        &env,
        state_file,
        header_file.clone(),
        download_spec.header_hash,
        reth_runtime.clone(),
    )
    .unwrap();

    let Environment {
        provider_factory, ..
    } = env
        .init::<GnosisNode>(AccessRights::RO, reth_runtime)
        .unwrap();
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
