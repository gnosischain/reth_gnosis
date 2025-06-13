use crate::initialize::download_init_state::{ensure_state, DownloadStateSpec};
use crate::primitives::header::GnosisHeader;
use crate::{spec::gnosis_spec::GnosisChainSpecParser, GnosisNode};
use alloy_consensus::Header;
use alloy_rlp::Decodable;
use reth::tokio_runtime;
use reth_cli_commands::common::{AccessRights, Environment, EnvironmentArgs};
use reth_cli_commands::init_state::without_evm;
use reth_db::table::{Decompress, Table};
use reth_db::tables;
use reth_db_common::init::init_from_state_dump;
use reth_db_common::DbTool;
use reth_primitives::{SealedHeader, StaticFileSegment};
use reth_provider::{
    BlockNumReader, DatabaseProviderFactory, StaticFileProviderFactory, StaticFileWriter,
};
use revm_primitives::{B256, U256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tracing::info;

const IMPORTED_FLAG: &str = "imported.flag";

/// Get an instance of key for given table
fn table_key<T: Table>(key: &str) -> Result<T::Key, eyre::Error> {
    serde_json::from_str(key).map_err(|e| eyre::eyre!(e))
}

/// Reads the header RLP from a file and returns the Header.
fn read_header_from_file(path: PathBuf) -> Result<Header, eyre::Error> {
    let mut file = File::open(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    let header = Header::decode(&mut &buf[..])?;
    Ok(header)
}

fn import_state(
    env: &EnvironmentArgs<GnosisChainSpecParser>,
    state: PathBuf,
    header: PathBuf,
    header_hash: &str,
    total_difficulty: &str,
) -> Result<(), eyre::Error> {
    let Environment {
        config,
        provider_factory,
        ..
    } = env.init::<GnosisNode>(AccessRights::RW)?;

    let static_file_provider = provider_factory.static_file_provider();
    let provider_rw = provider_factory.database_provider_rw()?;

    // ensure header, total difficulty and header hash are provided
    let header = read_header_from_file(header)?;
    let header_hash = B256::from_str(header_hash)?;
    let total_difficulty = U256::from_str(total_difficulty)?;

    let last_block_number = provider_rw.last_block_number()?;

    if last_block_number == 0 {
        without_evm::setup_without_evm(
            &provider_rw,
            // &header,
            // header_hash,
            SealedHeader::new(header.into(), header_hash),
            total_difficulty,
            |number| Header {
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

    info!(target: "reth::cli", "Initiating state dump");

    let reader = BufReader::new(reth_fs_util::open(state)?);

    let hash = init_from_state_dump(reader, &provider_rw, config.stages.etl)?;

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
        if imported_flag_path.exists() {
            println!("✅ State is imported, skipping import.");
            return;
        } else {
            println!("❌ State looks misconfigured, please delete the following directory and try again:");
            println!("{datadir:?}");
            std::process::exit(1);
        }
    }

    let state_path_str = format!("./{chain}-state");
    let state_path = Path::new(&state_path_str);

    if let Err(e) = tokio_runtime()
        .expect("Unable to build runtime")
        .block_on(ensure_state(state_path, chain))
    {
        eprintln!("state setup failed: {e}");
        std::process::exit(1);
    }

    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    let state_file: PathBuf = state_path.join("state.jsonl");
    let header_file: PathBuf = state_path.join("header.rlp");

    import_state(
        &env,
        state_file,
        header_file.clone(),
        download_spec.header_hash,
        download_spec.total_difficulty,
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
                let header = Header::decompress(content[0].as_slice()).unwrap();
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
