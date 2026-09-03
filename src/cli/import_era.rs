// File copied from https://github.com/paradigmxyz/reth/blob/94c93583af801a75ae5bb96080d21dc1851325fa/crates/cli/commands/src/import_era.rs
// Needed due to the addition of max_height till which to import ERA blocks
// This fills the gap from block 0 till the block from which reth_gnosis can switch to normal sync
//
// The import itself also stays forked (see `crate::cli::era`): upstream's `store_receipts` is a
// repair path for an already-executed range, and refuses a fresh database
// ("this database has executed no blocks"). reth_gnosis bootstraps headers, bodies and receipts
// together from block 0, which only the forked import does.

//! Command that initializes the node by importing a chain from ERA files.
use clap::{Args, Parser};
use eyre::eyre;
use reqwest::{Client, Url};
use reth::version::version_metadata;
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{AccessRights, CliNodeTypes, Environment, EnvironmentArgs};
use reth_era::common::file_ops::EraFileType;
use reth_era_downloader::{read_dir, EraClient, EraStream, EraStreamConfig};
use reth_etl::Collector;
use reth_fs_util as fs;
use reth_provider::StaticFileProviderFactory;
use reth_static_file_types::StaticFileSegment;
use std::{path::PathBuf, sync::Arc};
use tracing::info;

use crate::{cli::era, initialize::MAINNET_ERA_IMPORT_HEIGHT, primitives::GnosisNodePrimitives};

/// Gnosis mainnet ERA1 archive: `.era1`, pre-merge execution blocks, receipts mandatory.
pub const ERA1_IMPORT_URL: &str = "https://gc-era.gnosiscoredevs.io/";

/// Gnosis mainnet ERE archive: `.erae`, pre- and post-merge execution blocks.
///
/// Not usable with `--url` yet: the downloader enumerates files by scraping a directory index,
/// which this host does not serve. Fetch the files and pass `--path` instead.
pub const ERAE_IMPORT_URL: &str = "https://erae-gnosis.nethermind.dev/";

/// Syncs ERA encoded blocks from a local or remote source.
#[derive(Debug, Parser)]
pub struct ImportEraCommand<C: ChainSpecParser> {
    #[command(flatten)]
    env: EnvironmentArgs<C>,

    #[clap(flatten)]
    import: ImportArgs,

    /// Stop the import after this block height has been reached.
    ///
    /// The file containing the block is imported up to and including this height, then the
    /// import ends. Defaults to the height at which reth_gnosis switches over to normal sync
    /// (`MAINNET_ERA_IMPORT_HEIGHT`) on Gnosis mainnet, and to no limit elsewhere.
    #[arg(long, value_name = "TO_BLOCK", verbatim_doc_comment)]
    to_block: Option<u64>,
}

#[derive(Debug, Args)]
#[group(required = false, multiple = false)]
pub struct ImportArgs {
    /// The path to a directory for import.
    ///
    /// The ERA1 (`.era1`) or ERE (`.ere`, `.erae`) files are read from the local directory,
    /// parsing headers, bodies and receipts. The format is detected from the file extensions.
    #[arg(long, value_name = "IMPORT_ERA_PATH", verbatim_doc_comment)]
    path: Option<PathBuf>,

    /// The URL to a remote host where the ERA1 or ERE files are hosted.
    ///
    /// The files are read from the remote host using HTTP GET requests. The format is detected
    /// from the URL, and the host must serve a directory index listing the files.
    #[arg(long, value_name = "IMPORT_ERA_URL", verbatim_doc_comment)]
    url: Option<Url>,
}

