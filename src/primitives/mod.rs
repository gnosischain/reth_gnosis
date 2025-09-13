use block::{BlockBody, GnosisBlock, TransactionSigned};
use reth_primitives::{NodePrimitives, Receipt};

pub mod block;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GnosisNodePrimitives;

impl NodePrimitives for GnosisNodePrimitives {
    type Block = GnosisBlock;
    type BlockHeader = block::GnosisHeader;
    type BlockBody = BlockBody;
    type SignedTx = TransactionSigned;
    type Receipt = Receipt;
}
