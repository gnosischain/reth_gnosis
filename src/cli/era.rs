// Bulk is from https://github.com/paradigmxyz/reth/blob/main/crates/era-utils/src/history.rs
// Includes Gnosis-specific modifications:
// Reth doesn't import receipts because it executes the blocks to get it
// reth_gnosis imports the receipts directly from the ERA files

use alloy_consensus::{Eip658Value, ReceiptEnvelope, ReceiptWithBloom};
use alloy_primitives::{BlockHash, BlockNumber, TxNumber};
use alloy_rlp::Decodable;
use futures_util::{Stream, StreamExt};
use reth_db::transaction::DbTxMut;
use reth_db_api::table::Value;
use reth_era::{
    common::{decode::DecodeCompressedRlp, file_ops::StreamReader},
    e2s::error::E2sError,
    era1::{file::Era1Reader, types::execution::BlockTuple},
    ere::{file::EreReader, types::execution::BlockTuple as EreBlockTuple},
};
use reth_era_downloader::EraMeta;
use reth_era_utils::{build_index, open, save_stage_checkpoints};
use reth_ethereum_primitives::Receipt;
use reth_etl::Collector;
use reth_primitives_traits::{Block, FullBlockBody, FullBlockHeader, NodePrimitives};
use reth_provider::{
    providers::StaticFileProviderRWRefMut, BlockBodyIndicesProvider, BlockWriter, ProviderError,
    StateWriter, StaticFileProviderFactory, StaticFileSegment, StaticFileWriter,
};
use reth_storage_api::{
    DBProvider, DatabaseProviderFactory, NodePrimitivesProvider, StageCheckpointWriter,
};
use std::{
    error::Error,
    fmt::{Display, Formatter},
    ops::{Bound, RangeBounds},
    sync::mpsc,
};

const ERA_STEP: u64 = 8192;

/// Imports blocks from `downloader` using `provider`.
///
/// Returns current block height.
pub fn import<S, Downloader, Era, PF, B, BB, BH>(
    mut downloader: Downloader,
    provider_factory: &PF,
    hash_collector: &mut Collector<BlockHash, BlockNumber>,
    max_height: Option<u64>,
) -> eyre::Result<BlockNumber>
where
    B: Block<Header = BH, Body = BB>,
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<
        Transaction = <<<PF as DatabaseProviderFactory>::ProviderRW as NodePrimitivesProvider>::Primitives as NodePrimitives>::SignedTx,
        OmmerHeader = BH,
    >,
    S: EraBlocks<BH, BB>,
    Downloader: Stream<Item = eyre::Result<Era>> + Send + 'static + Unpin,
    Era: EraMeta + Send + 'static,
    PF: DatabaseProviderFactory<
        ProviderRW: BlockWriter<Block = B>
            + DBProvider
            + StaticFileProviderFactory<Primitives: NodePrimitives<Block = B, BlockHeader = BH, BlockBody = BB, Receipt = Receipt>>
            + StateWriter<Receipt = Receipt>
            + BlockBodyIndicesProvider
            + StageCheckpointWriter,
    > + StaticFileProviderFactory<Primitives = <<PF as DatabaseProviderFactory>::ProviderRW as NodePrimitivesProvider>::Primitives>,
{
    let (tx, rx) = mpsc::channel();

    // Handle IO-bound async download in a background tokio task
    tokio::spawn(async move {
        while let Some(file) = downloader.next().await {
            tx.send(Some(file))?;
        }
        tx.send(None)
    });

    let static_file_provider = provider_factory.static_file_provider();

    // Consistency check of expected headers in static files vs DB is done on provider::sync_gap
    // when poll_execute_ready is polled.
    let mut height = static_file_provider
        .get_highest_static_file_block(StaticFileSegment::Headers)
        .unwrap_or_default();

    while let Some(meta) = rx.recv()? {
        let receipt_height = static_file_provider
            .get_highest_static_file_tx(StaticFileSegment::Receipts)
            .unwrap_or_default();
        println!("Receipt height: {receipt_height}");

        let from = height;
        let provider = provider_factory.database_provider_rw()?;

        let mut range = height..=(height + ERA_STEP);
        let mut stop = false;
        if let Some(max_height) = max_height {
            if range.end() > &max_height {
                range = height..=max_height;
                stop = true;
            }
        }

        dbg!("Importing {:?}", &range);

        height = process::<S, _, _, _, _, _>(
            &meta?,
            &mut static_file_provider.latest_writer(StaticFileSegment::Headers)?,
            &mut static_file_provider.latest_writer(StaticFileSegment::Receipts)?,
            &provider,
            hash_collector,
            range,
        )?;

        save_stage_checkpoints(&provider, from, height, height, height)?;

        provider.commit()?;

        if stop {
            break;
        }
    }

    let provider = provider_factory.database_provider_rw()?;

    build_index(&provider, hash_collector)?;

    provider.commit()?;

    Ok(height)
}

