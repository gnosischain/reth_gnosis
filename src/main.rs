use clap::{Args, Parser};
use reth::{
    chainspec::EthereumChainSpecParser,
    cli::{Cli, Commands},
    CliRunner,
};
use reth_gnosis::{
    execute::{GnosisExecutionStrategyFactory, GnosisExecutorProvider},
    GnosisNode,
};
use reth_node_ethereum::BasicBlockExecutorProvider;
use tracing::{error, info};

// We use jemalloc for performance reasons
#[cfg(all(feature = "jemalloc", unix))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// No Additional arguments
#[derive(Debug, Clone, Copy, Default, Args)]
#[non_exhaustive]
pub struct NoArgs;

fn main() {
    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    let cli = Cli::<EthereumChainSpecParser, NoArgs>::parse();
    let _ = cli.init_tracing().unwrap();

    match cli.command {
        Commands::Import(command) => {
            info!(target: "reth::cli", "Importing with custom cli");
            let runner = CliRunner::default();
            let res = runner.run_blocking_until_ctrl_c(command.execute::<GnosisNode, _, _>(
                |chain_spec| -> BasicBlockExecutorProvider<GnosisExecutionStrategyFactory> {
                    GnosisExecutorProvider::gnosis(chain_spec)
                },
            ));
            if let Err(err) = res {
                error!(target: "reth::cli", "Error: {err:?}");
                std::process::exit(1);
            }
        }
        Commands::InitState(command) => {
            let runner = CliRunner::default();
            let res = runner.run_blocking_until_ctrl_c(command.execute::<GnosisNode>());
            if let Err(err) = res {
                error!(target: "reth::cli", "Error: {err:?}");
                std::process::exit(1);
            }
        }
        _ => {
            if let Err(err) = cli.run(|builder, _| async move {
                let handle = builder.node(GnosisNode::new()).launch().await?;

                handle.node_exit_future.await
            }) {
                error!(target: "reth::cli", "Error: {err:?}");
                std::process::exit(1);
            }
        }
    }
}
