// Gnosis-specific ERA1 export command.
//
// Upstream references:
//   - reth_cli_commands::export_era  (crates/cli/commands/src/export_era.rs)
//   - reth_era_utils::export         (crates/era-utils/src/export.rs)
//
// ══════════════════════════════════════════════════════════════════════════════
// DIFFERENCES FROM UPSTREAM
// ══════════════════════════════════════════════════════════════════════════════
//
// ── 1. Receipt encoding ───────────────────────────────────────────────────────
//
//   Upstream (reth_era_utils::export::compress_block_data):
//     CompressedReceipts::from_encodable_list(&receipts)
//     where receipts: Vec<Receipt>
//
//   The derived RlpEncodable on EthereumReceipt produces:
//     rlp([tx_type, success, cumulative_gas_used, logs])   ← WRONG
//
//   Gnosis (this file):
//     let receipts_with_bloom: Vec<_> = receipts.into_iter()
//         .map(|r| r.into_with_bloom())
//         .collect();
//     CompressedReceipts::from_encodable_list(&receipts_with_bloom)
//
//   Standard Ethereum wire format expected by the ERA1 spec:
//     legacy:  rlp([status, cumulative_gas_used, bloom, logs])
//     typed:   byte(tx_type) || rlp([status, cumulative_gas_used, bloom, logs])
//
//   The bloom (256 bytes) is computed from the receipt logs by into_with_bloom().
//   Without this fix the import at era.rs:250 fails with:
//     "RLP error: Failed to decode RLP data: unexpected length"
//   because it decodes as Vec<ReceiptWithBloom<Receipt>> and the missing bloom
//   causes the length mismatch.
//
// ── 2. Accumulator ────────────────────────────────────────────────────────────
//
//   Upstream (reth_era_utils::export):
//     let accumulator_hash = B256::from_slice(&final_header_data[0..32]);
//     // Uses the first 32 bytes of the last block's compressed (snappy) header
//     // as a placeholder hash. This is arbitrary and wrong.
//
//   Gnosis (this file):
//     compute_era1_accumulator(&block_records)
//
//   Implements the correct SSZ hash_tree_root(List[HeaderRecord, 8192]):
//     HeaderRecord        = (block_hash: Bytes32, total_difficulty: Uint256-LE)
//     hash_tree_root(rec) = sha256(block_hash || le_bytes32(total_difficulty))
//     merkle_root         = binary Merkle tree over 8192 padded leaves
//     mix_in_length       = sha256(merkle_root || le_bytes64(actual_count))
//
//   The result matches gc-era.gnosiscoredevs.io/accumulators.txt exactly.
//
// ── 3. Filename hash ──────────────────────────────────────────────────────────
//
//   Upstream:
//     Era1Id::new(...).with_hash([state_root[0..4]])
//     // First 4 bytes of the LAST block's state root → wrong, arbitrary
//     // Written to disk before computing blocks, so knowable upfront.
//
//   Gnosis:
//     Era1Id::new(...).with_hash([acc[0], acc[1], acc[2], acc[3]])
//     // First 4 bytes of the SSZ accumulator hash
//     // File is written to a temp path first, then renamed after the accumulator
//     // is computed from all block records.
//
//   Format: gnosis-00000-ac7f28ba.era1  (matches gc-era.gnosiscoredevs.io)
//
// ── 4. Network name ───────────────────────────────────────────────────────────
//
//   Upstream:
//     network: self.env.chain.chain().to_string()
//     // For Gnosis (chain ID 100) this yields "xdai" (historical alias)
//
//   Gnosis:
//     era1_network_name(self.env.chain.chain_id())
//     // Returns "gnosis" for chain ID 100, "chiado" for 10200
//     // Matches the prefix used by gc-era.gnosiscoredevs.io
//
// ── 5. TotalDifficulty byte order ─────────────────────────────────────────────
//
//   Upstream (reth_era::era1::types::execution::TotalDifficulty::to_entry):
//     let be_bytes = self.value.to_be_bytes_vec();
//     data[32 - be_bytes.len()..].copy_from_slice(&be_bytes);
//     // Stores U256 as big-endian, zero-padded on the LEFT
//     // e.g. 0x20000 → 00 00 00 ... 00 02 00 00  (BE)
//
//   ERA1 spec (https://github.com/eth-clients/e2store-format-specs):
//     total-difficulty := uint256-le
//     // 32-byte little-endian encoding
//     // e.g. 0x20000 → 00 00 02 00 ... 00 00 00  (LE)
//
//   Gnosis (this file):
//     let td_le: [u8; 32] = total_difficulty.to_le_bytes();
//     writer.write_entry(&Entry::new(TOTAL_DIFFICULTY, td_le.to_vec()))
//     // Uses E2StoreWriter directly, bypassing TotalDifficulty::to_entry()
//
//   Note: the import (era.rs) decodes TD via TotalDifficulty::from_entry() which
//   also uses big-endian, so our own round-trip still worked before this fix.
//   The fix matters for interoperability with other ERA1 tools.
//
// ── 6. Block-index offsets ────────────────────────────────────────────────────
//
//   Upstream (reth_era_utils::export):
//     offsets.push(position);   // absolute byte offset from start of file
//     // e.g. block 0 → 8, block 1 → 291, ...  (positive)
//
//   ERA1 spec:
//     block-index offsets are signed integers measured from the START of the
//     block-index record to the start of the corresponding block's header entry.
//     Since blocks precede the index, all offsets are NEGATIVE.
//     // e.g. block 0 → -3832039  (for a ~3.8 MB file)
//
//   Gnosis (this file):
//     let block_index_entry_pos = position + ACCUMULATOR_ENTRY_SIZE;
//     let relative_offsets: Vec<i64> = abs_offsets.iter()
//         .map(|&abs| abs - block_index_entry_pos)
//         .collect();
//
//   Note: the import (era.rs) uses sequential streaming and ignores offset
//   values entirely, so the old absolute offsets still worked for round-trips.
//   The fix matters for random-access readers and other ERA1 tools.
//
// ── 7. Writer type ────────────────────────────────────────────────────────────
//
//   Upstream:
//     let mut writer = Era1Writer::new(file);
//     writer.write_block(&block_tuple);
//     // Era1Writer::write_block calls TotalDifficulty::to_entry() internally,
//     // which uses big-endian (see difference #5 above).
//
//   Gnosis:
//     let mut writer = E2StoreWriter::new(file);
//     // Uses E2StoreWriter directly to write individual Entry values,
//     // which gives full control over the TD byte encoding.
//
// ── 8. Output manifest files ──────────────────────────────────────────────────
//
//   Upstream: writes only .era1 files, no manifest.
//
//   Gnosis: also writes two manifest files in the output directory:
//
//     checksums.txt     — SHA-256 of each .era1 file, one per line, 0x-prefixed
//                         Required by reth_era_downloader::read_dir for local
//                         import; that function verifies each file against this
//                         list before processing it.
//
//     accumulators.txt  — SSZ accumulator hash of each .era1 file, one per line,
//                         0x-prefixed. Mirrors gc-era.gnosiscoredevs.io/accumulators.txt
//                         for reference and verification.
//
// ══════════════════════════════════════════════════════════════════════════════
// NOTE ON SHA-256 CHECKSUM MISMATCH WITH OFFICIAL FILES
// ══════════════════════════════════════════════════════════════════════════════
//
// Even with all the above fixes, the SHA-256 checksums of our generated files
// will NOT match gc-era.gnosiscoredevs.io/checksums.txt. The reason is snappy
// compression: Go's snappy library and Rust's `snap` crate produce different
// compressed byte sequences for the same input.
//
// Two specific differences observed when comparing against official files:
//
//   a) Chunk type for small data (e.g., 3-byte empty body rlp([],[]) = c2 c0 c0):
//      Official (Go):  compressed chunk  type=0x00, 9 bytes
//      Ours (Rust):    uncompressed chunk type=0x01, 7 bytes
//      The Rust encoder correctly avoids compressing data that would expand;
//      Go always emits compressed chunks regardless of size.
//
//   b) Compressed bytes for larger data (e.g., block bodies with transactions):
//      Both use compressed chunks (type=0x00) and produce the same CRC32C
//      (same uncompressed content), but the compressed byte sequences differ
//      because the two implementations use different hash-table parameters
//      for match-finding.
//
// The accumulator hash, all block hashes, and all state roots match the
// official files exactly. The difference is purely cosmetic (valid snappy
// framing in both cases). The checksums.txt we generate is self-consistent
// with our own files and works correctly for local import.