/// Boxed so [`ProcessIter`] is independent of which ERA format produced the blocks.
type ProcessInnerIter<BH, BB> = Box<dyn Iterator<Item = eyre::Result<(BH, BB, ReceiptsType)>>>;

/// An iterator that wraps era file extraction. After the final item [`EraMeta::mark_as_processed`]
/// is called to ensure proper cleanup.
pub struct ProcessIter<'a, Era: ?Sized, BH, BB>
where
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<OmmerHeader = BH>,
{
    iter: ProcessInnerIter<BH, BB>,
    era: &'a Era,
}

impl<Era: EraMeta + ?Sized, BH, BB> std::fmt::Debug for ProcessIter<'_, Era, BH, BB>
where
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<OmmerHeader = BH>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessIter")
            .field("era", &self.era.path())
            .finish_non_exhaustive()
    }
}

impl<'a, Era: EraMeta + ?Sized, BH, BB> Display for ProcessIter<'a, Era, BH, BB>
where
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<OmmerHeader = BH>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.era.path().to_string_lossy(), f)
    }
}

impl<'a, Era, BH, BB> Iterator for ProcessIter<'a, Era, BH, BB>
where
    Era: EraMeta + ?Sized,
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<OmmerHeader = BH>,
{
    type Item = eyre::Result<(BH, BB, ReceiptsType)>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.iter.next() {
            Some(item) => Some(item),
            None => match self.era.mark_as_processed() {
                Ok(..) => None,
                Err(e) => Some(Err(e)),
            },
        }
    }
}

/// Reads execution `(header, body, receipts)` tuples out of an ERA file.
///
/// The per-format seam of the import pipeline. Unlike upstream's `EraBlockReader`, receipts are
/// mandatory here: reth_gnosis imports them rather than re-executing blocks, so a file without
/// them cannot be used.
pub trait EraBlocks<BH, BB> {
    /// Opens the ERA file at `meta` and iterates its execution blocks.
    fn blocks<M: EraMeta + ?Sized>(meta: &M) -> eyre::Result<ProcessInnerIter<BH, BB>>;
}

/// [`EraBlocks`] for `.era1` files, whose receipts entry is mandatory per spec.
#[derive(Debug)]
pub struct Era1;

impl<BH, BB> EraBlocks<BH, BB> for Era1
where
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<OmmerHeader = BH>,
{
    fn blocks<M: EraMeta + ?Sized>(meta: &M) -> eyre::Result<ProcessInnerIter<BH, BB>> {
        let reader: Era1Reader<std::fs::File> = open(meta)?;
        Ok(Box::new(reader.iter().map(decode)))
    }
}

/// [`EraBlocks`] for `.ere`/`.erae` files, which cover both pre- and post-merge blocks.
#[derive(Debug)]
pub struct Ere;

impl<BH, BB> EraBlocks<BH, BB> for Ere
where
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<OmmerHeader = BH>,
{
    fn blocks<M: EraMeta + ?Sized>(meta: &M) -> eyre::Result<ProcessInnerIter<BH, BB>> {
        let reader: EreReader<std::fs::File> = open(meta)?;
        Ok(Box::new(reader.iter().map(decode_ere)))
    }
}

/// Extracts header, body and receipts from an ERE block tuple.
///
/// ERE stores *slim* receipts, which omit the logs bloom; converting through [`ReceiptEnvelope`]
/// recomputes it from the logs. Receipts are optional per the ERE spec (the `noreceipts` profile
/// omits them), but reth_gnosis needs them, so a block without them is an error.
pub fn decode_ere<BH, BB, E>(
    block: Result<EreBlockTuple, E>,
) -> eyre::Result<(BH, BB, ReceiptsType)>
where
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<OmmerHeader = BH>,
    E: From<E2sError> + Error + Send + Sync + 'static,
{
    let block = block?;
    let header: BH = block.header.decode()?;
    let body: BB = block.body.decode()?;
    let number = header.number();

    let slim = block.receipts.as_ref().ok_or_else(|| {
        eyre::eyre!(
            "block {number} carries no receipts: this ERE file was written without them \
             (`noreceipts` profile), which reth_gnosis cannot import"
        )
    })?;

    let envelopes: Vec<ReceiptEnvelope> = slim
        .decode_receipts()?
        .into_iter()
        .map(Into::into)
        .collect();

    for envelope in &envelopes {
        if matches!(
            alloy_consensus::TxReceipt::status_or_post_state(envelope),
            Eip658Value::PostState(_)
        ) {
            eyre::bail!(
                "block {number} has pre-Byzantium receipts, which commit to a post-state root \
                 rather than a success status and so cannot be represented by this node's \
                 receipt type"
            );
        }
    }

    // Round-trip through the canonical encoding: the only construction path the node's receipt
    // type supports, and it preserves the bloom recomputed above.
    let encoded = alloy_rlp::encode(&envelopes);
    let receipts = ReceiptsType::decode(&mut encoded.as_slice())?;

    Ok((header, body, receipts))
}

