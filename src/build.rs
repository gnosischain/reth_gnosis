use std::sync::Arc;

use alloy_consensus::{
    proofs, BlockBody, BlockHeader, Header, Transaction, TxReceipt, EMPTY_OMMER_ROOT_HASH,
};
use alloy_eips::merge::BEACON_NONCE;
use alloy_primitives::Bytes;
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_errors::BlockExecutionError;
use reth_ethereum_primitives::Receipt;
use reth_evm::{
    block::BlockExecutorFactory,
    eth::EthBlockExecutionCtx,
    execute::{BlockAssembler, BlockAssemblerInput},
};
use reth_primitives::TransactionSigned;
use reth_primitives_traits::logs_bloom;
use reth_provider::BlockExecutionResult;

use crate::primitives::{block::Block as GnosisBlock, header::GnosisHeader};
/// Block builder for Gnosis.
#[derive(Debug)]
pub struct GnosisBlockAssembler<ChainSpec> {
    chain_spec: Arc<ChainSpec>,
    /// Extra data to use for the blocks.
    pub extra_data: Bytes,
}

impl<ChainSpec> GnosisBlockAssembler<ChainSpec> {
    /// Creates a new [`GnosisBlockAssembler`].
    pub fn new(chain_spec: Arc<ChainSpec>) -> Self {
        Self {
            chain_spec,
            // extra data representing "reth@v0.0.1-alpha0"
            extra_data: Bytes::from("reth@v0.1.0".as_bytes().to_vec()),
        }
    }
}

impl<ChainSpec> Clone for GnosisBlockAssembler<ChainSpec> {
    fn clone(&self) -> Self {
        Self {
            chain_spec: self.chain_spec.clone(),
            extra_data: self.extra_data.clone(),
        }
    }
}

// REF: https://github.com/paradigmxyz/reth/blob/91eb292e3ea1fae86accd6c887ba05b7b4fd045e/crates/optimism/evm/src/build.rs#L37
impl<F, ChainSpec> BlockAssembler<F> for GnosisBlockAssembler<ChainSpec>
where
    F: for<'a> BlockExecutorFactory<
        ExecutionCtx<'a> = EthBlockExecutionCtx<'a>,
        Transaction = TransactionSigned,
        Receipt = Receipt,
    >,
    ChainSpec: EthChainSpec + EthereumHardforks,
{
    type Block = GnosisBlock;

    fn assemble_block(
        &self,
        input: BlockAssemblerInput<'_, '_, F, GnosisHeader>,
    ) -> Result<GnosisBlock, BlockExecutionError> {
        let BlockAssemblerInput {
            evm_env,
            execution_ctx: ctx,
            parent,
            transactions,
            output:
                BlockExecutionResult {
                    receipts,
                    requests,
                    gas_used,
                },
            state_root,
            ..
        } = input;

        let timestamp = evm_env.block_env.timestamp;

        let transactions_root = proofs::calculate_transaction_root(&transactions);
        let receipts_root = Receipt::calculate_receipt_root_no_memo(receipts);
        let logs_bloom = logs_bloom(receipts.iter().flat_map(|r| r.logs()));

        let withdrawals = self
            .chain_spec
            .is_shanghai_active_at_timestamp(timestamp.to())
            .then(|| ctx.withdrawals.map(|w| w.into_owned()).unwrap_or_default());

        let withdrawals_root = withdrawals
            .as_deref()
            .map(|w| proofs::calculate_withdrawals_root(w));
        let requests_hash = self
            .chain_spec
            .is_prague_active_at_timestamp(timestamp.to())
            .then(|| requests.requests_hash());

        let mut excess_blob_gas = None;
        let mut blob_gas_used = None;

        // only determine cancun fields when active
        if self
            .chain_spec
            .is_cancun_active_at_timestamp(timestamp.to())
        {
            blob_gas_used = Some(
                transactions
                    .iter()
                    .map(|tx| tx.blob_gas_used().unwrap_or_default())
                    .sum(),
            );
            excess_blob_gas = if self
                .chain_spec
                .is_cancun_active_at_timestamp(parent.timestamp)
            {
                parent.maybe_next_block_excess_blob_gas(
                    self.chain_spec.blob_params_at_timestamp(timestamp.to()),
                )
            } else {
                // for the first post-fork block, both parent.blob_gas_used and
                // parent.excess_blob_gas are evaluated as 0
                Some(crate::blobs::CANCUN_BLOB_PARAMS.next_block_excess_blob_gas(0, 0))
            };
        }

        let header = Header {
            parent_hash: ctx.parent_hash,
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            beneficiary: evm_env.block_env.beneficiary,
            state_root,
            transactions_root,
            receipts_root,
            withdrawals_root,
            logs_bloom,
            timestamp: timestamp.to(),
            mix_hash: evm_env.block_env.prevrandao.unwrap_or_default(),
            nonce: BEACON_NONCE.into(),
            base_fee_per_gas: Some(evm_env.block_env.basefee),
            number: evm_env.block_env.number.to(),
            gas_limit: evm_env.block_env.gas_limit,
            difficulty: evm_env.block_env.difficulty,
            gas_used: *gas_used,
            extra_data: self.extra_data.clone(),
            parent_beacon_block_root: ctx.parent_beacon_block_root,
            blob_gas_used,
            excess_blob_gas,
            requests_hash,
        };

        Ok(GnosisBlock {
            header: header.into(),
            body: BlockBody {
                transactions,
                ommers: Default::default(),
                withdrawals,
            },
        })
    }
}
