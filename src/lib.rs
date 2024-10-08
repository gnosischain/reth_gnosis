use consensus::GnosisBeaconConsensus;
use evm_config::GnosisEvmConfig;
use execute::GnosisExecutorProvider;
use eyre::eyre;
// use gnosis::SYSTEM_ADDRESS;
use payload_builder::GnosisPayloadServiceBuilder;
use reth::{
    api::{FullNodeComponents, NodeAddOns},
    builder::{
        components::{
            ComponentsBuilder, ConsensusBuilder, EngineValidatorBuilder, ExecutorBuilder,
        },
        node::{FullNodeTypes, NodeTypes, NodeTypesWithEngine},
        BuilderContext, Node,
    },
    network::NetworkHandle,
    rpc::eth::EthApi,
};
use reth_chainspec::ChainSpec;
use reth_engine_primitives::EngineValidator;
use reth_ethereum_engine_primitives::EthereumEngineValidator;
use reth_node_ethereum::{
    node::{EthereumEngineValidatorBuilder, EthereumNetworkBuilder, EthereumPoolBuilder},
    EthEngineTypes, EthereumNode,
};
use std::sync::Arc;

mod consensus;
mod errors;
mod ethereum;
mod evm_config;
mod execute;
mod gnosis;
mod payload_builder;
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
        EthereumPoolBuilder,
        GnosisPayloadServiceBuilder,
        EthereumNetworkBuilder,
        GnosisExecutorBuilder,
        GnosisConsensusBuilder,
        EthereumEngineValidatorBuilder,
    >
    where
        Node: FullNodeTypes<
            Types: NodeTypesWithEngine<Engine = EthEngineTypes, ChainSpec = ChainSpec>,
        >,
        // EthereumEngineValidatorBuilder: EngineValidatorBuilder<Node>
    {
        EthereumNode::components::<Node>()
            .node_types::<Node>()
            .pool(EthereumPoolBuilder::default())
            .payload(GnosisPayloadServiceBuilder::default())
            .network(EthereumNetworkBuilder::default())
            .executor(GnosisExecutorBuilder::default())
            .consensus(GnosisConsensusBuilder::default())
            .engine_validator(EthereumEngineValidatorBuilder::default())

        // ComponentsBuilder::default()
        //     .node_types::<Node>()
        //     .pool(EthereumPoolBuilder::default())
    }
}

/// Configure the node types
impl NodeTypes for GnosisNode {
    type Primitives = ();
    type ChainSpec = ChainSpec;
}

impl NodeTypesWithEngine for GnosisNode {
    type Engine = EthEngineTypes;
}

/// Add-ons w.r.t. l1 ethereum.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GnosisAddOns;

impl<N: FullNodeComponents> NodeAddOns<N> for GnosisAddOns {
    type EthApi = EthApi<N::Provider, N::Pool, NetworkHandle, N::Evm>;
}

impl<Types, N> Node<N> for GnosisNode
where
    Types: NodeTypesWithEngine<Engine = EthEngineTypes, ChainSpec = ChainSpec>,
    N: FullNodeTypes<Types = Types>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        EthereumPoolBuilder,
        GnosisPayloadServiceBuilder,
        EthereumNetworkBuilder,
        GnosisExecutorBuilder,
        GnosisConsensusBuilder,
        EthereumEngineValidatorBuilder,
    >;

    type AddOns = GnosisAddOns;

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
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec>>,
{
    // Must implement ConfigureEvm;
    type EVM = GnosisEvmConfig;
    // Must implement BlockExecutorProvider;
    type Executor = GnosisExecutorProvider<Self::EVM>;

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
        let executor = GnosisExecutorProvider::new(chain_spec, evm_config.clone())?;

        Ok((evm_config, executor))
    }
}

/// A basic Gnosis consensus builder.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct GnosisConsensusBuilder;

impl<Node> ConsensusBuilder<Node> for GnosisConsensusBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec>>,
{
    type Consensus = Arc<dyn reth_consensus::Consensus>;

    async fn build_consensus(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Consensus> {
        if ctx.is_dev() {
            Ok(Arc::new(reth_auto_seal_consensus::AutoSealConsensus::new(
                ctx.chain_spec(),
            )))
        } else {
            Ok(Arc::new(GnosisBeaconConsensus::new(ctx.chain_spec())))
        }
    }
}

/// Builder for [`GnosisEngineValidator`].
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct GnosisEngineValidatorBuilder;

impl<Node, Types> EngineValidatorBuilder<Node> for GnosisEngineValidatorBuilder
where
    Types: NodeTypesWithEngine<ChainSpec = ChainSpec>,
    Node: FullNodeTypes<Types = Types>,
    EthereumEngineValidator: EngineValidator<Types::Engine>,
{
    type Validator = EthereumEngineValidator;

    async fn build_validator(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Validator> {
        Ok(EthereumEngineValidator::new(ctx.chain_spec()))
    }
}
