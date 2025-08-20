use clap::Parser;
use reth::beacon_consensus::EthBeaconConsensus;
use reth_cli_commands::common::EnvironmentArgs;
use reth_cli_commands::node::NoArgs;
use reth_gnosis::cli::gnosis_cli::{Commands, GnosisCli, GnosisNodeExt};
use reth_gnosis::initialize::download_init_state::{CHIADO_DOWNLOAD_SPEC, GNOSIS_DOWNLOAD_SPEC};
use reth_gnosis::initialize::import_and_ensure_state::download_and_import_init_state;
use reth_gnosis::GnosisEvmConfig;
use reth_gnosis::{spec::gnosis_spec::GnosisChainSpecParser, GnosisNode};
use tracing::{error, info};

// We use jemalloc for performance reasons
#[cfg(all(feature = "jemalloc", unix))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() {
    let user_cli: GnosisCli<GnosisChainSpecParser, GnosisNodeExt> = GnosisCli::parse();

    let _guard = user_cli.init_tracing();

    if let Commands::Node(ref node_cmd) = user_cli.command {
        let env = EnvironmentArgs::<GnosisChainSpecParser> {
            datadir: node_cmd.datadir.clone(),
            config: node_cmd.config.clone(),
            chain: node_cmd.chain.clone(),
            db: node_cmd.db,
        };

        info!("Debug Exts: {:?}", node_cmd.ext);

        match node_cmd.chain.chain().id() {
            100 => {
                if node_cmd.ext.include_pre_merge {
                    // TODO: Era file import
                    unimplemented!("Pre-merge era file import is not implemented yet.");
                }
                if !node_cmd.ext.no_download {
                    // Fetch pre-merge state from a URL and load into the DB
                    download_and_import_init_state("gnosis", GNOSIS_DOWNLOAD_SPEC, env, !node_cmd.ext.include_pre_merge)
                }
            },
            10200 => {
                if node_cmd.ext.include_pre_merge {
                    error!("Pre-merge era file import is not implemented for Gnosis testnet (chain ID 10200).");
                    std::process::exit(1);
                }
                if !node_cmd.ext.no_download {
                    // Fetch pre-merge state from a URL and load into the DB
                    download_and_import_init_state("chiado", CHIADO_DOWNLOAD_SPEC, env, true) // no era files for Chiado
                }
            }
            _ => {} // For other network do not download state
        }
    }

    info!("Starting reth with Gnosis chain spec");
    // Actual program run
    run_reth(user_cli);
}

fn run_reth(cli: GnosisCli<GnosisChainSpecParser, GnosisNodeExt>) {
    if let Err(err) = cli.run_with_components::<GnosisNode>(
        |chain_spec| {
            (
                GnosisEvmConfig::new(chain_spec.clone()),
                EthBeaconConsensus::new(chain_spec),
            )
        },
        async move |builder, _ext| {
            let handle = builder
                .node::<GnosisNode>(GnosisNode::new())
                .launch()
                .await?;
            handle.node_exit_future.await
        },
    ) {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
