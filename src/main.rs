use clap::{Args, Parser};
use reth::api::FullNodeComponents;
use reth_gnosis::engine::GnosisEngineValidator;
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

/// No Additional arguments
#[derive(Debug, Clone, Copy, Default, Args)]
#[non_exhaustive]
pub struct NoArgs;

type CliGnosis = GnosisCli<GnosisChainSpecParser, NoArgs>;

fn main() {
    let user_cli = CliGnosis::parse();
    let _guard = user_cli.init_tracing();

    // Note: pre-merge state import has been removed — the node now syncs from genesis
    // using AuRa consensus. The import logic is preserved in src/initialize/ for reference.

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
                    Box::new(ctx.node().task_executor().clone()),
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
