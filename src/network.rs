use reth::{
    api::{FullNodeTypes, NodeTypes, TxTy},
    builder::{components::NetworkBuilder, BuilderContext},
    network::{NetworkHandle, NetworkManager, PeersInfo},
};
use reth_chainspec::EthChainSpec;
use reth_eth_wire_types::{BasicNetworkPrimitives, UnifiedStatus};
use reth_ethereum_primitives::PooledTransactionVariant;
use reth_transaction_pool::{PoolTransaction, TransactionPool};
use tracing::info;

use crate::{primitives::GnosisNodePrimitives, spec::gnosis_spec::GnosisChainSpec};

pub type GnosisNetworkPrimitives =
    BasicNetworkPrimitives<GnosisNodePrimitives, PooledTransactionVariant>;

/// A basic ethereum payload service.
#[derive(Debug, Default, Clone, Copy)]
pub struct GnosisNetworkBuilder {
    // TODO add closure to modify network
}

impl<Node, Pool> NetworkBuilder<Node, Pool> for GnosisNetworkBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypes<ChainSpec = GnosisChainSpec, Primitives = GnosisNodePrimitives>,
    >,
    Pool: TransactionPool<
            Transaction: PoolTransaction<
                Consensus = TxTy<Node::Types>,
                Pooled = PooledTransactionVariant,
            >,
        > + Unpin
        + 'static,
{
    type Network = NetworkHandle<GnosisNetworkPrimitives>;

    async fn build_network(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<NetworkHandle<GnosisNetworkPrimitives>> {
        let mut network_config = ctx.network_config()?;

        let spec = ctx.chain_spec();
        let head = &ctx.head();

        // using actual genesis hash for mainnet and chiado
        let genesis_hash = spec.genesis_hash();

        let status = UnifiedStatus::builder()
            .chain(spec.chain())
            .genesis(genesis_hash)
            .blockhash(head.hash)
            .total_difficulty(Some(head.total_difficulty))
            .forkid(network_config.fork_filter.current())
            .build();
        network_config.status = status;

        let network = NetworkManager::builder(network_config).await?;
        let handle = ctx.start_network(network, pool);
        info!(target: "reth::cli", enode=%handle.local_node_record(), "P2P networking initialized");
        Ok(handle)
    }
}
