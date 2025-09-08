use block::{Block, BlockBody, TransactionSigned};
use gnosis_primitives::header::GnosisHeader;
use reth_primitives::{NodePrimitives, Receipt};

pub mod block;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GnosisNodePrimitives;

impl NodePrimitives for GnosisNodePrimitives {
    type Block = Block;
    type BlockHeader = GnosisHeader;
    type BlockBody = BlockBody;
    type SignedTx = TransactionSigned;
    type Receipt = Receipt;
}
