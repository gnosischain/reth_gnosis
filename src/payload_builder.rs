use reth_ethereum_engine_primitives::{EthPayloadAttributes, EthPayloadBuilderAttributes};
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_evm::ConfigureEvm;
use reth_node_builder::{
    components::PayloadBuilderBuilder, BuilderContext, FullNodeTypes, NodeTypes, PayloadTypes,
    PrimitivesTy, TxTy,
};
use reth_transaction_pool::{PoolTransaction, TransactionPool};

use crate::{
    evm_config::GnosisEvmConfig, payload::GnosisBuiltPayload, primitives::GnosisNodePrimitives,
    spec::gnosis_spec::GnosisChainSpec,
};

/// A basic ethereum payload service.
#[derive(Clone, Default, Debug)]
#[non_exhaustive]
pub struct GnosisPayloadBuilder;

impl GnosisPayloadBuilder {
    /// A helper method initializing [`crate::payload::GnosisPayloadBuilder`] with
    /// the given EVM config.
    pub fn build<Types, Node, Evm, Pool>(
        &self,
        evm_config: Evm,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<crate::payload::GnosisPayloadBuilder<Pool, Node::Provider, Evm>>
    where
        Types: NodeTypes<
            ChainSpec = GnosisChainSpec,
            Primitives = GnosisNodePrimitives,
            Payload: PayloadTypes<
                BuiltPayload = GnosisBuiltPayload,
                PayloadAttributes = EthPayloadAttributes,
                PayloadBuilderAttributes = EthPayloadBuilderAttributes,
            >,
        >,
        Node: FullNodeTypes<Types = Types>,
        Evm: ConfigureEvm<Primitives = PrimitivesTy<Types>>,
        Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<Node::Types>>>
            + Unpin
            + 'static,
    {
        let chain_spec = ctx.chain_spec();

        // let conf = ctx.payload_builder_config();
        let gas_limit = chain_spec.genesis.gas_limit;

        Ok(crate::payload::GnosisPayloadBuilder::new(
            ctx.provider().clone(),
            pool,
            evm_config,
            EthereumBuilderConfig::new().with_gas_limit(gas_limit),
        ))
    }
}

impl<Types, Node, Pool> PayloadBuilderBuilder<Node, Pool, GnosisEvmConfig> for GnosisPayloadBuilder
where
    Types: NodeTypes<
        ChainSpec = GnosisChainSpec,
        Primitives = GnosisNodePrimitives,
        Payload: PayloadTypes<
            BuiltPayload = GnosisBuiltPayload,
            PayloadAttributes = EthPayloadAttributes,
            PayloadBuilderAttributes = EthPayloadBuilderAttributes,
        >,
    >,
    Node: FullNodeTypes<Types = Types>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<Node::Types>>>
        + Unpin
        + 'static,
{
    type PayloadBuilder =
        crate::payload::GnosisPayloadBuilder<Pool, Node::Provider, GnosisEvmConfig>;

    async fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm_config: GnosisEvmConfig,
    ) -> eyre::Result<Self::PayloadBuilder> {
        self.build(evm_config, ctx, pool)
    }
}
