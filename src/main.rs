use clap::{Args, Parser};
use reth_gnosis::download_init_state::{
    ensure_state, DownloadStateSpec, CHIADO_DOWNLOAD_SPEC, GNOSIS_DOWNLOAD_SPEC,
};
use reth_gnosis::{cli::Cli, spec::GnosisChainSpecParser, GnosisNode};
use std::path::Path;

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

    // Fetch pre-merge state from a URL and load into the DB
    if let reth::cli::Commands::Node(ref node_cmd) = user_cli.command {
        let datadir = node_cmd
            .datadir
            .datadir
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        match node_cmd.chain.chain().id() {
            100 => download_and_import_init_state(&datadir, "gnosis", GNOSIS_DOWNLOAD_SPEC),
            10200 => download_and_import_init_state(&datadir, "chiado", CHIADO_DOWNLOAD_SPEC),
            _ => {} // For other network do not download state
        }
    }

    // Actual program run
    run_reth(user_cli);
}

fn download_and_import_init_state(
    datadir: &Option<String>,
    chain: &str,
    download_spec: DownloadStateSpec,
) {
    let state_path = Path::new("./here");
    if let Err(e) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Unable to build runtime")
        .block_on(ensure_state(&state_path))
    {
        eprintln!("state setup failed: {e}");
        std::process::exit(1);
    }

    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    // reth --chain "$SCRIPT_DIR/chainspecs/chiado.json" db --datadir "$DATA_DIR" get static-file headers 700000
    run_reth(CliGnosis::parse_from(&args_with_datadir(
        &[
            "reth", // dummy program name
            "--chain",
            chain,
            "db", // command
            "get",
            "static-file",
            "headers",
            download_spec.block_num,
        ],
        datadir,
    )));
    // TODO: Capture the output of this command somehow and compare with expected_state_root

    // reth --chain "$SCRIPT_DIR/chainspecs/chiado.json" init-state $STATE_FILE --without-evm --header $HEADER_FILE --total-difficulty 231708131825107706987652208063906496124457284 --header-hash cdc424294195555e53949b6043339a49b049b48caa8d85bc7d5f5d12a85964b6 --datadir $DATA_DIR
    run_reth(CliGnosis::parse_from(&args_with_datadir(
        &[
            "reth", // dummy program name
            "--chain",
            chain,
            "init-state", // command
            "$STATE_FILE",
            "--without-evm",
            "--header",
            "$HEADER_FILE",
            "--total-difficulty",
            download_spec.total_difficulty,
            "--header-hash",
            download_spec.header_hash,
        ],
        datadir,
    )));
}

fn args_with_datadir(args: &[&str], datadir: &Option<String>) -> Vec<String> {
    let mut args = args
        .iter()
        .map(|arg| arg.to_string())
        .collect::<Vec<String>>();
    if let Some(datadir) = datadir {
        args.push("--datadir".to_owned());
        args.push(datadir.clone());
    }
    args
}

fn run_reth(cli: CliGnosis) {
    if let Err(err) = cli.run(|builder, _| async move {
        let handle = builder.node(GnosisNode::new()).launch().await?;
        handle.node_exit_future.await
    }) {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
