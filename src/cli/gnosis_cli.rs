use std::{ffi::OsString, fmt, future::Future, sync::Arc};

use crate::cli::import_era;
use clap::{value_parser, Parser, Subcommand};
use reth::{
    args::LogArgs,
    builder::{NodeBuilder, WithLaunchContext},
    prometheus_exporter::install_prometheus_recorder,
    version::version_metadata,
    CliRunner,
};
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::{
    common::CliComponentsBuilder,
    config_cmd, db, download, dump_genesis, export_era, import, init_cmd, init_state,
    launcher::FnLauncher,
    node::{self, NoArgs},
    p2p, prune, re_execute, stage,
};
use reth_consensus::FullConsensus;
use reth_db::DatabaseEnv;
use reth_ethereum_consensus::EthBeaconConsensus;
use reth_tracing::FileWorkerGuard;
use tracing::info;

use crate::{
    evm_config::{GnosisEvmConfig, NoopHeaderLookup},
    primitives::GnosisNodePrimitives,
    spec::gnosis_spec::{GnosisChainSpec, GnosisChainSpecParser},
    GnosisNode,
};

/// The main reth_gnosis cli interface.
///
/// This is the entrypoint to the executable.
#[derive(Debug, Parser)]
#[command(author, version = version_metadata().short_version.as_ref(), long_version = version_metadata().long_version.as_ref(), about = "Reth", long_about = None)]
pub struct GnosisCli<
    Spec: ChainSpecParser = GnosisChainSpecParser,
    Ext: clap::Args + fmt::Debug = NoArgs,
> {
    /// The command to run
    #[command(subcommand)]
    pub command: Commands<Spec, Ext>,

    /// The chain this node is running.
    ///
    /// Possible values are either a built-in chain or the path to a chain specification file.
    #[arg(
        long,
        value_name = "CHAIN_OR_PATH",
        long_help = Spec::help_message(),
        default_value = Spec::SUPPORTED_CHAINS[0],
        value_parser = Spec::parser(),
        global = true,
    )]
    chain: Arc<Spec::ChainSpec>,

    /// Add a new instance of a node.
    ///
    /// Configures the ports of the node to avoid conflicts with the defaults.
    /// This is useful for running multiple nodes on the same machine.
    ///
    /// Max number of instances is 200. It is chosen in a way so that it's not possible to have
    /// port numbers that conflict with each other.
    ///
    /// Changes to the following port numbers:
    /// - `DISCOVERY_PORT`: default + `instance` - 1
    /// - `AUTH_PORT`: default + `instance` * 100 - 100
    /// - `HTTP_RPC_PORT`: default - `instance` + 1
    /// - `WS_RPC_PORT`: default + `instance` * 2 - 2
    #[arg(long, value_name = "INSTANCE", global = true, default_value_t = 1, value_parser = value_parser!(u16).range(..=200))]
    instance: u16,

    #[command(flatten)]
    logs: LogArgs,
}

impl GnosisCli {
    /// Parsers only the default CLI arguments
    pub fn parse_args() -> Self {
        Self::parse()
    }

    /// Parsers only the default CLI arguments from the given iterator
    pub fn try_parse_args_from<I, T>(itr: I) -> Result<Self, clap::error::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        Self::try_parse_from(itr)
    }
}

