use execute::GnosisExecutorProvider;
use reth::{
    api::NodeTypes,
    builder::{
        components::{ComponentsBuilder, ExecutorBuilder},
        node::FullNodeTypes,
        BuilderContext, Node,
    },
};
use reth_evm_ethereum::EthEvmConfig;
use reth_node_ethereum::{
    node::{EthereumNetworkBuilder, EthereumPayloadBuilder, EthereumPoolBuilder},
    EthEngineTypes, EthereumNode,
};

mod execute;

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
    >
    where
        Node: FullNodeTypes<Engine = EthEngineTypes>,
    {
        EthereumNode::components().executor(GnosisExecutorBuilder::default())
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
    type EVM = EthEvmConfig;
    // Must implement BlockExecutorProvider;
    type Executor = GnosisExecutorProvider<Self::EVM>;

    async fn build_evm(
        self,
        ctx: &BuilderContext<Node>,
    ) -> eyre::Result<(Self::EVM, Self::Executor)> {
        let chain_spec = ctx.chain_spec();
        let evm_config = EthEvmConfig::default();
        let executor = GnosisExecutorProvider::new(chain_spec, evm_config);

        Ok((evm_config, executor))
    }
}
