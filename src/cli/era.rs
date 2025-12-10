// Bulk is from https://github.com/paradigmxyz/reth/blob/main/crates/era-utils/src/history.rs
// Includes Gnosis-specific modifications:
// Reth doesn't import receipts because it executes the blocks to get it
// reth_gnosis imports the receipts directly from the ERA files

use alloy_consensus::ReceiptWithBloom;
use alloy_primitives::{BlockHash, BlockNumber, TxNumber};
use futures_util::{Stream, StreamExt};
use reth_db::transaction::DbTxMut;
use reth_db_api::table::Value;
use reth_era::{
    e2s_types::E2sError, era1_file::BlockTupleIterator, era_file_ops::StreamReader,
    execution_types::BlockTuple, DecodeCompressed,
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
    io::{Read, Seek},
    iter::Map,
    ops::{Bound, RangeBounds},
    sync::mpsc,
};

const ERA_STEP: u64 = 8192;

/// Imports blocks from `downloader` using `provider`.
///
/// Returns current block height.
pub fn import<Downloader, Era, PF, B, BB, BH>(
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

        height = process(
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

type ProcessInnerIter<R, BH, BB> = Map<
    BlockTupleIterator<R>,
    Box<dyn Fn(Result<BlockTuple, E2sError>) -> eyre::Result<(BH, BB, ReceiptsType)>>,
>;

/// An iterator that wraps era file extraction. After the final item [`EraMeta::mark_as_processed`]
/// is called to ensure proper cleanup.
#[derive(Debug)]
pub struct ProcessIter<'a, Era: ?Sized, R: Read, BH, BB>
where
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<OmmerHeader = BH>,
{
    iter: ProcessInnerIter<R, BH, BB>,
    era: &'a Era,
}

impl<'a, Era: EraMeta + ?Sized, R: Read, BH, BB> Display for ProcessIter<'a, Era, R, BH, BB>
where
    BH: FullBlockHeader + Value,
    BB: FullBlockBody<OmmerHeader = BH>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.era.path().to_string_lossy(), f)
    }
}

impl<'a, Era, R, BH, BB> Iterator for ProcessIter<'a, Era, R, BH, BB>
where
    R: Read + Seek,
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
pub fn process<Era, P, B, BB, BH>(
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
    P: DBProvider<Tx: DbTxMut>
        + NodePrimitivesProvider
        + BlockWriter<Block = B>
        + StateWriter<Receipt = Receipt>
        + BlockBodyIndicesProvider,
    <P as NodePrimitivesProvider>::Primitives:
        NodePrimitives<BlockHeader = BH, BlockBody = BB, Receipt = Receipt>,
{
    let reader = open(meta)?;
    let iter = reader.iter().map(Box::new(decode)
        as Box<dyn Fn(Result<BlockTuple, E2sError>) -> eyre::Result<(BH, BB, ReceiptsType)>>);
    let iter = ProcessIter { iter, era: meta };

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
        provider.append_block_bodies(vec![(header.number(), Some(body))])?;

        // GNOSIS-SPECIFIC: Write receipts to static files
        let idx = provider.block_body_indices(number);
        if let Ok(Some(idx)) = idx {
            let mut i = idx.first_tx_num();
            for receipt in receipts {
                receipts_writer.append_receipt(i, &receipt.receipt)?;
                i += 1;
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
