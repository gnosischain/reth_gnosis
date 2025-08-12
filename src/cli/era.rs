use alloy_primitives::{BlockHash, BlockNumber};
use futures_util::{Stream, StreamExt};
use reth_db_api::{
    table::Value,
};
use reth_era_downloader::EraMeta;
use reth_era_utils::{build_index, process, save_stage_checkpoints};
use reth_etl::Collector;
use reth_primitives_traits::{Block, FullBlockBody, FullBlockHeader, NodePrimitives};
use reth_provider::{
    writer::UnifiedStorageWriter, BlockWriter,
    ProviderError, StaticFileProviderFactory, StaticFileSegment, StaticFileWriter,
};
use reth_storage_api::{
    DBProvider, DatabaseProviderFactory, HeaderProvider,
    NodePrimitivesProvider, StageCheckpointWriter,
};
use std::{
    sync::mpsc
};

const  ERA_STEP: u64 = 8192;

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
            + StaticFileProviderFactory<Primitives: NodePrimitives<Block = B, BlockHeader = BH, BlockBody = BB>>
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

    // Find the latest total difficulty
    let mut td = static_file_provider
        .header_td_by_number(height)?
        .ok_or(ProviderError::TotalDifficultyNotFound(height))?;

    while let Some(meta) = rx.recv()? {
        let from = height;
        let provider = provider_factory.database_provider_rw()?;

        let mut range = height..=(height+ERA_STEP);
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
            &provider,
            hash_collector,
            &mut td,
            range,
        )?;

        save_stage_checkpoints(&provider, from, height, height, height)?;

        UnifiedStorageWriter::commit(provider)?;

        if stop {
            break;
        }
    }

    let provider = provider_factory.database_provider_rw()?;

    build_index(&provider, hash_collector)?;

    UnifiedStorageWriter::commit(provider)?;

    Ok(height)
}