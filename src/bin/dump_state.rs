//! dump-state: A tool to dump account state from a reth database.
//!
//! This binary reads the PlainAccountState table and outputs account information
//! with various filtering options.

use alloy_primitives::{Address, B256, U256};
use clap::{Parser, ValueEnum};
use eyre::Result;
use reth::args::{DatabaseArgs, DatadirArgs};
use reth_cli::chainspec::ChainSpecParser;
use reth_db::{open_db_read_only, DatabaseEnv};
use reth_db_api::{cursor::DbCursorRO, tables, transaction::DbTx};
use reth_gnosis::{
    spec::gnosis_spec::{GnosisChainSpec, GnosisChainSpecParser},
    GnosisNode,
};
use reth_node_builder::{NodeTypes, NodeTypesWithDBAdapter};
use reth_provider::{providers::StaticFileProvider, ProviderFactory};
use serde::Serialize;
use std::{
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

/// Output format for the dump
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum OutputFormat {
    /// JSON Lines format (one JSON object per line)
    #[default]
    Jsonl,
    /// CSV format
    Csv,
    /// Summary statistics only
    Summary,
}

/// Filter for accounts
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum AccountFilter {
    /// All accounts
    #[default]
    All,
    /// Only accounts with bytecode (contracts)
    HasCode,
    /// Only accounts without bytecode (EOAs)
    NoCode,
    /// Only accounts with non-zero balance
    HasBalance,
}

/// dump-state: Dump account state from a reth database
#[derive(Debug, Parser)]
#[command(
    name = "dump-state",
    version,
    about = "Dump account state from a reth database"
)]
struct Args {
    /// The chain this node is running.
    ///
    /// Possible values are either a built-in chain or the path to a chain specification file.
    #[arg(
        long,
        value_name = "CHAIN_OR_PATH",
        long_help = GnosisChainSpecParser::help_message(),
        default_value = GnosisChainSpecParser::SUPPORTED_CHAINS[0],
        value_parser = GnosisChainSpecParser::parser(),
        global = true
    )]
    chain: Arc<GnosisChainSpec>,

    /// Parameters for datadir configuration
    #[command(flatten)]
    datadir: DatadirArgs,

    /// All database related arguments
    #[command(flatten)]
    db: DatabaseArgs,

    /// Output format
    #[arg(long, short = 'f', default_value = "jsonl")]
    format: OutputFormat,

    /// Filter accounts
    #[arg(long, short = 'F', default_value = "all")]
    filter: AccountFilter,

    /// Minimum balance filter (in wei)
    #[arg(long)]
    min_balance: Option<U256>,

    /// Maximum number of accounts to output (0 = unlimited)
    #[arg(long, short = 'n', default_value = "0")]
    limit: usize,

    /// Skip the first N accounts
    #[arg(long, default_value = "0")]
    skip: usize,

    /// Include bytecode in output (only for JSON format)
    #[arg(long)]
    include_bytecode: bool,

    /// Output file (defaults to stdout)
    #[arg(long, short = 'o')]
    output: Option<PathBuf>,
}

/// Account data for serialization
#[derive(Debug, Serialize)]
struct AccountEntry {
    address: Address,
    nonce: u64,
    balance: U256,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytecode_hash: Option<B256>,
    has_code: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytecode: Option<String>,
}

/// Summary statistics
#[derive(Debug, Default)]
struct Summary {
    total_accounts: usize,
    accounts_with_code: usize,
    accounts_without_code: usize,
    total_balance: U256,
    max_balance: U256,
    max_balance_address: Option<Address>,
    max_nonce: u64,
    max_nonce_address: Option<Address>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Resolve data directory
    let data_dir = args.datadir.clone().resolve_datadir(args.chain.chain());
    let db_path = data_dir.db();

    eprintln!("Opening database at: {}", db_path.display());

    // Open database in read-only mode
    let db = Arc::new(open_db_read_only(&db_path, args.db.database_args())?);

    // Create provider factory
    let sf_path = data_dir.static_files();
    let static_file_provider =
        StaticFileProvider::<<GnosisNode as NodeTypes>::Primitives>::read_only(sf_path, false)?;

