use consensus::GnosisBeaconConsensus;
use evm_config::GnosisEvmConfig;
use execute::GnosisExecutorProvider;
use eyre::eyre;
use reth::{
    api::NodeTypes,
    builder::{
        components::{ComponentsBuilder, ConsensusBuilder, ExecutorBuilder},
        node::FullNodeTypes,
        BuilderContext, Node,
    },
};
use reth_node_ethereum::{
    node::{EthereumNetworkBuilder, EthereumPayloadBuilder, EthereumPoolBuilder},
    EthEngineTypes, EthereumNode,
};
use std::sync::Arc;

mod consensus;
mod ethereum;
mod evm_config;
mod execute;
mod gnosis;

#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Gnosis")]
pub struct GnosisArgs {
    /// Sample arg to test
    #[arg(long = "gnosis.sample-arg", value_name = "SAMPLE_ARG")]
    pub sample_arg: Option<String>,
}

/// Type configuration for a regular Optimism node.
#[derive(Debug, Default, Clone)]
pub struct GnosisNode {
    /// Additional Optimism args
    pub args: GnosisArgs,
}

impl GnosisNode {
    pub const fn new(args: GnosisArgs) -> Self {
        Self { args }
    }

    /// Returns the components for the given [GnosisArgs].
    pub fn components<Node>(
        _args: GnosisArgs,
    ) -> ComponentsBuilder<
        Node,
        EthereumPoolBuilder,
        EthereumPayloadBuilder,
        EthereumNetworkBuilder,
        GnosisExecutorBuilder,
        GnosisConsensusBuilder,
    >
    where
        Node: FullNodeTypes<Engine = EthEngineTypes>,
    {
        EthereumNode::components()
            .executor(GnosisExecutorBuilder::default())
            .consensus(GnosisConsensusBuilder::default())
    }
}

/// Configure the node types
impl NodeTypes for GnosisNode {
    type Primitives = ();
    type Engine = EthEngineTypes;
}

impl<N> Node<N> for GnosisNode
where
    N: FullNodeTypes<Engine = EthEngineTypes>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        EthereumPoolBuilder,
        EthereumPayloadBuilder,
        EthereumNetworkBuilder,
        GnosisExecutorBuilder,
        GnosisConsensusBuilder,
    >;

    fn components_builder(self) -> Self::ComponentsBuilder {
        let Self { args } = self;
        Self::components(args)
    }
}

/// A regular optimism evm and executor builder.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct GnosisExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for GnosisExecutorBuilder
where
    Node: FullNodeTypes,
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

        let evm_config = GnosisEvmConfig {
            collector_address: serde_json::from_value(collector_address.clone())?,
        };
        let executor = GnosisExecutorProvider::new(chain_spec, evm_config)?;

        Ok((evm_config, executor))
    }
}

/// A basic optimism consensus builder.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct GnosisConsensusBuilder;

impl<Node> ConsensusBuilder<Node> for GnosisConsensusBuilder
where
    Node: FullNodeTypes,
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
