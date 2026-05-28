use clap::{Args, Parser};
use reth::api::FullNodeComponents;
use reth::args::DefaultStorageValues;
use reth_cli_commands::common::EnvironmentArgs;
use reth_cli_commands::download::DownloadDefaults;
use reth_gnosis::cli::gnosis_cli::Commands;
use reth_gnosis::engine::GnosisEngineValidator;
use reth_gnosis::initialize::download_init_state::{CHIADO_DOWNLOAD_SPEC, GNOSIS_DOWNLOAD_SPEC};
use reth_gnosis::initialize::import_and_ensure_state::download_and_import_init_state;
use reth_gnosis::initialize::SNAPSHOT_API_URL;
use reth_gnosis::{
    cli::gnosis_cli::GnosisCli, spec::gnosis_spec::GnosisChainSpecParser, GnosisNode,
};
use reth_rpc::ValidationApi;
use reth_rpc_api::servers::BlockSubmissionValidationApiServer;
use reth_rpc_builder::{config::RethRpcServerConfig, RethRpcModule};
use std::sync::Arc;

// We use jemalloc for performance reasons
#[cfg(all(feature = "jemalloc", unix))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Gnosis-specific extension args attached to the `node` command.
///
/// Surfaced as a `Gnosis` group in `reth node --help`.
#[derive(Debug, Clone, Copy, Default, Args)]
#[command(next_help_heading = "Gnosis")]
pub struct GnosisExt {
    /// Download and import a canonical post-merge state snapshot before sync starts.
    ///
    /// By default the node syncs from genesis using AuRa consensus (and reth's
    /// modern v2 storage layout). Pass this flag to instead fetch a canonical
    /// post-merge state snapshot for the Gnosis (chain 100) or Chiado (chain
    /// 10200) network and import it before sync starts; this also forces the
    /// legacy v1 storage layout, which is required by the import path.
    ///
    /// The import is idempotent — once successful, `imported.flag` is written
    /// to the datadir and subsequent launches skip the download regardless of
    /// this flag.
    #[arg(long = "gnosis.import-post-merge-state", default_value_t = false)]
    pub import_post_merge_state: bool,
}

type CliGnosis = GnosisCli<GnosisChainSpecParser, GnosisExt>;

fn main() {
    let _ = DownloadDefaults::default()
        .with_snapshot_api_url(SNAPSHOT_API_URL)
        .with_long_help(format!(
            "Snapshots for Gnosis Chain and Chiado.\n\n\
            Auto-discovery: `reth download --chain chiado` (or `--chain gnosis`) picks \
            the latest snapshot from {SNAPSHOT_API_URL}.\n\n\
            Manual: pass --manifest-url with one of:\n    \
                {SNAPSHOT_API_URL}/latest/chiado/manifest.json\n    \
                {SNAPSHOT_API_URL}/latest/gnosis/manifest.json",
        ))
        .try_init();

    let user_cli = CliGnosis::parse();
    let _guard = user_cli.init_tracing();

    // Optional post-merge state import for `reth node` on Gnosis / Chiado.
    // Disabled by default; enable with `--gnosis.import-post-merge-state`.
    // The default path is genesis sync via AuRa consensus on reth's v2 layout.
    // The import is idempotent — `imported.flag` in the datadir prevents re-import.
    if let Commands::Node(ref node_cmd) = user_cli.command {
        if node_cmd.ext.import_post_merge_state {
            // Force v1 (legacy) storage layout for new databases. The import path
            // calls `setup_without_evm`, which does not populate the v2
            // AccountChangeSets / StorageChangeSets static-file segments at the
            // non-zero genesis block, tripping reth's launch consistency check.
            // Genesis-sync (AuRa) skips this branch and uses reth's default v2.
            let _ = DefaultStorageValues::default().with_v2(false).try_init();

            let env = EnvironmentArgs::<GnosisChainSpecParser> {
                datadir: node_cmd.datadir.clone(),
                config: node_cmd.config.clone(),
                chain: node_cmd.chain.clone(),
                db: node_cmd.db,
                static_files: node_cmd.static_files,
                storage: node_cmd.storage,
            };

            match node_cmd.chain.chain().id() {
                100 => download_and_import_init_state("gnosis", GNOSIS_DOWNLOAD_SPEC, env),
                10200 => download_and_import_init_state("chiado", CHIADO_DOWNLOAD_SPEC, env),
                _ => {} // For other networks do not download state.
            }
        }
    }

    // Actual program run
    run_reth(user_cli);
}

fn run_reth(cli: CliGnosis) {
    if let Err(err) = cli.run(|builder, _| async move {
        let handle = builder
            .node(GnosisNode::new())
            .extend_rpc_modules(|ctx| {
                let validation_api = ValidationApi::new(
                    ctx.provider().clone(),
                    Arc::new(ctx.node().consensus().clone()),
                    ctx.node().evm_config().clone(),
                    ctx.config().rpc.flashbots_config(),
                    ctx.node().task_executor().clone(),
                    Arc::new(GnosisEngineValidator::new(ctx.config().chain.clone())),
                );
                ctx.modules.merge_if_module_configured(
                    RethRpcModule::Flashbots,
                    validation_api.into_rpc(),
                )?;
                Ok(())
            })
            .launch_with_debug_capabilities()
            .await?;

        // Log fork IDs after node startup when tracing is initialized
        handle.node.chain_spec().log_all_fork_ids();

        handle.node_exit_future.await
    }) {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