use alloy_consensus::{BlockHeader, RlpEncodableReceipt, TxReceipt};
use alloy_primitives::Sealable;
use alloy_primitives::{BlockNumber, B256, U256};
use clap::{Args, Parser};
use eyre::{eyre, Result};
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{AccessRights, CliNodeTypes, Environment, EnvironmentArgs};
use reth_era::{
    common::file_ops::EraFileId,
    e2s::{
        file::E2StoreWriter,
        types::{Entry, IndexEntry},
    },
    era1::types::{
        execution::{
            CompressedBody, CompressedHeader, CompressedReceipts, ACCUMULATOR, COMPRESSED_BODY,
            COMPRESSED_HEADER, COMPRESSED_RECEIPTS, MAX_BLOCKS_PER_ERA1, TOTAL_DIFFICULTY,
        },
        group::{BlockIndex, Era1Id},
    },
};
use reth_era_utils::{calculate_td_by_number, ExportConfig};
use reth_fs_util as fs;
use reth_primitives_traits::Block;
use reth_provider::DatabaseProviderFactory;
use reth_storage_api::{BlockNumReader, BlockReader, HeaderProvider, ReceiptProvider};
use sha2::{Digest, Sha256};
use std::{io, path::PathBuf, sync::Arc, time::Instant};
use tracing::{info, warn};

