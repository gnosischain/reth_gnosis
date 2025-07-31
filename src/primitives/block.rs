use alloy_consensus::TxEip4844;
use gnosis_primitives::header::GnosisHeader;

pub type TransactionSigned = alloy_consensus::EthereumTxEnvelope<TxEip4844>;

/// The Block type of this node
pub type Block = alloy_consensus::Block<TransactionSigned, GnosisHeader>;

/// The body type of this node
pub type BlockBody = alloy_consensus::BlockBody<TransactionSigned, GnosisHeader>;

/// A local trait to convert AlloyBlock<AlloyHeader> into Block
pub trait IntoBlock {
    fn into_block(self) -> Block;
}

impl IntoBlock for alloy_consensus::Block<TransactionSigned, alloy_consensus::Header> {
    fn into_block(self) -> Block {
        Block {
            header: GnosisHeader::from(self.header),
            body: BlockBody {
                transactions: self.body.transactions,
                ommers: self
                    .body
                    .ommers
                    .into_iter()
                    .map(|ommer| GnosisHeader::from(ommer))
                    .collect(),
                withdrawals: self.body.withdrawals,
            },
        }
    }
}
