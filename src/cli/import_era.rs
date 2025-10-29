// File copied from https://github.com/paradigmxyz/reth/blob/94c93583af801a75ae5bb96080d21dc1851325fa/crates/cli/commands/src/import_era.rs
// Needed due to the addition of max_height till which to import ERA blocks
// This fills the gap from block 0 till the block from which reth_gnosis can switch to normal sync

//! Command that initializes the node by importing a chain from ERA files.
use clap::{Args, Parser};
use reqwest::{Client, Url};
use reth::version::version_metadata;
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{AccessRights, CliNodeTypes, Environment, EnvironmentArgs};
use reth_era_downloader::{EraClient, EraStream, EraStreamConfig};
use reth_etl::Collector;
use reth_fs_util as fs;
use reth_provider::StaticFileProviderFactory;
use reth_static_file_types::StaticFileSegment;
use std::{path::PathBuf, sync::Arc};
use tracing::info;

use crate::{cli::era, initialize::MAINNET_ERA_IMPORT_HEIGHT, primitives::GnosisNodePrimitives};

pub const ERA_IMPORT_URL: &str = "https://gc-era.gnosiscoredevs.io/";
pub const ERA_IMPORTED_FLAG: &str = "era-imported.flag";

/// Syncs ERA encoded blocks from a local or remote source.
#[derive(Debug, Parser)]
pub struct ImportEraCommand<C: ChainSpecParser> {
    #[command(flatten)]
    env: EnvironmentArgs<C>,

    #[clap(flatten)]
    import: ImportArgs,
}

#[derive(Debug, Args)]
#[group(required = false, multiple = false)]
pub struct ImportArgs {
    /// The path to a directory for import.
    ///
    /// The ERA1 files are read from the local directory parsing headers and bodies.
    #[arg(long, value_name = "IMPORT_ERA_PATH", verbatim_doc_comment)]
    path: Option<PathBuf>,

    /// The URL to a remote host where the ERA1 files are hosted.
    ///
    /// The ERA1 files are read from the remote host using HTTP GET requests parsing headers
    /// and bodies.
    #[arg(long, value_name = "IMPORT_ERA_URL", verbatim_doc_comment)]
    url: Option<Url>,
}

impl<C: ChainSpecParser<ChainSpec: EthChainSpec + EthereumHardforks>> ImportEraCommand<C> {
    /// Execute `import-era` command
    pub async fn execute<N>(self) -> eyre::Result<()>
    where
        N: CliNodeTypes<ChainSpec = C::ChainSpec, Primitives = GnosisNodePrimitives>,
    {
        execute_inner::<C, N>(&self.env)
    }
}

impl<C: ChainSpecParser> ImportEraCommand<C> {
    /// Returns the underlying chain being used to run this command
    pub fn chain_spec(&self) -> Option<&Arc<C::ChainSpec>> {
        Some(&self.env.chain)
    }
}

pub fn execute_inner<C, N>(env: &EnvironmentArgs<C>) -> eyre::Result<()>
where
    C: ChainSpecParser<ChainSpec: EthChainSpec + EthereumHardforks>,
    N: CliNodeTypes<ChainSpec = C::ChainSpec, Primitives = GnosisNodePrimitives>,
{
    dbg!("Starting import era...");
    info!(target: "reth::cli", "reth {} starting", version_metadata().short_version.as_ref());

    let Environment {
        provider_factory,
        config,
        ..
    } = env.init::<N>(AccessRights::RW)?;

    let mut hash_collector = Collector::new(config.stages.etl.file_size, config.stages.etl.dir);

    let next_block = provider_factory
        .static_file_provider()
        .get_highest_static_file_block(StaticFileSegment::Headers)
        .unwrap_or_default()
        + 1;

    let max_height: Option<u64> = match env.chain.chain_id() {
        100 => Some(MAINNET_ERA_IMPORT_HEIGHT), // Gnosis mainnet
        _ => panic!(
            "Era import not supported for chain id {}",
            env.chain.chain_id()
        ),
    };

    let url = Url::parse(ERA_IMPORT_URL).unwrap();
    let folder = env
        .datadir
        .clone()
        .resolve_datadir(env.chain.chain())
        .data_dir()
        .join("era");

    fs::create_dir_all(&folder)?;

    let config = EraStreamConfig::default().start_from(next_block);
    let client = EraClient::new(Client::new(), url, folder);
    let stream = EraStream::new(client, config);

    era::import(stream, &provider_factory, &mut hash_collector, max_height)?;

    let datadir = env.datadir.clone().resolve_datadir(env.chain.chain());
    let datadir = datadir.data_dir();

    // create the IMPORTED_FLAG file
    let imported_flag_path = datadir.join(ERA_IMPORTED_FLAG);
    if let Err(e) = std::fs::File::create(imported_flag_path) {
        eprintln!("Failed to create {ERA_IMPORTED_FLAG} file: {e}");
        std::process::exit(1);
    }
    println!("✅ ERA imported successfully.");

    Ok(())
}