/// Size of an e2store entry header: 2-byte type + 4-byte length + 2-byte reserved
const ENTRY_HEADER_SIZE: i64 = 8;
/// Version entry is header-only (no data payload)
const VERSION_ENTRY_SIZE: i64 = ENTRY_HEADER_SIZE;
/// Accumulator entry: 8-byte header + 32-byte hash
const ACCUMULATOR_ENTRY_SIZE: i64 = ENTRY_HEADER_SIZE + 32;
/// TotalDifficulty entry: 8-byte header + 32-byte LE value
const TD_ENTRY_SIZE: i64 = ENTRY_HEADER_SIZE + 32;

/// Exports block data to ERA1 files with correct receipt encoding.
///
/// Unlike the upstream `export-era`, this command:
/// - Encodes receipts as `ReceiptWithBloom` (standard Ethereum wire format)
/// - Computes the correct SSZ accumulator (hash_tree_root of header records)
/// - Uses the accumulator hash in the filename (matching gc-era.gnosiscoredevs.io)
/// - Generates `checksums.txt` (SHA256, 0x-prefixed) and `accumulators.txt` (SSZ hash, 0x-prefixed)
#[derive(Debug, Parser)]
pub struct ExportEraCommand<C: ChainSpecParser> {
    #[command(flatten)]
    env: EnvironmentArgs<C>,

    #[clap(flatten)]
    export: ExportArgs,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Optional first block number to export from the db.
    /// It is by default 0.
    #[arg(long, value_name = "first-block-number", verbatim_doc_comment)]
    first_block_number: Option<u64>,
    /// Optional last block number to export from the db.
    /// It is by default 8191.
    #[arg(long, value_name = "last-block-number", verbatim_doc_comment)]
    last_block_number: Option<u64>,
    /// The maximum number of blocks per file, it can help you to decrease the size of the files.
    /// Must be less than or equal to 8192.
    #[arg(long, value_name = "max-blocks-per-file", verbatim_doc_comment)]
    max_blocks_per_file: Option<u64>,
    /// The directory path where to export era1 files.
    /// The block data are read from the database.
    #[arg(long, value_name = "EXPORT_ERA1_PATH", verbatim_doc_comment)]
    path: Option<PathBuf>,
}

