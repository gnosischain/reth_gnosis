use std::sync::Arc;

use alloy_consensus::{proofs, BlockBody, BlockHeader, Header, TxReceipt, EMPTY_OMMER_ROOT_HASH};
use alloy_eips::merge::BEACON_NONCE;
use alloy_primitives::Bytes;
use gnosis_primitives::header::GnosisHeader;
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_errors::BlockExecutionError;
use reth_ethereum_primitives::Receipt;
use reth_evm::{
    block::BlockExecutorFactory,
    execute::{BlockAssembler, BlockAssemblerInput},
};
use reth_primitives::TransactionSigned;
use reth_primitives_traits::logs_bloom;
use reth_provider::BlockExecutionResult;
use revm::context::Block;

use crate::{block::GnosisBlockExecutionCtx, primitives::block::GnosisBlock};

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
            extra_data: Bytes::from("reth_gnosis@v1.0.1".as_bytes().to_vec()),
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
        ExecutionCtx<'a> = GnosisBlockExecutionCtx<'a>,
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
                    blob_gas_used,
                },
            state_root,
            ..
        } = input;

        let timestamp = evm_env.block_env.timestamp().saturating_to();

        let transactions_root = proofs::calculate_transaction_root(&transactions);
        let receipts_root = Receipt::calculate_receipt_root_no_memo(receipts);
        let logs_bloom = logs_bloom(receipts.iter().flat_map(|r| r.logs()));

        let withdrawals = self
            .chain_spec
            .is_shanghai_active_at_timestamp(timestamp)
            .then(|| ctx.withdrawals.map(|w| w.into_owned()).unwrap_or_default());

        let withdrawals_root = withdrawals
            .as_deref()
            .map(|w| proofs::calculate_withdrawals_root(w));
        let requests_hash = self
            .chain_spec
            .is_prague_active_at_timestamp(timestamp)
            .then(|| requests.requests_hash());

        let mut excess_blob_gas = None;
        let mut block_blob_gas_used = None;

        // only determine cancun fields when active
        if self.chain_spec.is_cancun_active_at_timestamp(timestamp) {
            block_blob_gas_used = Some(*blob_gas_used);
            excess_blob_gas = if self
                .chain_spec
                .is_cancun_active_at_timestamp(parent.timestamp)
            {
                parent.maybe_next_block_excess_blob_gas(
                    self.chain_spec.blob_params_at_timestamp(timestamp),
                )
            } else {
                // for the first post-fork block, both parent.blob_gas_used and
                // parent.excess_blob_gas are evaluated as 0
                Some(
                    alloy_eips::eip7840::BlobParams::cancun()
                        .next_block_excess_blob_gas_osaka(0, 0, 0),
                )
            };
        }

        let header = Header {
            parent_hash: ctx.parent_hash,
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            beneficiary: evm_env.block_env.beneficiary(),
            state_root,
            transactions_root,
            receipts_root,
            withdrawals_root,
            logs_bloom,
            timestamp,
            mix_hash: evm_env.block_env.prevrandao().unwrap_or_default(),
            nonce: BEACON_NONCE.into(),
            base_fee_per_gas: Some(evm_env.block_env.basefee()),
            number: evm_env.block_env.number().saturating_to(),
            gas_limit: evm_env.block_env.gas_limit(),
            difficulty: evm_env.block_env.difficulty(),
            gas_used: *gas_used,
            extra_data: self.extra_data.clone(),
            parent_beacon_block_root: ctx.parent_beacon_block_root,
            blob_gas_used: block_blob_gas_used,
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