impl<C: ChainSpecParser<ChainSpec: EthChainSpec + EthereumHardforks>> ImportEraCommand<C> {
    /// Execute `import-era` command
    pub async fn execute<N>(self, runtime: reth::tasks::Runtime) -> eyre::Result<()>
    where
        N: CliNodeTypes<ChainSpec = C::ChainSpec, Primitives = GnosisNodePrimitives>,
    {
        info!(target: "reth::cli", "reth {} starting", version_metadata().short_version.as_ref());

        let Environment {
            provider_factory,
            config,
            ..
        } = self.env.init::<N>(AccessRights::RW, runtime)?;

        let mut hash_collector = Collector::new(config.stages.etl.file_size, config.stages.etl.dir);

        let next_block = provider_factory
            .static_file_provider()
            .get_highest_static_file_block(StaticFileSegment::Headers)
            .unwrap_or_default()
            + 1;

        let to_block = self
            .to_block
            .or_else(|| default_to_block(self.env.chain.as_ref()));

        if let Some(path) = self.import.path {
            let era_type = EraFileType::from_dir(&path)?.ok_or_else(|| {
                eyre!(
                    "No ERA1 (.era1) or ERE (.ere, .erae) files found in {}",
                    path.display()
                )
            })?;

            info!(target: "reth::cli", ?era_type, path = %path.display(), ?to_block, "Starting ERA import");

            match era_type {
                EraFileType::Era1 => era::import::<era::Era1, _, _, _, _, _, _>(
                    read_dir(path, next_block)?,
                    &provider_factory,
                    &mut hash_collector,
                    to_block,
                )?,
                EraFileType::Ere => era::import::<era::Ere, _, _, _, _, _, _>(
                    read_dir(path, next_block)?,
                    &provider_factory,
                    &mut hash_collector,
                    to_block,
                )?,
                EraFileType::Era => return Err(unsupported_era()),
            };
        } else {
            let url = match self.import.url {
                Some(url) => url,
                None => default_url(self.env.chain.as_ref())?,
            };
            let era_type = EraFileType::from_url(url.as_str());

            info!(target: "reth::cli", ?era_type, %url, ?to_block, "Starting ERA import");

            let folder = self
                .env
                .datadir
                .clone()
                .resolve_datadir(self.env.chain.chain())
                .data_dir()
                .join("era");

            fs::create_dir_all(&folder)?;

            let config = EraStreamConfig::default().start_from(next_block);
            let client = EraClient::new(Client::new(), url, folder).with_era_type(era_type);
            let stream = EraStream::new(client, config);

            match era_type {
                EraFileType::Era1 => era::import::<era::Era1, _, _, _, _, _, _>(
                    stream,
                    &provider_factory,
                    &mut hash_collector,
                    to_block,
                )?,
                EraFileType::Ere => era::import::<era::Ere, _, _, _, _, _, _>(
                    stream,
                    &provider_factory,
                    &mut hash_collector,
                    to_block,
                )?,
                EraFileType::Era => return Err(unsupported_era()),
            };
        }

        println!("✅ ERA imported successfully.");

        Ok(())
    }
}

impl<C: ChainSpecParser> ImportEraCommand<C> {
    /// Returns the underlying chain being used to run this command
    pub fn chain_spec(&self) -> Option<&Arc<C::ChainSpec>> {
        Some(&self.env.chain)
    }
}

/// Consensus-layer `.era` files hold beacon blocks, not the execution blocks and receipts this
/// import needs.
fn unsupported_era() -> eyre::Report {
    eyre!("consensus `.era` files are not supported: use `.era1` or `.ere`/`.erae` instead")
}

/// Height at which reth_gnosis stops ERA import and hands over to normal sync.
fn default_to_block(chain: &impl EthChainSpec) -> Option<u64> {
    match chain.chain_id() {
        100 => Some(MAINNET_ERA_IMPORT_HEIGHT), // Gnosis mainnet
        _ => None,
    }
}

/// Default ERA host per chain, used when neither `--url` nor `--path` is given.
fn default_url(chain: &impl EthChainSpec) -> eyre::Result<Url> {
    match chain.chain_id() {
        100 => Ok(Url::parse(ERA1_IMPORT_URL).expect("URL should be valid")),
        id => Err(eyre!(
            "No known ERA host for chain id {id}: pass --url or --path"
        )),
    }
}