impl<C: ChainSpecParser<ChainSpec: EthChainSpec + EthereumHardforks>> ExportEraCommand<C> {
    /// Execute `export-era` command
    pub async fn execute<N>(self) -> eyre::Result<()>
    where
        N: CliNodeTypes<ChainSpec = C::ChainSpec>,
    {
        let Environment {
            provider_factory, ..
        } = self.env.init::<N>(AccessRights::RO)?;

        let data_dir = match &self.export.path {
            Some(path) => path.clone(),
            None => self
                .env
                .datadir
                .clone()
                .resolve_datadir(self.env.chain.chain())
                .data_dir()
                .join("era1-export"),
        };

        let network_name = era1_network_name(self.env.chain.chain_id());

        let export_config = ExportConfig {
            network: network_name.to_string(),
            first_block_number: self.export.first_block_number.unwrap_or(0),
            last_block_number: self
                .export
                .last_block_number
                .unwrap_or(MAX_BLOCKS_PER_ERA1 as u64 - 1),
            max_blocks_per_file: self
                .export
                .max_blocks_per_file
                .unwrap_or(MAX_BLOCKS_PER_ERA1 as u64),
            dir: data_dir,
        };

        export_config.validate()?;

        info!(
            target: "reth::cli",
            "Starting ERA1 block export: blocks {}-{} to {}",
            export_config.first_block_number,
            export_config.last_block_number,
            export_config.dir.display()
        );

        let provider = provider_factory.database_provider_ro()?;

        let exported_files = export_gnosis(&provider, &export_config)?;

        info!(
            target: "reth::cli",
            "Successfully exported {} ERA1 files to {}",
            exported_files.len(),
            export_config.dir.display()
        );

        Ok(())
    }
}

impl<C: ChainSpecParser> ExportEraCommand<C> {
    /// Returns the underlying chain being used to run this command
    pub fn chain_spec(&self) -> Option<&Arc<C::ChainSpec>> {
        Some(&self.env.chain)
    }
}

/// Maps chain ID to the ERA1 network name used in filenames.
///
/// The reth chain string for Gnosis is "xdai" (historical name), but the official
/// gnosis era files use "gnosis" as the network name.
fn era1_network_name(chain_id: u64) -> &'static str {
    match chain_id {
        100 => "gnosis",   // Gnosis mainnet
        10200 => "chiado", // Gnosis Chiado testnet
        _ => "unknown",
    }
}

