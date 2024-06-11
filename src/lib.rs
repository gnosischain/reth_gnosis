use execute::{GnosisEvmConfig, GnosisExecutorProvider};
use reth::{
    args::RpcServerArgs,
    builder::{
        components::ExecutorBuilder, node::FullNodeTypes, BuilderContext, NodeBuilder, NodeConfig,
    },
    primitives::{Chain, ChainSpec, Genesis},
    tasks::TaskManager,
};
use reth_node_ethereum::EthereumNode;

mod execute;

#[derive(Debug, Default, Clone)]
struct Args {}

/// Type configuration for a regular Optimism node.
#[derive(Debug, Default, Clone)]
pub struct GnosisNode {
    /// Additional Optimism args
    pub args: Args,
}

pub async fn start_node() {
    let tasks = TaskManager::current();

    // create optimism genesis with canyon at block 2
    let spec = ChainSpec::builder()
        .chain(Chain::mainnet())
        .genesis(Genesis::default())
        .london_activated()
        .paris_activated()
        .shanghai_activated()
        .cancun_activated()
        .build();

    let node_config = NodeConfig::test()
        .with_rpc(RpcServerArgs::default().with_http())
        .with_chain(spec);

    let handle = NodeBuilder::new(node_config)
        .testing_node(tasks.executor())
        .with_types::<EthereumNode>()
        .with_components(EthereumNode::components().executor(GnosisExecutorBuilder::default()))
        .launch()
        .await
        .unwrap();

    println!("Node started");

    handle.wait_for_node_exit().await.unwrap();
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
    ) -> Result<(Self::EVM, Self::Executor), ()> {
        let chain_spec = ctx.chain_spec();
        let evm_config = GnosisEvmConfig::default();
        let executor = GnosisExecutorProvider::new(chain_spec, evm_config);

        Ok((evm_config, executor))
    }
}
