use alloy_consensus::TxEip4844;

pub type TransactionSigned = alloy_consensus::EthereumTxEnvelope<TxEip4844>;

pub type GnosisHeader = alloy_consensus::Header;

/// The Block type of this node
pub type GnosisBlock = alloy_consensus::Block<TransactionSigned, GnosisHeader>;

/// The body type of this node
pub type BlockBody = alloy_consensus::BlockBody<TransactionSigned, GnosisHeader>;

/// A local trait to convert AlloyBlock<AlloyHeader> into Block
pub trait IntoBlock {
    fn into_block(self) -> GnosisBlock;
}

impl IntoBlock for alloy_consensus::Block<TransactionSigned, alloy_consensus::Header> {
    fn into_block(self) -> GnosisBlock {
        GnosisBlock {
            header: GnosisHeader::from(self.header),
            body: BlockBody {
                transactions: self.body.transactions,
                ommers: self
                    .body
                    .ommers
                    .into_iter()
                    .map(GnosisHeader::from)
                    .collect(),
                withdrawals: self.body.withdrawals,
            },
        }
    }
}