/// Exports blocks to ERA1 files with correct receipt encoding and accumulator computation.
///
/// Writes to the directory specified in `config`:
/// - `{network}-{NNNNN}-{acc_hash}.era1` files (one per chunk)
/// - `checksums.txt` with 0x-prefixed SHA256 hash per file (for `read_dir` import)
/// - `accumulators.txt` with 0x-prefixed SSZ accumulator hash per file
pub fn export_gnosis<P>(provider: &P, config: &ExportConfig) -> Result<Vec<PathBuf>>
where
    P: BlockReader,
    <P as HeaderProvider>::Header: Sealable,
    <P as ReceiptProvider>::Receipt: TxReceipt + RlpEncodableReceipt,
{
    config.validate()?;
    info!(
        "Exporting blockchain history from block {} to {} with this max of blocks per file of {}",
        config.first_block_number, config.last_block_number, config.max_blocks_per_file
    );

    let last_block_number = determine_export_range(provider, config)?;

    info!(
        target: "era::history::export",
        first = config.first_block_number,
        last = last_block_number,
        max_blocks_per_file = config.max_blocks_per_file,
        "Preparing era1 export data"
    );

    if !config.dir.exists() {
        fs::create_dir_all(&config.dir)
            .map_err(|e| eyre!("Failed to create output directory: {}", e))?;
    }

    let start_time = Instant::now();

    let mut created_files = Vec::new();
    let mut checksums = Vec::new();
    let mut accumulator_hashes = Vec::new();

    let mut total_difficulty = if config.first_block_number > 0 {
        calculate_td_by_number(provider, config.first_block_number - 1)?
    } else {
        U256::ZERO
    };

    for start_block in
        (config.first_block_number..=last_block_number).step_by(config.max_blocks_per_file as usize)
    {
        let end_block = (start_block + config.max_blocks_per_file - 1).min(last_block_number);
        let block_count = (end_block - start_block + 1) as usize;

        info!(
            target: "era::history::export",
            "Processing blocks {start_block} to {end_block} ({block_count} blocks)"
        );

        let headers = provider.headers_range(start_block..=end_block)?;

        // Write to a temp file first; we need the accumulator hash to determine the filename,
        // but the accumulator depends on all block hashes which we only know after processing.
        let temp_path = config
            .dir
            .join(format!("export-{start_block}-{end_block}.era1.tmp"));
        let file = std::fs::File::create(&temp_path)?;
        // Use E2StoreWriter directly so we can write TotalDifficulty in little-endian format
        // (spec-compliant) rather than the big-endian encoding in reth's TotalDifficulty::to_entry().
        let mut writer = E2StoreWriter::new(file);
        writer.write_version()?;

        // Absolute byte offsets of each block's header entry, measured from the start of the file.
        // These are converted to relative (spec-compliant negative) offsets after all blocks are
        // written and the block-index entry position is known.
        let mut abs_offsets = Vec::<i64>::with_capacity(block_count);
        let mut position: i64 = VERSION_ENTRY_SIZE;
        let mut blocks_written = 0;

        // Collect (block_hash, total_difficulty) for SSZ accumulator computation
        let mut block_records: Vec<(B256, U256)> = Vec::with_capacity(block_count);

        for (i, header) in headers.into_iter().enumerate() {
            let expected_block_number = start_block + i as u64;

            // Capture hash and difficulty before header is consumed by compress
            let block_hash = header.hash_slow();
            let block_difficulty = header.difficulty();

            let (compressed_header, compressed_body, compressed_receipts) =
                compress_gnosis_block_data(provider, header, expected_block_number)?;

            total_difficulty += block_difficulty;
            block_records.push((block_hash, total_difficulty));

            let header_entry_size = ENTRY_HEADER_SIZE + compressed_header.data.len() as i64;
            let body_entry_size = ENTRY_HEADER_SIZE + compressed_body.data.len() as i64;
            let receipts_entry_size = ENTRY_HEADER_SIZE + compressed_receipts.data.len() as i64;
            let block_entry_total =
                header_entry_size + body_entry_size + receipts_entry_size + TD_ENTRY_SIZE;

            // Record the absolute file offset of this block's header entry.
            abs_offsets.push(position);
            position += block_entry_total;

            // Write header, body, receipts entries.
            writer.write_entry(&Entry::new(COMPRESSED_HEADER, compressed_header.data))?;
            writer.write_entry(&Entry::new(COMPRESSED_BODY, compressed_body.data))?;
            writer.write_entry(&Entry::new(COMPRESSED_RECEIPTS, compressed_receipts.data))?;

            // Write TotalDifficulty in little-endian (spec-compliant).
            // reth's TotalDifficulty::to_entry() uses big-endian, which is wrong per the ERA1 spec.
            let td_le: [u8; 32] = total_difficulty.to_le_bytes();
            writer.write_entry(&Entry::new(TOTAL_DIFFICULTY, td_le.to_vec()))?;

            blocks_written += 1;
        }

        if blocks_written > 0 {
            // Compute the SSZ accumulator hash from the collected block records
            let accumulator_hash = compute_era1_accumulator(&block_records);

            // Write accumulator entry (must precede block index).
            let acc_entry = Entry::new(ACCUMULATOR, accumulator_hash.to_vec());
            writer.write_entry(&acc_entry)?;

            // The block-index entry header starts immediately after the accumulator.
            // Per the ERA1 spec, block-index offsets are measured from the start of the
            // block-index record (i.e. its entry header), so they are NEGATIVE for blocks
            // that precede the index in the file.
            let block_index_entry_pos: i64 = position + ACCUMULATOR_ENTRY_SIZE;
            let relative_offsets: Vec<i64> = abs_offsets
                .iter()
                .map(|&abs| abs - block_index_entry_pos)
                .collect();

            let block_index = BlockIndex::new(start_block, relative_offsets);
            let block_index_entry = block_index.to_entry();
            writer.write_entry(&block_index_entry)?;
            writer.flush()?;
            drop(writer);

            // Determine final filename using the first 4 bytes of the accumulator hash
            let hash_bytes = [
                accumulator_hash[0],
                accumulator_hash[1],
                accumulator_hash[2],
                accumulator_hash[3],
            ];
            let era1_id = if config.max_blocks_per_file == MAX_BLOCKS_PER_ERA1 as u64 {
                Era1Id::new(&config.network, start_block, block_count as u32).with_hash(hash_bytes)
            } else {
                Era1Id::new(&config.network, start_block, block_count as u32)
                    .with_hash(hash_bytes)
                    .with_era_count()
            };

            let file_path = config.dir.join(era1_id.to_file_name());

            // Remove existing file if present (re-export scenario)
            if file_path.exists() {
                std::fs::remove_file(&file_path)?;
            }
            std::fs::rename(&temp_path, &file_path)?;

            // Compute SHA256 checksum of the final file
            let checksum = sha256_file(&file_path)?;
            checksums.push(format!("0x{}", hex::encode(&checksum)));
            accumulator_hashes.push(format!("0x{}", hex::encode(accumulator_hash.as_slice())));

            info!(
                target: "era::history::export",
                "Wrote ERA1 file: {file_path:?} with {blocks_written} blocks (accumulator: 0x{})",
                hex::encode(&accumulator_hash[..4])
            );
            created_files.push(file_path);
        } else {
            // Clean up empty temp file
            let _ = std::fs::remove_file(&temp_path);
        }
    }

    // Write checksums.txt and accumulators.txt (0x-prefixed, one entry per line)
    // Format matches gc-era.gnosiscoredevs.io/checksums.txt and accumulators.txt
    if !created_files.is_empty() {
        let checksums_path = config.dir.join("checksums.txt");
        std::fs::write(&checksums_path, checksums.join("\n") + "\n")?;
        info!(
            target: "era::history::export",
            "Wrote checksums.txt ({} entries)",
            checksums.len()
        );

        let accumulators_path = config.dir.join("accumulators.txt");
        std::fs::write(&accumulators_path, accumulator_hashes.join("\n") + "\n")?;
        info!(
            target: "era::history::export",
            "Wrote accumulators.txt ({} entries)",
            accumulator_hashes.len()
        );
    }

    info!(
        target: "era::history::export",
        "Successfully wrote {} ERA1 files in {:?}",
        created_files.len(),
        start_time.elapsed()
    );

    Ok(created_files)
}

