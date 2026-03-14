// use consensus::GnosisBeaconConsensus;
use evm_config::GnosisEvmConfig;
use gnosis_primitives::header::GnosisHeader;
use network::GnosisNetworkBuilder;
use payload_builder::GnosisPayloadBuilder;
use pool::GnosisPoolBuilder;
use reth::api::{AddOnsContext, FullNodeComponents};
use reth_node_builder::rpc::RpcContext;
use reth_rpc::eth::EthApiTypes;
use reth_consensus::FullConsensus;
use reth_engine_local::LocalPayloadAttributesBuilder;
use reth_ethereum_consensus::EthBeaconConsensus;
use reth_ethereum_engine_primitives::{EthPayloadAttributes, EthPayloadBuilderAttributes};
use jsonrpsee::Methods;
use reth_node_builder::{
    components::{
        BasicPayloadServiceBuilder, ComponentsBuilder, ConsensusBuilder, ExecutorBuilder,
    },
    rpc::{PayloadValidatorBuilder, RpcAddOns},
    BuilderContext, DebugNode, FullNodeTypes, Node, NodeAdapter, NodeTypes,
    PayloadAttributesBuilder, PayloadTypes,
};
use reth_node_ethereum::EthereumEthApiBuilder;
use reth_provider::{
    BlockHashReader, BlockNumReader, EthStorage, HeaderProvider, StateProviderFactory,
};
use spec::gnosis_spec::GnosisChainSpec;
use std::sync::Arc;

use crate::{
    arb_simulation::{ArbitrageSimulationApiServer, ArbitrageSimulationImpl},
    engine::{GnosisEngineTypes, GnosisEngineValidator},
    fork_simulation::{ForkSimulationApiServer, ForkSimulationImpl},
    payload::GnosisBuiltPayload,
    primitives::{
        block::{BlockBody, GnosisBlock, TransactionSigned},
        GnosisNodePrimitives,
    },
    rpc::GnosisNetwork,
};

mod arb_simulation;
mod blobs;
pub mod block;
mod build;
mod fork_simulation;

/// Register eth_forkSyncStatus, eth_callAtBlock, eth_callScriptAtBlock into RPC modules.
fn register_fork_simulation_rpc<Node, EthApi>(
    ctx: RpcContext<'_, Node, EthApi>,
) -> eyre::Result<()>
where
    Node: FullNodeComponents<Evm = GnosisEvmConfig>,
    EthApi: EthApiTypes,
    Node::Provider: BlockNumReader
        + BlockHashReader
        + HeaderProvider<Header = GnosisHeader>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    let provider = ctx.node().provider().clone();
    let evm_config = ctx.node().evm_config().clone();
    let fork_sim = ForkSimulationImpl::new(provider.clone(), evm_config.clone());
    let arb_sim = ArbitrageSimulationImpl::new(provider, evm_config);
    let mut methods = Methods::new();
    methods.merge(fork_sim.into_rpc())?;
    methods.merge(arb_sim.into_rpc())?;
    ctx.modules.merge_configured(methods)?;
    Ok(())
}
pub mod cli;
pub mod consts;
mod engine;
mod errors;
pub mod evm;
pub mod evm_config;
pub mod gnosis;
pub mod initialize;
mod network;
mod payload;
mod payload_builder;
mod pool;
mod primitives;
mod rpc;
pub mod spec;
mod testing;

#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Gnosis")]
pub struct GnosisArgs {
    /// Sample arg to test
    #[arg(long = "gnosis.sample-arg", value_name = "SAMPLE_ARG")]
    pub sample_arg: Option<String>,
}

/// Type configuration for a regular Gnosis node.
#[derive(Debug, Default, Clone)]
pub struct GnosisNode {
    /// Additional Gnosis args
    pub args: GnosisArgs,
}

impl GnosisNode {
    pub const fn new() -> Self {
        let args = GnosisArgs { sample_arg: None };
        Self { args }
    }

    /// Returns the components for the given [GnosisArgs].
    pub fn components<Node>(
        _args: &GnosisArgs,
    ) -> ComponentsBuilder<
        Node,
        GnosisPoolBuilder,
        BasicPayloadServiceBuilder<GnosisPayloadBuilder>,
        GnosisNetworkBuilder,
        GnosisExecutorBuilder,
        GnosisConsensusBuilder,
    >
    where
        Node: FullNodeTypes<
            Types: NodeTypes<
                ChainSpec = GnosisChainSpec,
                Primitives = GnosisNodePrimitives,
                Payload: PayloadTypes<
                    BuiltPayload = GnosisBuiltPayload,
                    PayloadAttributes = EthPayloadAttributes,
                    PayloadBuilderAttributes = EthPayloadBuilderAttributes,
                >,
            >,
        >,
    {
        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(GnosisPoolBuilder::default())
            .executor(GnosisExecutorBuilder::default())
            .payload(BasicPayloadServiceBuilder::default())
            .network(GnosisNetworkBuilder::default())
            .consensus(GnosisConsensusBuilder::default())
    }
}

