// use std::sync::Arc;

// use reth::rpc::api::BlockSubmissionValidationApiServer;
// use reth::rpc::{
//     api::eth::FromEvmError,
//     builder::{config::RethRpcServerConfig, RethRpcModule},
//     server_types::eth::EthApiError,
// };
// use reth_chainspec::Hardforks;
// use reth_evm::{ConfigureEvm, EvmFactory, EvmFactoryFor, NextBlockEnvAttributes};
// use reth_node_api::AddOnsContext;
// use reth_node_builder::{
//     rpc::{
//         BasicEngineApiBuilder, EngineValidatorAddOn, EngineValidatorBuilder, EthApiBuilder,
//         EthApiCtx, RethRpcAddOns, RpcAddOns, RpcHandle,
//     },
//     FullNodeComponents, NodeAddOns, NodeTypes,
// };
// use reth_node_ethereum::EthereumEngineValidator;
// use reth_provider::EthStorage;
// use reth_rpc::{
//     eth::{EthApiFor, EthApiServer, FullEthApiServer},
//     ValidationApi,
// };
// use revm::context::TxEnv;

// use crate::{
//     engine::{GnosisEngineTypes, GnosisEngineValidator},
//     primitives::GnosisNodePrimitives,
//     spec::gnosis_spec::GnosisChainSpec,
//     GnosisEngineValidatorBuilder, GnosisNode,
// };

// use super::block::TransactionSigned;

// #[derive(Debug, Default)]
// #[non_exhaustive]
// pub struct GnosisApiBuilder;

// impl<N> EthApiBuilder<N> for GnosisApiBuilder
// where
//     N: FullNodeComponents<Types = GnosisNode>,
//     EthApiFor<N>: FullEthApiServer<Provider = N::Provider, Pool = N::Pool>,
// {
//     type EthApi = EthApiFor<N>;

//     async fn build_eth_api(self, ctx: EthApiCtx<'_, N>) -> eyre::Result<Self::EthApi> {
//         let api = reth_rpc::EthApiBuilder::new(
//             ctx.components.provider().clone(),
//             ctx.components.pool().clone(),
//             ctx.components.network().clone(),
//             ctx.components.evm_config().clone(),
//         )
//         .eth_cache(ctx.cache)
//         .task_spawner(ctx.components.task_executor().clone())
//         .gas_cap(ctx.config.rpc_gas_cap.into())
//         .max_simulate_blocks(ctx.config.rpc_max_simulate_blocks)
//         .eth_proof_window(ctx.config.eth_proof_window)
//         .fee_history_cache_config(ctx.config.fee_history_cache)
//         .proof_permits(ctx.config.proof_permits)
//         .gas_oracle_config(ctx.config.gas_oracle)
//         .build();
//         Ok(api)
//     }
// }

// #[derive(Debug)]
// pub struct GnosisAddOns<N>
// where
//     N: FullNodeComponents<Types = GnosisNode>,
//     EthApiFor<N>: FullEthApiServer<Provider = N::Provider, Pool = N::Pool>,
// {
//     inner: RpcAddOns<
//         N,
//         GnosisApiBuilder,
//         GnosisEngineValidatorBuilder,
//         BasicEngineApiBuilder<GnosisEngineValidatorBuilder>,
//     >,
// }

// impl<N> NodeAddOns<N> for GnosisAddOns<N>
// where
//     N: FullNodeComponents<
//         Types: NodeTypes<
//             ChainSpec = GnosisChainSpec,
//             Primitives = GnosisNodePrimitives,
//             Payload = GnosisEngineTypes,
//         >,
//         Evm: ConfigureEvm<NextBlockEnvCtx = NextBlockEnvAttributes>,
//     >,
//     EthApiError: FromEvmError<N::Evm>,
//     EvmFactoryFor<N::Evm>: EvmFactory<Tx = TxEnv>,
// {
//     type Handle = RpcHandle<N, EthApiFor<N>>;

//     async fn launch_add_ons(
//         self,
//         ctx: reth_node_api::AddOnsContext<'_, N>,
//     ) -> eyre::Result<Self::Handle> {
//         let validation_api = ValidationApi::new(
//             ctx.node.provider().clone(),
//             Arc::new(ctx.node.consensus().clone()),
//             ctx.node.evm_config().clone(),
//             ctx.config.rpc.flashbots_config(),
//             Box::new(ctx.node.task_executor().clone()),
//             Arc::new(GnosisEngineValidator::new(Arc::new(
//                 ctx.config.chain.inner.clone(),
//             ))),
//         );

//         self.inner
//             .launch_add_ons_with(ctx, move |container| {
//                 container.modules.merge_if_module_configured(
//                     RethRpcModule::Flashbots,
//                     validation_api.into_rpc(),
//                 )?;

//                 Ok(())
//             })
//             .await
//     }
// }

// impl<N: FullNodeComponents<Types = GnosisNode>> Default for GnosisAddOns<N>
// where
//     EthApiFor<N>: FullEthApiServer<Provider = N::Provider, Pool = N::Pool>,
// {
//     fn default() -> Self {
//         Self {
//             inner: Default::default(),
//         }
//     }
// }

// impl<N> RethRpcAddOns<N> for GnosisAddOns<N>
// where
//     N: FullNodeComponents<
//         Types: NodeTypes<
//             ChainSpec = GnosisChainSpec,
//             Primitives = GnosisNodePrimitives,
//             Payload = GnosisEngineTypes,
//         >,
//         Evm: ConfigureEvm<NextBlockEnvCtx = NextBlockEnvAttributes>,
//     >,
//     EthApiError: FromEvmError<N::Evm>,
//     EvmFactoryFor<N::Evm>: EvmFactory<Tx = TxEnv>,
// {
//     type EthApi = EthApiFor<N>;

//     fn hooks_mut(&mut self) -> &mut reth_node_builder::rpc::RpcHooks<N, Self::EthApi> {
//         self.inner.hooks_mut()
//     }
// }

// impl<N> EngineValidatorAddOn<N> for GnosisAddOns<N>
// where
//     N: FullNodeComponents<Types = GnosisNode>,
//     EthApiFor<N>: FullEthApiServer<Provider = N::Provider, Pool = N::Pool>,
// {
//     type Validator = GnosisEngineValidator;

//     async fn engine_validator(&self, ctx: &AddOnsContext<'_, N>) -> eyre::Result<Self::Validator> {
//         GnosisEngineValidatorBuilder::default().build(ctx).await
//     }
// }
