use block::{Block, BlockBody, TransactionSigned};
use gnosis_primitives::header::GnosisHeader;
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
