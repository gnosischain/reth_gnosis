use alloy_consensus::Header;
use reth_rpc::RpcTypes;

/// The gnosis RPC network types
#[derive(Debug, Copy, Default, Clone)]
#[non_exhaustive]
pub struct GnosisNetwork;

impl RpcTypes for GnosisNetwork {
    type Header = alloy_rpc_types_eth::Header<Header>;
    type Receipt = alloy_rpc_types_eth::TransactionReceipt;
    type TransactionRequest = alloy_rpc_types_eth::transaction::TransactionRequest;
    type TransactionResponse = alloy_rpc_types_eth::Transaction;
}
