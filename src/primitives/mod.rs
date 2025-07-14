use block::{Block, BlockBody, TransactionSigned};
use header::GnosisHeader;
use reth_node_builder::{
    rpc::{EthApiBuilder, EthApiCtx},
    FullNodeComponents, NodeTypes,
};
use reth_primitives::{NodePrimitives, Receipt};
use reth_provider::EthStorage;
use reth_rpc::eth::{EthApiFor, FullEthApiServer};
use reth_trie_db::MerklePatriciaTrie;

use crate::{engine::GnosisEngineTypes, spec::gnosis_spec::GnosisChainSpec, GnosisNode};

pub mod block;
pub mod header;
pub mod rpc;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GnosisNodePrimitives;

impl NodePrimitives for GnosisNodePrimitives {
    type Block = Block;
    type BlockHeader = GnosisHeader;
    type BlockBody = BlockBody;
    type SignedTx = TransactionSigned;
    type Receipt = Receipt;
}

// /// Builds [`EthApi`](reth_rpc::EthApi) for Ethereum.
// #[derive(Debug, Default)]
// pub struct GnosisEthApiBuilder;

// impl<N> EthApiBuilder<N> for GnosisEthApiBuilder
// where
//     N: FullNodeComponents<Types = GnosisNodePrimitives>,
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
