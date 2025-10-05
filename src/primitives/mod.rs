// this module is the central source of gnosis-specific primitives
// it's made to facilitate the upcoming switch to gnosis_primitives crate

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