/// Compresses a single block's data, encoding receipts as `ReceiptWithBloom`.
fn compress_gnosis_block_data<P>(
    provider: &P,
    header: P::Header,
    expected_block_number: BlockNumber,
) -> Result<(CompressedHeader, CompressedBody, CompressedReceipts)>
where
    P: BlockReader,
    <P as HeaderProvider>::Header: Sealable,
    <P as ReceiptProvider>::Receipt: TxReceipt + RlpEncodableReceipt,
{
    let actual_block_number = header.number();

    if expected_block_number != actual_block_number {
        return Err(eyre!(
            "Expected block {expected_block_number}, got {actual_block_number}"
        ));
    }

    let body = provider
        .block_by_number(actual_block_number)?
        .ok_or_else(|| eyre!("Block not found for block {}", actual_block_number))?
        .into_body();

    let receipts = provider
        .receipts_by_block(actual_block_number.into())?
        .ok_or_else(|| eyre!("Receipts not found for block {}", actual_block_number))?;

    let compressed_header = CompressedHeader::from_header(&header)?;
    let compressed_body = CompressedBody::from_body(&body)?;

    // Wrap each receipt with its computed bloom before encoding.
    // This produces the standard Ethereum wire format used in ERA1:
    //   legacy:  rlp([status, cumulative_gas_used, bloom, logs])
    //   typed:   byte(tx_type) || rlp([status, cumulative_gas_used, bloom, logs])
    let receipts_with_bloom: Vec<_> = receipts.into_iter().map(|r| r.into_with_bloom()).collect();
    let compressed_receipts = CompressedReceipts::from_encodable_list(&receipts_with_bloom)
        .map_err(|e| eyre!("Failed to compress receipts: {}", e))?;

    Ok((compressed_header, compressed_body, compressed_receipts))
}