impl<C, Ext> GnosisCli<C, Ext>
where
    C: ChainSpecParser<ChainSpec = GnosisChainSpec>,
    Ext: clap::Args + fmt::Debug,
{
    /// Execute the configured cli command.
    ///
    /// This accepts a closure that is used to launch the node via the
    /// [`NodeCommand`](reth_cli_commands::node::NodeCommand).
    pub fn run<L, Fut>(self, launcher: L) -> eyre::Result<()>
    where
        L: FnOnce(WithLaunchContext<NodeBuilder<Arc<DatabaseEnv>, C::ChainSpec>>, Ext) -> Fut,
        Fut: Future<Output = eyre::Result<()>>,
        C: ChainSpecParser<ChainSpec = GnosisChainSpec>,
    {
        self.with_runner(CliRunner::try_default_runtime()?, launcher)
    }

    /// Execute the configured cli command with the provided [`CliComponentsBuilder`].
    pub fn run_with_components(
        self,
        components: impl CliComponentsBuilder<GnosisNode>,
        launcher: impl AsyncFnOnce(
            WithLaunchContext<NodeBuilder<Arc<DatabaseEnv>, C::ChainSpec>>,
            Ext,
        ) -> eyre::Result<()>,
    ) -> eyre::Result<()>
    where
        C: ChainSpecParser<ChainSpec = GnosisChainSpec>,
    {
        self.with_runner_and_components(CliRunner::try_default_runtime()?, components, launcher)
    }

    pub fn with_runner<L, Fut>(self, runner: CliRunner, launcher: L) -> eyre::Result<()>
    where
        L: FnOnce(WithLaunchContext<NodeBuilder<Arc<DatabaseEnv>, C::ChainSpec>>, Ext) -> Fut,
        Fut: Future<Output = eyre::Result<()>>,
        C: ChainSpecParser<ChainSpec = GnosisChainSpec>,
    {
        let components = |spec: Arc<C::ChainSpec>| {
            (
                GnosisEvmConfig::new(spec.clone(), NoopHeaderLookup),
                Arc::new(EthBeaconConsensus::new(spec))
                    as Arc<dyn FullConsensus<GnosisNodePrimitives>>,
            )
        };

        self.with_runner_and_components(runner, components, async move |builder, ext| {
            launcher(builder, ext).await
        })
    }

    /// Execute the configured cli command with the provided [`CliRunner`] and
    /// [`CliComponentsBuilder`].
    pub fn with_runner_and_components(
        mut self,
        runner: CliRunner,
        components: impl CliComponentsBuilder<GnosisNode>,
        launcher: impl AsyncFnOnce(
            WithLaunchContext<NodeBuilder<Arc<DatabaseEnv>, C::ChainSpec>>,
            Ext,
        ) -> eyre::Result<()>,
    ) -> eyre::Result<()>
    where
        C: ChainSpecParser<ChainSpec = GnosisChainSpec>,
    {
        // Add network name if available to the logs dir
        if let Some(chain_spec) = self.command.chain_spec() {
            self.logs.log_file_directory = self
                .logs
                .log_file_directory
                .join(chain_spec.chain().to_string());
        }
        let _guard = self.init_tracing()?;
        info!(target: "reth::cli", "Initialized tracing, debug log directory: {}", self.logs.log_file_directory);

        // Install the prometheus recorder to be sure to record all metrics
        let _ = install_prometheus_recorder();

        match self.command {
            Commands::Node(command) => runner.run_command_until_exit(|ctx| {
                command.execute(ctx, FnLauncher::new::<C, Ext>(launcher))
            }),
            Commands::Init(command) => {
                runner.run_blocking_until_ctrl_c(command.execute::<GnosisNode>())
            }
            Commands::InitState(command) => {
                runner.run_blocking_until_ctrl_c(command.execute::<GnosisNode>())
            }
            Commands::DumpGenesis(command) => runner.run_blocking_until_ctrl_c(command.execute()),
            Commands::Db(command) => {
                runner.run_blocking_command_until_exit(|ctx| command.execute::<GnosisNode>(ctx))
            }
            Commands::Stage(command) => runner
                .run_command_until_exit(|ctx| command.execute::<GnosisNode, _>(ctx, components)),
            Commands::P2P(command) => runner.run_until_ctrl_c(command.execute::<GnosisNode>()),
            Commands::Config(command) => runner.run_until_ctrl_c(command.execute()),
            Commands::Prune(command) => runner.run_until_ctrl_c(command.execute::<GnosisNode>()),
            Commands::Import(command) => {
                runner.run_blocking_until_ctrl_c(command.execute::<GnosisNode, _>(components))
            }
            // Commands::Debug(_command) => todo!(),
            Commands::ImportEra(command) => {
                runner.run_blocking_until_ctrl_c(command.execute::<GnosisNode>())
            }
            Commands::Download(command) => {
                runner.run_blocking_until_ctrl_c(command.execute::<GnosisNode>())
            }
            Commands::ExportEra(_export_era_command) => unimplemented!(),
            Commands::ReExecute(command) => {
                runner.run_until_ctrl_c(command.execute::<GnosisNode>(components))
            }
        }
    }

    /// Initializes tracing with the configured options.
    ///
    /// If file logging is enabled, this function returns a guard that must be kept alive to ensure
    /// that all logs are flushed to disk.
    pub fn init_tracing(&self) -> eyre::Result<Option<FileWorkerGuard>> {
        let guard = self.logs.init_tracing()?;
        Ok(guard)
    }

    pub fn set_chain(&mut self, chain: Arc<GnosisChainSpec>) {
        self.chain = chain;
    }
}

