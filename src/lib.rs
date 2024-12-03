// use consensus::GnosisBeaconConsensus;
use evm_config::GnosisEvmConfig;
use execute::GnosisExecutionStrategyFactory;
use eyre::eyre;
use payload_builder::GnosisPayloadServiceBuilder;
use reth::{
    api::{AddOnsContext, FullNodeComponents},
    builder::{
        components::{ComponentsBuilder, ConsensusBuilder, ExecutorBuilder},
        node::{FullNodeTypes, NodeTypes, NodeTypesWithEngine},
        rpc::{EngineValidatorBuilder, RpcAddOns},
        BuilderContext, Node, NodeAdapter, NodeComponentsBuilder,
    },
    network::NetworkHandle,
};
use reth_chainspec::ChainSpec;
use reth_engine_primitives::EngineValidator;
use reth_ethereum_consensus::EthBeaconConsensus;
use reth_ethereum_engine_primitives::EthereumEngineValidator;
use reth_node_ethereum::{
    node::{EthereumNetworkBuilder, EthereumPoolBuilder},
    BasicBlockExecutorProvider, EthEngineTypes, EthereumNode,
};
use reth_primitives::EthPrimitives;
use reth_provider::EthStorage;
use reth_rpc::EthApi;
use reth_trie_db::MerklePatriciaTrie;
use std::sync::Arc;

mod errors;
mod evm_config;
pub mod execute;
mod gnosis;
mod payload_builder;

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
        EthereumPoolBuilder,
        GnosisPayloadServiceBuilder,
        EthereumNetworkBuilder,
        GnosisExecutorBuilder,
        GnosisConsensusBuilder,
    >
    where
        // Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec, Primitives = EthPrimitives>>,
        // <Node::Types as NodeTypesWithEngine>::Engine: PayloadTypes<
        //     BuiltPayload = EthBuiltPayload,
        //     PayloadAttributes = EthPayloadAttributes,
        //     PayloadBuilderAttributes = EthPayloadBuilderAttributes,
        // >,
        Node: FullNodeTypes<
            Types: NodeTypesWithEngine<
                Engine = EthEngineTypes,
                ChainSpec = ChainSpec,
                Primitives = EthPrimitives,
            >,
        >,
    {
        EthereumNode::components::<Node>()
            .node_types::<Node>()
            .pool(EthereumPoolBuilder::default())
            .payload(GnosisPayloadServiceBuilder::default())
            .network(EthereumNetworkBuilder::default())
            .executor(GnosisExecutorBuilder::default())
            .consensus(GnosisConsensusBuilder::default())
    }
}

/// Configure the node types
impl NodeTypes for GnosisNode {
    type Primitives = EthPrimitives;
    type ChainSpec = ChainSpec;
    type StateCommitment = MerklePatriciaTrie;
    type Storage = EthStorage;
}

impl NodeTypesWithEngine for GnosisNode {
    type Engine = EthEngineTypes;
}

/// Add-ons w.r.t. gnosis
pub type GnosisAddOns<N> = RpcAddOns<
    N,
    EthApi<
        <N as FullNodeTypes>::Provider,
        <N as FullNodeComponents>::Pool,
        NetworkHandle,
        <N as FullNodeComponents>::Evm,
    >,
    GnosisEngineValidatorBuilder,
>;

impl<N> Node<N> for GnosisNode
where
    N: FullNodeTypes<
        Types: NodeTypesWithEngine<
            Engine = EthEngineTypes,
            ChainSpec = ChainSpec,
            Primitives = EthPrimitives,
            Storage = EthStorage,
        >,
    >,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        EthereumPoolBuilder,
        GnosisPayloadServiceBuilder,
        EthereumNetworkBuilder,
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
    Node: FullNodeTypes<
        Types: NodeTypesWithEngine<
            Engine = EthEngineTypes,
            ChainSpec = ChainSpec,
            Primitives = EthPrimitives,
        >,
    >,
{
    // Must implement ConfigureEvm;
    type EVM = GnosisEvmConfig;
    // Must implement BlockExecutorProvider;
    type Executor = BasicBlockExecutorProvider<GnosisExecutionStrategyFactory>;

    async fn build_evm(
        self,
        ctx: &BuilderContext<Node>,
    ) -> eyre::Result<(Self::EVM, Self::Executor)> {
        let chain_spec = ctx.chain_spec();
        let collector_address = ctx
            .config()
            .chain
            .genesis()
            .config
            .extra_fields
            .get("eip1559collector")
            .ok_or(eyre!("no eip1559collector field"))?;

        let evm_config = GnosisEvmConfig::new(
            serde_json::from_value(collector_address.clone())?,
            chain_spec.clone(),
        );
        let strategy_factory =
            GnosisExecutionStrategyFactory::new(ctx.chain_spec(), evm_config.clone())?;
        let executor = BasicBlockExecutorProvider::new(strategy_factory);

        Ok((evm_config, executor))
    }
}

/// A basic Gnosis consensus builder.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct GnosisConsensusBuilder;

impl<Node> ConsensusBuilder<Node> for GnosisConsensusBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec, Primitives = EthPrimitives>>,
{
    type Consensus = Arc<dyn reth_consensus::FullConsensus>;

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
    Types: NodeTypesWithEngine<ChainSpec = ChainSpec>,
    Node: FullNodeComponents<Types = Types>,
    EthereumEngineValidator: EngineValidator<Types::Engine>,
{
    type Validator = EthereumEngineValidator;

    async fn build(self, ctx: &AddOnsContext<'_, Node>) -> eyre::Result<Self::Validator> {
        Ok(EthereumEngineValidator::new(ctx.config.chain.clone()))
    }
}