    let provider_factory =
        ProviderFactory::<NodeTypesWithDBAdapter<GnosisNode, Arc<DatabaseEnv>>>::new(
            db.clone(),
            args.chain.clone(),
            static_file_provider,
        );

    // Get read transaction
    let provider = provider_factory.provider()?;
    let tx = provider.tx_ref();

    // Setup output
    let mut output: Box<dyn Write> = match &args.output {
        Some(path) => Box::new(std::fs::File::create(path)?),
        None => Box::new(io::stdout()),
    };

    // CSV header
    if matches!(args.format, OutputFormat::Csv) {
        writeln!(output, "address,nonce,balance,bytecode_hash,has_code")?;
    }

    let mut summary = Summary::default();
    let mut count = 0usize;
    let mut skipped = 0usize;

    // Create cursor and walk through all accounts
    let mut cursor = tx.cursor_read::<tables::PlainAccountState>()?;
    let mut walker = cursor.walk(None)?;

    while let Some(result) = walker.next() {
        let (address, account) = result?;

        // Apply filters
        let has_code = account.bytecode_hash.is_some();

        match args.filter {
            AccountFilter::All => {}
            AccountFilter::HasCode => {
                if !has_code {
                    continue;
                }
            }
            AccountFilter::NoCode => {
                if has_code {
                    continue;
                }
            }
            AccountFilter::HasBalance => {
                if account.balance.is_zero() {
                    continue;
                }
            }
        }

        // Apply minimum balance filter
        if let Some(min_bal) = args.min_balance {
            if account.balance < min_bal {
                continue;
            }
        }

        // Handle skip
        if skipped < args.skip {
            skipped += 1;
            continue;
        }

        // Update summary
        summary.total_accounts += 1;
        if has_code {
            summary.accounts_with_code += 1;
        } else {
            summary.accounts_without_code += 1;
        }
        summary.total_balance = summary.total_balance.saturating_add(account.balance);

        if account.balance > summary.max_balance {
            summary.max_balance = account.balance;
            summary.max_balance_address = Some(address);
        }

        if account.nonce > summary.max_nonce {
            summary.max_nonce = account.nonce;
            summary.max_nonce_address = Some(address);
        }

        // Output based on format
        match args.format {
            OutputFormat::Jsonl => {
                let bytecode = if args.include_bytecode {
                    if let Some(hash) = account.bytecode_hash {
                        // Try to fetch the bytecode
                        if let Ok(Some(code)) = tx.get::<tables::Bytecodes>(hash) {
                            Some(format!("0x{}", hex::encode(code.bytes())))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                let entry = AccountEntry {
                    address,
                    nonce: account.nonce,
                    balance: account.balance,
                    bytecode_hash: account.bytecode_hash,
                    has_code,
                    bytecode,
                };
                writeln!(output, "{}", serde_json::to_string(&entry)?)?;
            }
            OutputFormat::Csv => {
                writeln!(
                    output,
                    "{},{},{},{},{}",
                    address,
                    account.nonce,
                    account.balance,
                    account
                        .bytecode_hash
                        .map(|h| format!("{}", h))
                        .unwrap_or_default(),
                    has_code
                )?;
            }
            OutputFormat::Summary => {
                // Don't output individual accounts, just collect stats
            }
        }

        count += 1;

        // Check limit
        if args.limit > 0 && count >= args.limit {
            break;
        }

        // Progress indicator to stderr
        if count % 100_000 == 0 {
            eprintln!("Processed {} accounts...", count);
        }
    }

    // Output summary
    if matches!(args.format, OutputFormat::Summary) || count > 0 {
        eprintln!("\n=== Summary ===");
        eprintln!("Total accounts processed: {}", summary.total_accounts);
        eprintln!(
            "Accounts with code (contracts): {}",
            summary.accounts_with_code
        );
        eprintln!(
            "Accounts without code (EOAs): {}",
            summary.accounts_without_code
        );
        eprintln!("Total balance: {} wei", summary.total_balance);
        eprintln!(
            "Max balance: {} wei ({})",
            summary.max_balance,
            summary.max_balance_address.unwrap_or_default()
        );
        eprintln!(
            "Max nonce: {} ({})",
            summary.max_nonce,
            summary.max_nonce_address.unwrap_or_default()
        );
    }

    Ok(())
}