/// Extracts block headers and bodies from `meta` and appends them using `writer` and `provider`.
///
/// Adds on to `total_difficulty` and collects hash to height using `hash_collector`.
///
/// Skips all blocks below the [`start_bound`] of `block_numbers` and stops when reaching past the
/// [`end_bound`] or the end of the file.
///
/// Returns last block height.
///
/// [`start_bound`]: RangeBounds::start_bound
/// [`end_bound`]: RangeBounds::end_bound
pub fn process<S, Era, P, B, BB, BH>(
    meta: &Era,
    header_writer: &mut StaticFileProviderRWRefMut<'_, <P as NodePrimitivesProvider>::Primitives>,
    receipts_writer: &mut StaticFileProviderRWRefMut<'_, <P as NodePrimitivesProvider>::Primitives>,
    provider: &P,
    hash_collector: &mut Collector<BlockHash, BlockNumber>,
    block_numbers: impl RangeBounds<BlockNumber>,
) -> eyre::Result<BlockNumber>
where
    B: Block<Header = BH, Body = BB>,
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<
        Transaction = <<P as NodePrimitivesProvider>::Primitives as NodePrimitives>::SignedTx,
        OmmerHeader = BH,
    >,
    Era: EraMeta + ?Sized,
    S: EraBlocks<BH, BB>,
    P: DBProvider<Tx: DbTxMut>
        + NodePrimitivesProvider
        + BlockWriter<Block = B>
        + StateWriter<Receipt = Receipt>
        + BlockBodyIndicesProvider,
    <P as NodePrimitivesProvider>::Primitives:
        NodePrimitives<BlockHeader = BH, BlockBody = BB, Receipt = Receipt>,
{
    let iter = ProcessIter {
        iter: S::blocks(meta)?,
        era: meta,
    };

    process_iter(
        iter,
        header_writer,
        receipts_writer,
        provider,
        hash_collector,
        block_numbers,
    )
}

type ReceiptsType = Vec<ReceiptWithBloom<Receipt>>;

pub fn receipts_to_iter(
    receipts: ReceiptsType,
    starts_from: TxNumber,
) -> impl Iterator<Item = Result<(TxNumber, Receipt), ProviderError>> {
    receipts.into_iter().enumerate().map(move |(i, receipt)| {
        let tx_number = starts_from + i as TxNumber;
        Ok((tx_number, receipt.receipt))
    })
}

/// Extracts a pair of [`FullBlockHeader`] and [`FullBlockBody`] from [`BlockTuple`].
pub fn decode<BH, BB, E>(block: Result<BlockTuple, E>) -> eyre::Result<(BH, BB, ReceiptsType)>
where
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<OmmerHeader = BH>,
    E: From<E2sError> + Error + Send + Sync + 'static,
{
    let block = block?;
    let header: BH = block.header.decode()?;
    let body: BB = block.body.decode()?;
    let receipts: ReceiptsType = block.receipts.decode()?;

    Ok((header, body, receipts))
}

