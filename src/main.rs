use std::env;

use clap::{Args, Parser};
use reth_cli_commands::common::EnvironmentArgs;
use reth_gnosis::cli::gnosis_cli::Commands;
use reth_gnosis::consts::{DEFAULT_7702_PATCH_TIME, DEFAULT_EL_PATCH_TIME};
use reth_gnosis::initialize::download_init_state::{CHIADO_DOWNLOAD_SPEC, GNOSIS_DOWNLOAD_SPEC};
use reth_gnosis::initialize::import_and_ensure_state::download_and_import_init_state;
use reth_gnosis::{
    cli::gnosis_cli::GnosisCli, spec::gnosis_spec::GnosisChainSpecParser, GnosisNode,
};

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

    let timestamp = env::var("GNOSIS_EL_PATCH_TIME")
        .unwrap_or(DEFAULT_EL_PATCH_TIME.to_string())
        .parse::<u64>()
        .unwrap_or_default();
    println!("Gnosis EL Patch Time is set to: {timestamp}");

    let is_patch2_enabled = env::var("GNOSIS_EL_7702_PATCH_TIME")
        .unwrap_or(DEFAULT_7702_PATCH_TIME.to_string())
        .parse::<u64>()
        .unwrap_or_default();
    println!("GNOSIS_EL_7702_PATCH_TIME Time is set to: {is_patch2_enabled}");

    // Fetch pre-merge state from a URL and load into the DB
    if let Commands::Node(ref node_cmd) = user_cli.command {
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
            .node(GnosisNode::new())
            .launch_with_debug_capabilities()
            .await?;
        handle.node_exit_future.await
    }) {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
