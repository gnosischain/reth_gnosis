// use consensus::GnosisBeaconConsensus;
use evm_config::GnosisEvmConfig;
use network::GnosisNetworkBuilder;
use payload_builder::GnosisPayloadBuilder;
use pool::GnosisPoolBuilder;
use reth::api::{AddOnsContext, FullNodeComponents};
use reth_consensus::FullConsensus;
use reth_errors::ConsensusError;
use reth_ethereum_consensus::EthBeaconConsensus;
use reth_ethereum_engine_primitives::{
    EthBuiltPayload, EthPayloadAttributes, EthPayloadBuilderAttributes,
};
use reth_node_builder::{
    components::{
        BasicPayloadServiceBuilder, ComponentsBuilder, ConsensusBuilder, ExecutorBuilder,
    },
    rpc::{EngineValidatorBuilder, RpcAddOns},
    BuilderContext, FullNodeTypes, Node, NodeAdapter, NodeComponentsBuilder, NodeTypes,
    PayloadTypes,
};
use reth_node_ethereum::{EthEngineTypes, EthereumEngineValidator};
use reth_primitives::EthPrimitives;
use reth_provider::EthStorage;
use reth_trie_db::MerklePatriciaTrie;
use spec::gnosis_spec::GnosisChainSpec;
use std::sync::Arc;

mod blobs;
mod block;
mod build;
pub mod cli;
mod errors;
mod evm;
mod evm_config;
mod gnosis;
pub mod initialize;
mod network;
mod payload;
mod payload_builder;
mod pool;
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
                Primitives = EthPrimitives,
                Payload: PayloadTypes<
                    BuiltPayload = EthBuiltPayload,
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
    type Primitives = EthPrimitives;
    type ChainSpec = GnosisChainSpec;
    type StateCommitment = MerklePatriciaTrie;
    type Storage = EthStorage;
    type Payload = EthEngineTypes;
}

/// Add-ons w.r.t. gnosis
pub type GnosisAddOns<N> = RpcAddOns<N, rpc::GnosisEthApiBuilder, GnosisEngineValidatorBuilder>;

impl<N> Node<N> for GnosisNode
where
    N: FullNodeTypes<
        Types: NodeTypes<
            Payload = EthEngineTypes,
            ChainSpec = GnosisChainSpec,
            Primitives = EthPrimitives,
            Storage = EthStorage,
        >,
    >,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        GnosisPoolBuilder,
        BasicPayloadServiceBuilder<GnosisPayloadBuilder>,
        GnosisNetworkBuilder,
        GnosisExecutorBuilder,
        GnosisConsensusBuilder,
    >;

    type AddOns = GnosisAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        let Self { args } = self;
        Self::components(args)
    }

    fn add_ons(&self) -> Self::AddOns {
        GnosisAddOns::default()
    }
}

/// A regular Gnosis evm and executor builder.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct GnosisExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for GnosisExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = GnosisChainSpec, Primitives = EthPrimitives>>,
{
    type EVM = GnosisEvmConfig;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        let evm_config = GnosisEvmConfig::new(ctx.chain_spec());

        Ok(evm_config)
    }
}

/// A basic Gnosis consensus builder.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct GnosisConsensusBuilder;

impl<Node> ConsensusBuilder<Node> for GnosisConsensusBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = GnosisChainSpec, Primitives = EthPrimitives>>,
{
    type Consensus = Arc<dyn FullConsensus<EthPrimitives, Error = ConsensusError>>;

    async fn build_consensus(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Consensus> {
        Ok(Arc::new(EthBeaconConsensus::new(ctx.chain_spec())))
    }
}

/// Builder for [`EthereumEngineValidator`].
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct GnosisEngineValidatorBuilder;

impl<Node, Types> EngineValidatorBuilder<Node> for GnosisEngineValidatorBuilder
where
    Types: NodeTypes<
        Payload = EthEngineTypes,
        ChainSpec = GnosisChainSpec,
        Primitives = EthPrimitives,
    >,
    Node: FullNodeComponents<Types = Types>,
{
    type Validator = EthereumEngineValidator;

    async fn build(self, ctx: &AddOnsContext<'_, Node>) -> eyre::Result<Self::Validator> {
        Ok(EthereumEngineValidator::new(Arc::new(
            ctx.config.chain.clone().as_ref().clone().into(),
        )))
    }
}
