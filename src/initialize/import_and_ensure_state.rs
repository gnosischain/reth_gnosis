use crate::cli::import_era::ERA_IMPORTED_FLAG;
use crate::initialize::download_init_state::{
    ensure_state, DownloadStateSpec, COMPRESSED_STATE_FILE, HEADER_FILE,
};
use crate::{spec::gnosis_spec::GnosisChainSpecParser, GnosisNode};
use alloy_consensus::BlockHeader;
use alloy_genesis::GenesisAccount;
use alloy_primitives::{Address, B256};
use alloy_rlp::Decodable;
use gnosis_primitives::header::GnosisHeader;
use reth_cli_commands::common::{AccessRights, Environment, EnvironmentArgs};
use reth_cli_commands::init_state::without_evm;
use reth_codecs::Compact;
use reth_config::config::EtlConfig;
use reth_db::table::{Decompress, Table};
use reth_db::tables;
use reth_db_common::init::{
    insert_genesis_hashes, insert_history, insert_state, AVERAGE_COUNT_ACCOUNTS_PER_GB_STATE_DUMP,
};
use reth_db_common::DbTool;
use reth_etl::Collector;
use reth_primitives::{SealedHeader, StaticFileSegment};
use reth_provider::{
    BlockHashReader, BlockNumReader, ChainSpecProvider, DBProvider, DatabaseProviderFactory,
    HashingWriter, HeaderProvider, HistoryWriter, NodePrimitivesProvider, RocksDBProviderFactory,
    StageCheckpointWriter, StateWriter, StaticFileProviderFactory, StaticFileWriter,
    StorageSettingsCache, TrieWriter,
};
use reth_stages_types::{StageCheckpoint, StageId};
use reth_trie::{
    prefix_set::{PrefixSetMut, TriePrefixSets},
    IntermediateStateRootState, StateRoot as StateRootComputer, StateRootProgress,
};
use reth_trie_db::DatabaseStateRoot;
use revm_primitives::B256 as RevmB256;
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tokio::runtime::Runtime;
use tracing::{error, info, trace};

const IMPORTED_FLAG: &str = "imported.flag";

/// ETL buffer size for state import: 128 MB (vs upstream default of 500 MB)
const IMPORT_ETL_BUFFER_SIZE: usize = 128 * 1024 * 1024;

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

// ── Deserialization types matching upstream JSONL format ─────

/// State root line: `{"root":"0x..."}`
#[derive(Debug, Deserialize)]
struct StateRoot {
    root: B256,
}

/// Account line: `{"address":"0x...","balance":"0x...","nonce":"0x...", ...}`
#[derive(Debug, Deserialize)]
struct GenesisAccountWithAddress {
    #[serde(flatten)]
    genesis_account: GenesisAccount,
    address: Address,
}

// ── Low-memory state import ─────────────────────────────────

/// Parses and returns the expected state root from the first line of the JSONL.
fn parse_state_root(reader: &mut impl BufRead) -> eyre::Result<B256> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let root = serde_json::from_str::<StateRoot>(&line)?.root;
    trace!(target: "reth::cli", %root, "Read state root from file");
    Ok(root)
}

/// Parses accounts from the reader into an ETL collector (spills to disk at `buffer_cap` bytes).
fn parse_accounts(
    mut reader: impl BufRead,
    etl_config: EtlConfig,
) -> eyre::Result<Collector<Address, GenesisAccount>> {
    let mut line = String::new();
    let mut collector = Collector::new(etl_config.file_size, etl_config.dir);

    loop {
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }

        let GenesisAccountWithAddress {
            genesis_account,
            address,
        } = serde_json::from_str(&line)?;
        collector.insert(address, genesis_account)?;

        if !collector.is_empty()
            && collector
                .len()
                .is_multiple_of(AVERAGE_COUNT_ACCOUNTS_PER_GB_STATE_DUMP)
        {
            info!(target: "reth::cli", parsed_new_accounts = collector.len());
        }

        line.clear();
    }

    Ok(collector)
}

