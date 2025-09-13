use std::{ffi::OsString, fmt, future::Future, sync::Arc};

use clap::{value_parser, Parser};
use reth::{
    args::LogArgs,
    builder::{NodeBuilder, WithLaunchContext},
    cli::Commands,
    prometheus_exporter::install_prometheus_recorder,
    version::version_metadata,
    CliRunner,
};
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::{common::CliComponentsBuilder, launcher::FnLauncher, node::NoArgs};
use reth_db::DatabaseEnv;
use reth_ethereum_consensus::EthBeaconConsensus;
use reth_tracing::FileWorkerGuard;
use tracing::info;

use crate::{
    evm_config::GnosisEvmConfig,
    spec::gnosis_spec::{GnosisChainSpec, GnosisChainSpecParser},
    GnosisNode,
};

/// The main reth_gnosis cli interface.
///
/// This is the entrypoint to the executable.
#[derive(Debug, Parser)]
#[command(author, version = version_metadata().short_version.as_ref(), long_version = version_metadata().long_version.as_ref(), about = "Reth", long_about = None)]
pub struct Cli<Spec: ChainSpecParser = GnosisChainSpecParser, Ext: clap::Args + fmt::Debug = NoArgs>
{
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

impl Cli {
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

impl<C, Ext> Cli<C, Ext>
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
    ///
    /// This accepts a closure that is used to launch the node via the
    /// [`NodeCommand`](node::NodeCommand).
    ///
    /// This command will be run on the [default tokio runtime](reth_cli_runner::tokio_runtime).
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

    /// Execute the configured cli command with the provided [`CliRunner`].
    ///
    ///
    /// # Example
    ///
    /// ```no_run
    /// use reth_cli_runner::CliRunner;
    /// use reth_ethereum_cli::interface::Cli;
    /// use reth_node_ethereum::EthereumNode;
    ///
    /// let runner = CliRunner::try_default_runtime().unwrap();
    ///
    /// Cli::parse_args()
    ///     .with_runner(runner, |builder, _| async move {
    ///         let handle = builder.launch_node(EthereumNode::default()).await?;
    ///         handle.wait_for_node_exit().await
    ///     })
    ///     .unwrap();
    /// ```
    pub fn with_runner<L, Fut>(self, runner: CliRunner, launcher: L) -> eyre::Result<()>
    where
        L: FnOnce(WithLaunchContext<NodeBuilder<Arc<DatabaseEnv>, C::ChainSpec>>, Ext) -> Fut,
        Fut: Future<Output = eyre::Result<()>>,
        C: ChainSpecParser<ChainSpec = GnosisChainSpec>,
    {
        let components = |spec: Arc<C::ChainSpec>| {
            (
                GnosisEvmConfig::new(spec.clone()),
                EthBeaconConsensus::new(spec),
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
        _components: impl CliComponentsBuilder<GnosisNode>,
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
                runner.run_blocking_until_ctrl_c(command.execute::<GnosisNode>())
            }
            Commands::Stage(_command) => unimplemented!(),
            Commands::P2P(_command) => unimplemented!(),
            Commands::Config(command) => runner.run_until_ctrl_c(command.execute()),
            Commands::Recover(command) => {
                runner.run_command_until_exit(|ctx| command.execute::<GnosisNode>(ctx))
            }
            Commands::Prune(command) => runner.run_until_ctrl_c(command.execute::<GnosisNode>()),
            Commands::Import(_command) => unimplemented!(),
            // Commands::Debug(_command) => todo!(),
            Commands::ImportEra(_) => unimplemented!(),
            Commands::Download(_) => unimplemented!(),
            Commands::ExportEra(_export_era_command) => unimplemented!(),
            Commands::ReExecute(_command) => unimplemented!(),
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