/// Configure the node types
impl NodeTypes for GnosisNode {
    type Primitives = GnosisNodePrimitives;
    type ChainSpec = GnosisChainSpec;
    type Storage = EthStorage<TransactionSigned, GnosisHeader>;
    type Payload = GnosisEngineTypes;
}

impl<N: FullNodeComponents<Types = Self>> DebugNode<N> for GnosisNode {
    type RpcBlock = alloy_rpc_types_eth::Block;

    fn rpc_to_primitive_block(rpc_block: Self::RpcBlock) -> GnosisBlock {
        let block: reth_ethereum_primitives::Block =
            rpc_block.into_consensus().convert_transactions();
        GnosisBlock {
            header: GnosisHeader::from(block.header),
            body: BlockBody {
                transactions: block.body.transactions,
                ommers: block
                    .body
                    .ommers
                    .into_iter()
                    .map(GnosisHeader::from)
                    .collect(),
                withdrawals: block.body.withdrawals,
            },
        }
    }

    fn local_payload_attributes_builder(
        chain_spec: &Self::ChainSpec,
    ) -> impl PayloadAttributesBuilder<<Self::Payload as PayloadTypes>::PayloadAttributes, GnosisHeader>
    {
        LocalPayloadAttributesBuilder::new(Arc::new(chain_spec.clone()))
    }
}

/// Add-ons w.r.t. gnosis
pub type GnosisAddOns<N> =
    RpcAddOns<N, EthereumEthApiBuilder<GnosisNetwork>, GnosisEngineValidatorBuilder>;

impl<N> Node<N> for GnosisNode
where
    N: FullNodeTypes<Types = Self>,
    <N as FullNodeTypes>::Provider: BlockNumReader
        + BlockHashReader
        + HeaderProvider<Header = GnosisHeader>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        GnosisPoolBuilder,
        BasicPayloadServiceBuilder<GnosisPayloadBuilder>,
        GnosisNetworkBuilder,
        GnosisExecutorBuilder,
        GnosisConsensusBuilder,
    >;

    type AddOns = GnosisAddOns<NodeAdapter<N>>;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        let Self { args } = self;
        Self::components(args)
    }

    fn add_ons(&self) -> Self::AddOns {
        GnosisAddOns::default().extend_rpc_modules(register_fork_simulation_rpc)
    }
}

/// A regular Gnosis evm and executor builder.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct GnosisExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for GnosisExecutorBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypes<ChainSpec = GnosisChainSpec, Primitives = GnosisNodePrimitives>,
        Provider: HeaderProvider<Header = GnosisHeader> + std::fmt::Debug + Clone + Unpin + 'static,
    >,
{
    type EVM = GnosisEvmConfig;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        let evm_config = GnosisEvmConfig::new(ctx.chain_spec(), ctx.provider().clone());

        Ok(evm_config)
    }
}

/// A basic Gnosis consensus builder.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct GnosisConsensusBuilder;

impl<Node> ConsensusBuilder<Node> for GnosisConsensusBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypes<ChainSpec = GnosisChainSpec, Primitives = GnosisNodePrimitives>,
    >,
{
    type Consensus = Arc<dyn FullConsensus<GnosisNodePrimitives>>;

    async fn build_consensus(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Consensus> {
        Ok(Arc::new(EthBeaconConsensus::new(ctx.chain_spec())))
    }
}

/// Builder for GnosisEngineValidator.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct GnosisEngineValidatorBuilder;

impl<Node, Types> PayloadValidatorBuilder<Node> for GnosisEngineValidatorBuilder
where
    Types: NodeTypes<
        Payload = GnosisEngineTypes,
        ChainSpec = GnosisChainSpec,
        Primitives = GnosisNodePrimitives,
    >,
    Node: FullNodeComponents<Types = Types>,
{
    type Validator = GnosisEngineValidator;

    async fn build(self, ctx: &AddOnsContext<'_, Node>) -> eyre::Result<Self::Validator> {
        Ok(GnosisEngineValidator::new(ctx.config.chain.clone()))
    }
}
