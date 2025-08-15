use reth_node_api::FullNodeComponents;
use reth_node_builder::rpc::{EthApiBuilder, EthApiCtx};
use reth_rpc::eth::core::EthApiFor;
use reth_rpc_eth_api::{helpers::AddDevSigners, FullEthApiServer};

#[derive(Debug, Default, Clone)]
pub struct GnosisEthApiBuilder;

impl<N> EthApiBuilder<N> for GnosisEthApiBuilder
where
	N: FullNodeComponents,
	EthApiFor<N>: FullEthApiServer<Provider = N::Provider, Pool = N::Pool> + AddDevSigners + Unpin + 'static,
{
	type EthApi = EthApiFor<N>;

	fn build_eth_api(
		self,
		ctx: EthApiCtx<'_, N>,
	) -> impl core::future::Future<Output = eyre::Result<Self::EthApi>> + Send {
		async move {
			let api = reth_rpc::eth::core::EthApi::builder(
				ctx.components.provider().clone(),
				ctx.components.pool().clone(),
				ctx.components.network().clone(),
				ctx.components.evm_config().clone(),
			)
			.eth_cache(ctx.cache)
			.task_spawner(ctx.components.task_executor().clone())
			.build();
			Ok(api)
		}
	}
}