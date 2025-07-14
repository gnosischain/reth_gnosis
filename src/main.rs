use clap::{Args, Parser};
use reth_cli_commands::common::EnvironmentArgs;
use reth_gnosis::initialize::download_init_state::{CHIADO_DOWNLOAD_SPEC, GNOSIS_DOWNLOAD_SPEC};
use reth_gnosis::initialize::import_and_ensure_state::download_and_import_init_state;
use reth_gnosis::{cli::Cli, spec::gnosis_spec::GnosisChainSpecParser, GnosisNode};

// We use jemalloc for performance reasons
#[cfg(all(feature = "jemalloc", unix))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// No Additional arguments
#[derive(Debug, Clone, Copy, Default, Args)]
#[non_exhaustive]
pub struct NoArgs;

type CliGnosis = Cli<GnosisChainSpecParser, NoArgs>;

fn main() {
    let user_cli = CliGnosis::parse();
    let _guard = user_cli.init_tracing();

    // Fetch pre-merge state from a URL and load into the DB
    if let reth::cli::Commands::Node(ref node_cmd) = user_cli.command {
        let env = EnvironmentArgs::<GnosisChainSpecParser> {
            datadir: node_cmd.datadir.clone(),
            config: node_cmd.config.clone(),
            chain: node_cmd.chain.clone(),
            db: node_cmd.db,
        };

        match node_cmd.chain.chain().id() {
            100 => download_and_import_init_state("gnosis", GNOSIS_DOWNLOAD_SPEC, env),
            10200 => download_and_import_init_state("chiado", CHIADO_DOWNLOAD_SPEC, env),
            _ => {} // For other network do not download state
        }
    }

    // Actual program run
    run_reth(user_cli);
}

fn run_reth(cli: CliGnosis) {
    if let Err(err) = cli.run(|builder, _| async move {
        let handle = builder
            .node::<GnosisNode>(GnosisNode::new())
            .launch()
            .await?;
        handle.node_exit_future.await
    }) {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
