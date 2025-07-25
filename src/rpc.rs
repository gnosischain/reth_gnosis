use alloy_eips::eip2930::AccessList;
use alloy_network::{BuildResult, Network, NetworkWallet, TransactionBuilder};
use alloy_primitives::{Address, Bytes, ChainId, TxKind};
use alloy_rpc_types_eth::TransactionRequest;
use crate::primitives::header::GnosisHeader;

/// The gnosis RPC network types
#[derive(Debug, Copy, Default, Clone)]
#[non_exhaustive]
pub struct GnosisNetwork;

impl alloy_network::Network for GnosisNetwork {
    type TxType = alloy_consensus::TxType;

    type TxEnvelope = alloy_consensus::TxEnvelope;

    type UnsignedTx = alloy_consensus::TypedTransaction;

    type ReceiptEnvelope = alloy_consensus::ReceiptEnvelope;

    type Header = alloy_consensus::Header;

    type TransactionRequest = alloy_rpc_types_eth::transaction::TransactionRequest;

    type TransactionResponse = alloy_rpc_types_eth::Transaction;

    type ReceiptResponse = alloy_rpc_types_eth::TransactionReceipt;

    type HeaderResponse = alloy_rpc_types_eth::Header<GnosisHeader>;

    type BlockResponse = alloy_rpc_types_eth::Block<Self::TransactionResponse, Self::HeaderResponse>;
}

impl TransactionBuilder<GnosisNetwork> for TransactionRequest {
    fn chain_id(&self) -> Option<ChainId> {
        self.chain_id
    }

    fn set_chain_id(&mut self, chain_id: ChainId) {
        self.chain_id = Some(chain_id);
    }

    fn nonce(&self) -> Option<u64> {
        self.nonce
    }

    fn set_nonce(&mut self, nonce: u64) {
        self.nonce = Some(nonce);
    }

    fn take_nonce(&mut self) -> Option<u64> {
        self.nonce.take()
    }

    fn input(&self) -> Option<&Bytes> {
        self.input.input.as_ref()
    }

    fn set_input<T: Into<Bytes>>(&mut self, input: T) {
        self.input.input = Some(input.into());
    }

    fn from(&self) -> Option<Address> {
        self.from
    }

    fn set_from(&mut self, from: Address) {
        self.from = Some(from);
    }

    fn kind(&self) -> Option<TxKind> {
        self.to
    }

    fn clear_kind(&mut self) {
        self.to = None;
    }

    fn set_kind(&mut self, kind: TxKind) {
        self.to = Some(kind);
    }

    fn value(&self) -> Option<U256> {
        self.value
    }

    fn set_value(&mut self, value: U256) {
        self.value = Some(value);
    }

    fn gas_price(&self) -> Option<u128> {
        self.gas_price
    }

    fn set_gas_price(&mut self, gas_price: u128) {
        self.gas_price = Some(gas_price);
    }

    fn max_fee_per_gas(&self) -> Option<u128> {
        self.max_fee_per_gas
    }

    fn set_max_fee_per_gas(&mut self, max_fee_per_gas: u128) {
        self.max_fee_per_gas = Some(max_fee_per_gas);
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.max_priority_fee_per_gas
    }

    fn set_max_priority_fee_per_gas(&mut self, max_priority_fee_per_gas: u128) {
        self.max_priority_fee_per_gas = Some(max_priority_fee_per_gas);
    }

    fn gas_limit(&self) -> Option<u64> {
        self.gas
    }

    fn set_gas_limit(&mut self, gas_limit: u64) {
        self.gas = Some(gas_limit);
    }

    fn access_list(&self) -> Option<&AccessList> {
        self.access_list.as_ref()
    }

    fn set_access_list(&mut self, access_list: AccessList) {
        self.access_list = Some(access_list);
    }

    fn complete_type(
        &self,
        ty: <GnosisNetwork as Network>::TxType,
    ) -> Result<(), Vec<&'static str>> {

    }

    fn can_submit(&self) -> bool {
        self.from.is_some() &&
            self.to.is_some() &&
            self.gas.is_some() &&
            (self.gas_price.is_some() || self.max_fee_per_gas.is_some())
    }

    fn can_build(&self) -> bool {
        self.to.is_some() &&
            self.gas.is_some() &&
            (self.gas_price.is_some() || self.max_fee_per_gas.is_some())
    }

    fn output_tx_type(&self) -> <GnosisNetwork as Network>::TxType {

    }

    fn output_tx_type_checked(&self) -> Option<<GnosisNetwork as Network>::TxType> {

    }

    fn prep_for_submission(&mut self) {

    }

    fn build_unsigned(
        self,
    ) -> BuildResult<<GnosisNetwork as Network>::UnsignedTx, GnosisNetwork> {
        Ok(<Self as TransactionBuilder<GnosisNetwork>>::output_tx_type(&self))
    }

    async fn build<W: NetworkWallet<GnosisNetwork>>(
        self,
        _wallet: &W,
    ) -> Result<<GnosisNetwork as Network>::TxEnvelope, TransactionBuilderError<GnosisNetwork>>
    {
        Err(TransactionBuilderError::InvalidTransactionRequest(
            <Self as TransactionBuilder<GnosisNetwork>>::output_tx_type(&self),
            vec!["unsupported"],
        ))
    }
}