/// Extracts block headers and bodies from `iter` and appends them using `writer` and `provider`.
///
/// Adds on to `total_difficulty` and collects hash to height using `hash_collector`.
///
/// Skips all blocks below the [`start_bound`] of `block_numbers` and stops when reaching past the
/// [`end_bound`] or the end of the file.
///
/// Returns last block height.
///
/// [`start_bound`]: RangeBounds::start_bound
/// [`end_bound`]: RangeBounds::end_bound
pub fn process_iter<P, B, BB, BH>(
    mut iter: impl Iterator<Item = eyre::Result<(BH, BB, ReceiptsType)>>,
    header_writer: &mut StaticFileProviderRWRefMut<'_, <P as NodePrimitivesProvider>::Primitives>,
    receipts_writer: &mut StaticFileProviderRWRefMut<'_, <P as NodePrimitivesProvider>::Primitives>,
    provider: &P,
    hash_collector: &mut Collector<BlockHash, BlockNumber>,
    block_numbers: impl RangeBounds<BlockNumber>,
) -> eyre::Result<BlockNumber>
where
    B: Block<Header = BH, Body = BB>,
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<
        Transaction = <<P as NodePrimitivesProvider>::Primitives as NodePrimitives>::SignedTx,
        OmmerHeader = BH,
    >,
    P: DBProvider<Tx: DbTxMut>
        + NodePrimitivesProvider
        + BlockWriter<Block = B>
        + StateWriter<Receipt = Receipt>
        + BlockBodyIndicesProvider,
    <P as NodePrimitivesProvider>::Primitives:
        NodePrimitives<BlockHeader = BH, BlockBody = BB, Receipt = Receipt>,
{
    let mut last_header_number = match block_numbers.start_bound() {
        Bound::Included(&number) => number,
        Bound::Excluded(&number) => number.saturating_sub(1),
        Bound::Unbounded => 0,
    };
    let target = match block_numbers.end_bound() {
        Bound::Included(&number) => Some(number),
        Bound::Excluded(&number) => Some(number.saturating_add(1)),
        Bound::Unbounded => None,
    };

    for block in &mut iter {
        let (header, body, receipts) = block?;
        let number = header.number();

        if number <= last_header_number {
            continue;
        }
        if let Some(target) = target {
            if number > target {
                break;
            }
        }

        let hash = header.hash_slow();
        last_header_number = number;

        // Append to Headers segment
        header_writer.append_header(&header, &hash)?;

        // Write bodies to database.
        provider.append_block_bodies(vec![(header.number(), Some(&body))])?;

        // GNOSIS-SPECIFIC: Write receipts to static files
        let idx = provider.block_body_indices(number);
        if let Ok(Some(idx)) = idx {
            for (i, receipt) in (idx.first_tx_num()..).zip(receipts) {
                receipts_writer.append_receipt(i, &receipt.receipt)?;
            }
        } else {
            panic!("Failed to get block body indices for block {number}");
        }
        receipts_writer.increment_block(number)?;
        // GNOSIS-SPECIFIC END

        hash_collector.insert(hash, number)?;
    }

    Ok(last_header_number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{Header, TxReceipt, TxType};
    use alloy_primitives::{Address, Bloom, Bytes, Log, LogData, B256};
    use reth_era::ere::types::execution::{
        CompressedBody as EreCompressedBody, CompressedHeader as EreCompressedHeader,
        CompressedSlimReceipts, SlimReceipt,
    };
    use reth_ethereum_primitives::BlockBody;

    fn sample_log() -> Log {
        Log {
            address: Address::repeat_byte(0x11),
            data: LogData::new_unchecked(
                vec![B256::repeat_byte(0x22)],
                Bytes::from_static(b"payload"),
            ),
        }
    }

    fn ere_tuple(number: u64, receipts: Option<&[SlimReceipt]>) -> EreBlockTuple {
        let header = Header {
            number,
            ..Default::default()
        };
        let tuple = EreBlockTuple::new(
            EreCompressedHeader::from_rlp(&alloy_rlp::encode(&header)).unwrap(),
            EreCompressedBody::from_rlp(&alloy_rlp::encode(BlockBody::default())).unwrap(),
        );
        match receipts {
            Some(receipts) => {
                tuple.with_receipts(CompressedSlimReceipts::from_receipts(receipts).unwrap())
            }
            None => tuple,
        }
    }

    /// ERE stores slim receipts without the logs bloom; decoding must recompute it.
    #[test]
    fn decode_ere_recovers_receipts_and_recomputes_bloom() {
        let log = sample_log();
        let slim = SlimReceipt {
            tx_type: TxType::Eip1559,
            status: Eip658Value::Eip658(true),
            cumulative_gas_used: 21_000,
            logs: vec![log.clone()],
        };

        let (header, _body, receipts): (Header, BlockBody, ReceiptsType) =
            decode_ere::<_, _, E2sError>(Ok(ere_tuple(7, Some(&[slim])))).unwrap();

        assert_eq!(header.number, 7);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].receipt.logs(), &[log]);
        assert_eq!(receipts[0].receipt.cumulative_gas_used(), 21_000);
        // The bloom is absent from the slim form, so a zero bloom would mean it was not restored.
        assert_ne!(receipts[0].logs_bloom, Bloom::ZERO);
    }

    /// The `noreceipts` ERE profile omits receipts entirely, which reth_gnosis cannot import.
    #[test]
    fn decode_ere_rejects_file_without_receipts() {
        let err = decode_ere::<Header, BlockBody, E2sError>(Ok(ere_tuple(7, None)))
            .expect_err("a block without receipts must be rejected");
        assert!(
            err.to_string().contains("carries no receipts"),
            "unexpected error: {err}"
        );
    }
}
