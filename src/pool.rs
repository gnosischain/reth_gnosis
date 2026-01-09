// Pool settings in this file have been copied directly from:
// https://github.com/paradigmxyz/reth/blob/74cc561917642e8024e8aeab16a85da554a7db4e/crates/ethereum/node/src/node.rs#L443
// Changes are made to EPOCH_SLOTS (16 slots on Gnosis vs 32 slots on Ethereum)

use std::time::SystemTime;

use alloy_eips::eip7840::BlobParams;
use reth_chainspec::EthChainSpec;
use reth_node_builder::{
    components::{PoolBuilder, TxPoolBuilder},
    node::{FullNodeTypes, NodeTypes},
    BuilderContext,
};
use reth_transaction_pool::{
    blobstore::DiskFileBlobStore, EthTransactionPool, TransactionValidationTaskExecutor,
};
use tracing::{debug, info};

use crate::{primitives::GnosisNodePrimitives, spec::gnosis_spec::GnosisChainSpec};

/// Slots per epoch on Gnosis Chain
const EPOCH_SLOTS: u64 = 16;

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GnosisPoolBuilder {}

impl<Types, Node> PoolBuilder<Node> for GnosisPoolBuilder
where
    Types: NodeTypes<ChainSpec = GnosisChainSpec, Primitives = GnosisNodePrimitives>,
    Node: FullNodeTypes<Types = Types>,
{
    type Pool = EthTransactionPool<Node::Provider, DiskFileBlobStore>;

    async fn build_pool(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Pool> {
        let pool_config = ctx.pool_config();

        let blob_cache_size = if let Some(blob_cache_size) = pool_config.blob_cache_size {
            Some(blob_cache_size)
        } else {
            // get the current blob params for the current timestamp, fallback to default Cancun
            // params
            let current_timestamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)?
                .as_secs();
            let blob_params = ctx
                .chain_spec()
                .blob_params_at_timestamp(current_timestamp)
                .unwrap_or_else(BlobParams::cancun);

            // Derive the blob cache size from the target blob count, to auto scale it by
            // multiplying it with the slot count for 2 epochs
            Some((blob_params.target_blob_count * EPOCH_SLOTS * 2) as u32)
        };

        let blob_store =
            reth_node_builder::components::create_blob_store_with_cache(ctx, blob_cache_size)?;

        let validator = TransactionValidationTaskExecutor::eth_builder(ctx.provider().clone())
            .with_head_timestamp(ctx.head().timestamp)
            .with_max_tx_input_bytes(ctx.config().txpool.max_tx_input_bytes)
            .kzg_settings(ctx.kzg_settings()?)
            .with_local_transactions_config(pool_config.local_transactions_config.clone())
            .set_tx_fee_cap(ctx.config().rpc.rpc_tx_fee_cap)
            .with_max_tx_gas_limit(ctx.config().txpool.max_tx_gas_limit)
            .with_minimum_priority_fee(ctx.config().txpool.minimum_priority_fee)
            .with_additional_tasks(ctx.config().txpool.additional_validation_tasks)
            .build_with_tasks(ctx.task_executor().clone(), blob_store.clone());

        if validator.validator().eip4844() {
            // initializing the KZG settings can be expensive, this should be done upfront so that
            // it doesn't impact the first block or the first gossiped blob transaction, so we
            // initialize this in the background
            let kzg_settings = validator.validator().kzg_settings().clone();
            ctx.task_executor().spawn_blocking(async move {
                let _ = kzg_settings.get();
                debug!(target: "reth::cli", "Initialized KZG settings");
            });
        }

        let transaction_pool = TxPoolBuilder::new(ctx)
            .with_validator(validator)
            .build_and_spawn_maintenance_task(blob_store, pool_config)?;

        info!(target: "reth::cli", "Transaction pool initialized");
        debug!(target: "reth::cli", "Spawned txpool maintenance task");

        Ok(transaction_pool)
    }
}