// Ref: https://github.com/paradigmxyz/reth/blob/79c71b86924d6cb9f3d2fa36f1a6f49b27e1735d/crates/ethereum/cli/src/interface.rs#L222-L269

/// Implementing our own Commands is needed to have custom path for ImportEra command
/// Commands to be executed
#[derive(Debug, Subcommand)]
pub enum Commands<C: ChainSpecParser, Ext: clap::Args + fmt::Debug> {
    /// Start the node
    #[command(name = "node")]
    Node(Box<node::NodeCommand<C, Ext>>),
    /// Initialize the database from a genesis file.
    #[command(name = "init")]
    Init(init_cmd::InitCommand<C>),
    /// Initialize the database from a state dump file.
    #[command(name = "init-state")]
    InitState(init_state::InitStateCommand<C>),
    /// This syncs RLP encoded blocks from a file.
    #[command(name = "import")]
    Import(import::ImportCommand<C>),
    /// This syncs ERA encoded blocks from a directory.
    #[command(name = "import-era")]
    ImportEra(import_era::ImportEraCommand<C>),
    /// Exports block to era1 files in a specified directory.
    #[command(name = "export-era")]
    ExportEra(export_era::ExportEraCommand<C>),
    /// Dumps genesis block JSON configuration to stdout.
    DumpGenesis(dump_genesis::DumpGenesisCommand<C>),
    /// Database debugging utilities
    #[command(name = "db")]
    Db(db::Command<C>),
    /// Download public node snapshots
    #[command(name = "download")]
    Download(download::DownloadCommand<C>),
    /// Manipulate individual stages.
    #[command(name = "stage")]
    Stage(stage::Command<C>),
    /// P2P Debugging utilities
    #[command(name = "p2p")]
    P2P(Box<p2p::Command<C>>),
    /// Write config to stdout
    #[command(name = "config")]
    Config(config_cmd::Command),
    /// Prune according to the configuration without any limits
    #[command(name = "prune")]
    Prune(prune::PruneCommand<C>),
    /// Re-execute blocks in parallel to verify historical sync correctness.
    #[command(name = "re-execute")]
    ReExecute(re_execute::Command<C>),
}

impl<C: ChainSpecParser, Ext: clap::Args + fmt::Debug> Commands<C, Ext> {
    /// Returns the underlying chain being used for commands
    pub fn chain_spec(&self) -> Option<&Arc<C::ChainSpec>> {
        match self {
            Self::Node(cmd) => cmd.chain_spec(),
            Self::Init(cmd) => cmd.chain_spec(),
            Self::InitState(cmd) => cmd.chain_spec(),
            Self::Import(cmd) => cmd.chain_spec(),
            Self::ExportEra(cmd) => cmd.chain_spec(),
            Self::ImportEra(cmd) => cmd.chain_spec(),
            Self::DumpGenesis(cmd) => cmd.chain_spec(),
            Self::Db(cmd) => cmd.chain_spec(),
            Self::Download(cmd) => cmd.chain_spec(),
            Self::Stage(cmd) => cmd.chain_spec(),
            Self::P2P(cmd) => cmd.chain_spec(),
            Self::Config(_) => None,
            Self::Prune(cmd) => cmd.chain_spec(),
            Self::ReExecute(cmd) => cmd.chain_spec(),
        }
    }
}