/// Computes the SSZ `hash_tree_root` of `List[HeaderRecord, MAX_BLOCKS_PER_ERA1]`.
///
/// Per the ERA1 spec, each `HeaderRecord` contains `(block_hash: Bytes32, total_difficulty: Uint256)`.
///
/// ```text
/// hash_tree_root(HeaderRecord) = sha256(block_hash || le_bytes32(total_difficulty))
/// hash_tree_root(List)         = mix_in_length(merkle(leaves, capacity=8192), actual_length)
/// mix_in_length(root, n)       = sha256(root || le_bytes32(n))
/// ```
fn compute_era1_accumulator(block_records: &[(B256, U256)]) -> B256 {
    let capacity = MAX_BLOCKS_PER_ERA1; // 8192 = 2^13

    // Compute hash_tree_root for each HeaderRecord leaf
    let mut leaves: Vec<[u8; 32]> = block_records
        .iter()
        .map(|(block_hash, total_difficulty)| {
            // hash_tree_root(HeaderRecord) = sha256(block_hash || le_bytes32(total_difficulty))
            let mut data = [0u8; 64];
            data[..32].copy_from_slice(block_hash.as_slice());
            // SSZ encodes uint256 as 32 bytes little-endian
            let td_le = total_difficulty.to_le_bytes::<32>();
            data[32..].copy_from_slice(&td_le);
            sha256_32(&data)
        })
        .collect();

    // Pad to `capacity` with the SSZ zero-hash
    let zero_hash = [0u8; 32];
    while leaves.len() < capacity {
        leaves.push(zero_hash);
    }

    // Build the binary Merkle tree bottom-up
    let merkle_root = merkle_root_of(&leaves);

    // mix_in_length: sha256(merkle_root || le_bytes32(actual_length))
    let mut mix = [0u8; 64];
    mix[..32].copy_from_slice(&merkle_root);
    let length = block_records.len() as u64;
    mix[32..40].copy_from_slice(&length.to_le_bytes()); // rest stays zero (uint256 LE)

    B256::from(sha256_32(&mix))
}

/// Computes the Merkle root over a slice of 32-byte leaves.
/// The slice length must be a power of two.
fn merkle_root_of(leaves: &[[u8; 32]]) -> [u8; 32] {
    debug_assert!(
        leaves.len().is_power_of_two(),
        "leaf count must be a power of two"
    );

    let mut level: Vec<[u8; 32]> = leaves.to_vec();

    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| {
                let mut data = [0u8; 64];
                data[..32].copy_from_slice(&pair[0]);
                data[32..].copy_from_slice(&pair[1]);
                sha256_32(&data)
            })
            .collect();
    }

    level[0]
}

/// SHA256 of `data`, returned as a 32-byte array.
fn sha256_32(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

/// SHA256 of a file, returned as raw bytes.
fn sha256_file(path: &PathBuf) -> Result<Vec<u8>> {
    let mut hasher = Sha256::new();
    let file = std::fs::File::open(path)?;
    let mut reader = io::BufReader::new(file);
    io::copy(&mut reader, &mut hasher)?;
    Ok(hasher.finalize().to_vec())
}

fn determine_export_range<P>(provider: &P, config: &ExportConfig) -> Result<BlockNumber>
where
    P: HeaderProvider + BlockNumReader,
{
    let best_block_number = provider.best_block_number()?;

    let last_block_number = if best_block_number < config.last_block_number {
        warn!(
            "Last block {} is beyond current head {}, setting last = head",
            config.last_block_number, best_block_number
        );

        if let Ok(headers) = provider.headers_range(best_block_number..=config.last_block_number) {
            if let Some(last_header) = headers.last() {
                let highest_block = last_header.number();
                info!(
                    "Found highest available block {} via headers_range",
                    highest_block
                );
                highest_block
            } else {
                warn!(
                    "No headers found in range, using best_block_number {}",
                    best_block_number
                );
                best_block_number
            }
        } else {
            warn!(
                "headers_range failed, using best_block_number {}",
                best_block_number
            );
            best_block_number
        }
    } else {
        config.last_block_number
    };

    Ok(last_block_number)
}