/// Writes accounts from the collector into the database.
///
/// Unlike upstream `dump_state`, this does **not** accumulate individual `Nibbles`
/// into `TriePrefixSetsMut`. Instead it uses `PrefixSetMut::all()` which sets
/// a single boolean flag meaning "every key changed" — achieving the same trie
/// traversal behaviour with near-zero memory overhead.
fn dump_state<Provider>(
    mut collector: Collector<Address, GenesisAccount>,
    provider_rw: &Provider,
    block: u64,
) -> Result<TriePrefixSets, eyre::Error>
where
    Provider: StaticFileProviderFactory
        + DBProvider<Tx: reth_db_api::transaction::DbTxMut>
        + HeaderProvider
        + HashingWriter
        + HistoryWriter
        + StateWriter
        + StorageSettingsCache
        + RocksDBProviderFactory
        + NodePrimitivesProvider
        + AsRef<Provider>,
{
    let accounts_len = collector.len();
    let mut accounts = Vec::with_capacity(AVERAGE_COUNT_ACCOUNTS_PER_GB_STATE_DUMP);
    let mut total_inserted_accounts = 0;

    for (index, entry) in collector.iter()?.enumerate() {
        let (address, account) = entry?;
        let (address, _) = Address::from_compact(address.as_slice(), address.len());
        let (account, _) = GenesisAccount::from_compact(account.as_slice(), account.len());

        accounts.push((address, account));

        if (index > 0 && index.is_multiple_of(AVERAGE_COUNT_ACCOUNTS_PER_GB_STATE_DUMP))
            || index == accounts_len - 1
        {
            total_inserted_accounts += accounts.len();

            info!(target: "reth::cli",
                total_inserted_accounts,
                "Writing accounts to db"
            );

            insert_genesis_hashes(
                provider_rw,
                accounts.iter().map(|(address, account)| (address, account)),
            )?;

            insert_history(
                provider_rw,
                accounts.iter().map(|(address, account)| (address, account)),
                block,
            )?;

            insert_state(
                provider_rw,
                accounts.iter().map(|(address, account)| (address, account)),
                block,
            )?;

            accounts.clear();
        }
    }

    // Build lightweight prefix sets: just the `all` flag, no per-key nibbles.
    let prefix_sets = TriePrefixSets {
        account_prefix_set: PrefixSetMut::all().freeze(),
        storage_prefix_sets: Default::default(),
        destroyed_accounts: Default::default(),
    };

    Ok(prefix_sets)
}

/// Computes the state root from the database and writes trie nodes.
fn compute_state_root<Provider>(
    provider: &Provider,
    prefix_sets: TriePrefixSets,
) -> Result<B256, eyre::Error>
where
    Provider: DBProvider<Tx: reth_db_api::transaction::DbTxMut> + TrieWriter,
{
    trace!(target: "reth::cli", "Computing state root");

    let tx = provider.tx_ref();
    let mut intermediate_state: Option<IntermediateStateRootState> = None;
    let mut total_flushed_updates: usize = 0;

    loop {
        let state_root = StateRootComputer::from_tx(tx)
            .with_prefix_sets(prefix_sets.clone())
            .with_intermediate_state(intermediate_state);

        match state_root.root_with_progress()? {
            StateRootProgress::Progress(state, _, updates) => {
                let updated_len = provider.write_trie_updates(updates)?;
                total_flushed_updates += updated_len;

                trace!(target: "reth::cli",
                    last_account_key = %state.account_root_state.last_hashed_key,
                    updated_len,
                    total_flushed_updates,
                    "Flushing trie updates"
                );

                intermediate_state = Some(*state);

                if total_flushed_updates.is_multiple_of(1_000_000) {
                    info!(target: "reth::cli",
                        total_flushed_updates,
                        "Flushing trie updates"
                    );
                }
            }
            StateRootProgress::Complete(root, _, updates) => {
                let updated_len = provider.write_trie_updates(updates)?;
                total_flushed_updates += updated_len;

                trace!(target: "reth::cli",
                    %root,
                    updated_len,
                    total_flushed_updates,
                    "State root has been computed"
                );

                return Ok(root);
            }
        }
    }
}

