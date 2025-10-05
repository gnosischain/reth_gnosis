use alloy_consensus::TxEip4844;

pub type TransactionSigned = alloy_consensus::EthereumTxEnvelope<TxEip4844>;

pub type GnosisHeader = alloy_consensus::Header;

/// The Block type of this node
pub type GnosisBlock = alloy_consensus::Block<TransactionSigned, GnosisHeader>;

/// The body type of this node
pub type BlockBody = alloy_consensus::BlockBody<TransactionSigned, GnosisHeader>;
