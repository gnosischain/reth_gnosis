//! Command that initializes the node by importing a chain from ERA files.
use alloy_chains::{ChainKind, NamedChain};
use clap::{Args, Parser};
use eyre::eyre;
use reqwest::{Client, Url};
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{AccessRights, CliNodeTypes, Environment, EnvironmentArgs};
use reth_era_downloader::{read_dir, EraClient, EraStream, EraStreamConfig};
use reth_etl::Collector;
use reth_fs_util as fs;
use reth_node_core::version::SHORT_VERSION;
use reth_provider::StaticFileProviderFactory;
use reth_static_file_types::StaticFileSegment;
use std::{path::PathBuf, sync::Arc};
use tracing::info;

use crate::{cli::era, initialize::MAINNET_ERA_IMPORT_HEIGHT, primitives::GnosisNodePrimitives, spec::gnosis_spec::GnosisChainSpecParser, GnosisNode};

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

trait TryFromChain {
    fn try_to_url(&self) -> eyre::Result<Url>;
}

impl TryFromChain for ChainKind {
    fn try_to_url(&self) -> eyre::Result<Url> {
        Ok(match self {
            ChainKind::Named(NamedChain::Mainnet) => {
                Url::parse("https://era.ithaca.xyz/era1/index.html").expect("URL should be valid")
            }
            ChainKind::Named(NamedChain::Sepolia) => {
                Url::parse("https://era.ithaca.xyz/sepolia-era1/index.html")
                    .expect("URL should be valid")
            }
            chain => return Err(eyre!("No known host for ERA files on chain {chain:?}")),
        })
    }
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
where C: ChainSpecParser<ChainSpec: EthChainSpec + EthereumHardforks>,
      N: CliNodeTypes<ChainSpec = C::ChainSpec, Primitives = GnosisNodePrimitives>,
{
    dbg!("Starting import era...");
    info!(target: "reth::cli", "reth {} starting", SHORT_VERSION);

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
        // 100 => Some(MAINNET_ERA_IMPORT_HEIGHT), // Gnosis mainnet
        100 => Some(MAINNET_ERA_IMPORT_HEIGHT), // Gnosis mainnet
        _ => panic!("Era import not supported for chain id {}", env.chain.chain_id()),
    };

    let url = Url::parse("https://gc-era.gnosiscoredevs.io/").unwrap();
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

    Ok(())
}