/// Low-memory replacement for upstream `init_from_state_dump`.
///
/// Key differences:
///   1. Uses `PrefixSetMut::all()` instead of accumulating per-key `Nibbles` — saves 5-15 GB.
///   2. Uses a smaller ETL buffer (128 MB vs 500 MB default).
///   3. Caller provides a streaming reader (e.g. zstd decoder) so no 27 GB intermediate file.
fn init_from_state_dump_low_mem<Provider>(
    mut reader: impl BufRead,
    provider_rw: &Provider,
    etl_config: EtlConfig,
) -> eyre::Result<B256>
where
    Provider: StaticFileProviderFactory
        + DBProvider<Tx: reth_db_api::transaction::DbTxMut>
        + BlockNumReader
        + BlockHashReader
        + ChainSpecProvider
        + StageCheckpointWriter
        + HistoryWriter
        + HeaderProvider
        + HashingWriter
        + TrieWriter
        + StateWriter
        + StorageSettingsCache
        + RocksDBProviderFactory
        + NodePrimitivesProvider
        + AsRef<Provider>,
{
    let block = provider_rw.last_block_number()?;
    let hash = provider_rw
        .block_hash(block)?
        .ok_or_else(|| eyre::eyre!("Block hash not found for block {block}"))?;
    let header = provider_rw
        .header_by_number(block)?
        .map(SealedHeader::seal_slow)
        .ok_or_else(|| eyre::eyre!("Header not found for block {block}"))?;

    let expected_state_root = header.state_root();

    // First line is the state root – verify it matches the header.
    let dump_state_root = parse_state_root(&mut reader)?;
    if expected_state_root != dump_state_root {
        error!(target: "reth::cli",
            ?dump_state_root,
            ?expected_state_root,
            "State root from dump does not match header"
        );
        return Err(eyre::eyre!(
            "State root mismatch: dump={dump_state_root}, header={expected_state_root}"
        ));
    }

    info!(target: "reth::cli", block, "Initializing state at block");

    // Parse accounts into ETL collector (streams from reader, spills to disk).
    let collector = parse_accounts(&mut reader, etl_config)?;

    // Write state to DB — returns lightweight prefix sets (PrefixSetMut::all()).
    let prefix_sets = dump_state(collector, provider_rw, block)?;

    info!(target: "reth::cli", "All accounts written to database, starting state root computation");

    // Compute and verify state root.
    let computed_state_root = compute_state_root(provider_rw, prefix_sets)?;
    if computed_state_root == expected_state_root {
        info!(target: "reth::cli", ?computed_state_root, "Computed state root matches");
    } else {
        error!(target: "reth::cli",
            ?computed_state_root,
            ?expected_state_root,
            "Computed state root does NOT match"
        );
        return Err(eyre::eyre!(
            "State root mismatch: computed={computed_state_root}, expected={expected_state_root}"
        ));
    }

    // Mark sync stages that require state as complete at this block.
    for stage in StageId::STATE_REQUIRED {
        provider_rw.save_stage_checkpoint(stage, StageCheckpoint::new(block))?;
    }

    Ok(hash)
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

    let static_file_provider = provider_factory.static_file_provider();
    let provider_rw = provider_factory.database_provider_rw()?;

    // ensure header, total difficulty and header hash are provided
    let header = read_header_from_file(header)?;
    let header_hash = RevmB256::from_str(header_hash)?;

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

        // SAFETY: it's safe to commit static files, since in the event of a crash, they
        // will be unwound according to database checkpoints.
        //
        // Necessary to commit, so the header is accessible to provider_rw and
        // init_state_dump
        static_file_provider.commit()?;
    } else if last_block_number > 0 && last_block_number < header.number {
        return Err(eyre::eyre!(
            "Data directory should be empty when calling init-state with --without-evm-history."
        ));
    }

    info!(target: "reth::cli", "Initiating state dump (streaming from compressed file)");

    // Stream decompress directly from the .zst file — no intermediate 27 GB file.
    let zstd_file = File::open(&compressed_state)?;
    let decoder = zstd::Decoder::new(BufReader::new(zstd_file))?;
    let reader = BufReader::new(decoder);

    let etl_config = EtlConfig {
        file_size: IMPORT_ETL_BUFFER_SIZE,
        dir: None,
    };

    let hash = init_from_state_dump_low_mem(reader, &provider_rw, etl_config)?;

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
        // DB is initialized, check if the state is imported
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

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
    }

    // Import directly from compressed file (no decompression step).
    let compressed_file: PathBuf = state_path.join(COMPRESSED_STATE_FILE);
    let header_file: PathBuf = state_path.join(HEADER_FILE);

    import_state(
        &env,
        compressed_file,
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

    // create the IMPORTED_FLAG file
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
