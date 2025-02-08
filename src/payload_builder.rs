use reth_ethereum_engine_primitives::{
    EthBuiltPayload, EthPayloadAttributes, EthPayloadBuilderAttributes,
};
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_evm::ConfigureEvmFor;
use reth_node_builder::{
    components::PayloadServiceBuilder, BuilderContext, FullNodeTypes, NodeTypesWithEngine,
    PayloadBuilderConfig, PayloadTypes, PrimitivesTy, TxTy,
};
use reth_primitives::EthPrimitives;
use reth_transaction_pool::{PoolTransaction, TransactionPool};
use revm_primitives::Address;

use crate::{evm_config::GnosisEvmConfig, spec::GnosisChainSpec};

/// A basic ethereum payload service.
#[derive(Clone, Default, Debug)]
#[non_exhaustive]
pub struct GnosisPayloadBuilder;

impl GnosisPayloadBuilder {
    /// A helper method initializing [`reth_ethereum_payload_builder::GnosisPayloadBuilder`] with
    /// the given EVM config.
    pub fn build<Types, Node, Evm, Pool>(
        &self,
        evm_config: Evm,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<crate::payload::GnosisPayloadBuilder<Pool, Node::Provider, Evm>>
    where
        Types: NodeTypesWithEngine<ChainSpec = GnosisChainSpec, Primitives = EthPrimitives>,
        Node: FullNodeTypes<Types = Types>,
        Evm: ConfigureEvmFor<PrimitivesTy<Types>>,
        Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<Node::Types>>>
            + Unpin
            + 'static,
        Types::Engine: PayloadTypes<
            BuiltPayload = EthBuiltPayload,
            PayloadAttributes = EthPayloadAttributes,
            PayloadBuilderAttributes = EthPayloadBuilderAttributes,
        >,
    {
        let chain_spec = ctx.chain_spec();
        let block_rewards_contract = chain_spec
            .genesis()
            .config
            .extra_fields
            .get("blockRewardsContract")
            .expect("blockRewardsContract field not found in genesis config");
        let block_rewards_contract: Address =
            serde_json::from_value(block_rewards_contract.clone())
                .expect("failed to parse blockRewardsContract field");

        let conf = ctx.payload_builder_config();
        let gas_limit = chain_spec.genesis.gas_limit;

        Ok(crate::payload::GnosisPayloadBuilder::new(
            ctx.provider().clone(),
            pool,
            evm_config,
            block_rewards_contract,
            EthereumBuilderConfig::new(conf.extra_data_bytes()).with_gas_limit(gas_limit),
        ))
    }
}

impl<Types, Node, Pool> PayloadServiceBuilder<Node, Pool> for GnosisPayloadBuilder
where
    Types: NodeTypesWithEngine<ChainSpec = GnosisChainSpec, Primitives = EthPrimitives>,
    Node: FullNodeTypes<Types = Types>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<Node::Types>>>
        + Unpin
        + 'static,
    Types::Engine: PayloadTypes<
        BuiltPayload = EthBuiltPayload,
        PayloadAttributes = EthPayloadAttributes,
        PayloadBuilderAttributes = EthPayloadBuilderAttributes,
    >,
{
    type PayloadBuilder =
        crate::payload::GnosisPayloadBuilder<Pool, Node::Provider, GnosisEvmConfig>;

    async fn build_payload_builder(
        &self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<Self::PayloadBuilder> {
        self.build(GnosisEvmConfig::new(ctx.chain_spec()), ctx, pool)
    }
}
