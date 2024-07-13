//! This module is exactly identical to <https://github.com/paradigmxyz/reth/blob/268e768d822a0d4eb8ed365dc6390862f759a849/crates/ethereum/evm/src/execute.rs>

use reth::{
    primitives::{BlockWithSenders, Receipt, Request},
    providers::ProviderError,
    revm::{
        primitives::ResultAndState, state_change::apply_blockhashes_update, Database,
        DatabaseCommit, Evm, State,
    },
};
use reth_chainspec::{ChainSpec, EthereumHardforks};
use reth_evm::{
    execute::{BlockExecutionError, BlockValidationError},
    system_calls::{
        apply_beacon_root_contract_call, apply_consolidation_requests_contract_call,
        apply_withdrawal_requests_contract_call,
    },
    ConfigureEvm,
};
use reth_evm_ethereum::eip6110::parse_deposits_from_receipts;
use revm_primitives::EVMError;
use std::{fmt::Display, sync::Arc};

/// Helper type for the output of executing a block.
#[derive(Debug, Clone)]
pub struct EthExecuteOutput {
    pub receipts: Vec<Receipt>,
    pub requests: Vec<Request>,
    pub gas_used: u64,
}

/// Helper container type for EVM with chain spec.
#[derive(Debug, Clone)]
pub struct EthEvmExecutor<EvmConfig> {
    /// The chainspec
    pub chain_spec: Arc<ChainSpec>,
    /// How to create an EVM.
    pub evm_config: EvmConfig,
}

impl<EvmConfig> EthEvmExecutor<EvmConfig>
where
    EvmConfig: ConfigureEvm,
{
    /// Executes the transactions in the block and returns the receipts of the transactions in the
    /// block, the total gas used and the list of EIP-7685 [requests](Request).
    ///
    /// This applies the pre-execution and post-execution changes that require an [EVM](Evm), and
    /// executes the transactions.
    pub fn execute_state_transitions<Ext, DB>(
        &self,
        block: &BlockWithSenders,
        mut evm: Evm<'_, Ext, &mut State<DB>>,
    ) -> Result<EthExecuteOutput, BlockExecutionError>
    where
        DB: Database<Error: Into<ProviderError> + Display>,
    {
        // apply pre execution changes
        apply_beacon_root_contract_call(
            &self.evm_config,
            &self.chain_spec,
            block.timestamp,
            block.number,
            block.parent_beacon_block_root,
            &mut evm,
        )?;
        apply_blockhashes_update(
            evm.db_mut(),
            &self.chain_spec,
            block.timestamp,
            block.number,
            block.parent_hash,
        )?;

        // execute transactions
        let mut cumulative_gas_used = 0;
        let mut receipts = Vec::with_capacity(block.body.len());
        for (sender, transaction) in block.transactions_with_sender() {
            // The sum of the transaction‚Äôs gas limit, Tg, and the gas utilized in this block prior,
            // must be no greater than the block‚Äôs gasLimit.
            let block_available_gas = block.header.gas_limit - cumulative_gas_used;
            if transaction.gas_limit() > block_available_gas {
                return Err(
                    BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                        transaction_gas_limit: transaction.gas_limit(),
                        block_available_gas,
                    }
                    .into(),
                );
            }

            self.evm_config
                .fill_tx_env(evm.tx_mut(), transaction, *sender);

            // Execute transaction.
            let ResultAndState { result, state } = evm.transact().map_err(move |err| {
                let new_err = match err {
                    EVMError::Transaction(e) => EVMError::Transaction(e),
                    EVMError::Header(e) => EVMError::Header(e),
                    EVMError::Database(e) => EVMError::Database(e.into()),
                    EVMError::Custom(e) => EVMError::Custom(e),
                    EVMError::Precompile(e) => EVMError::Precompile(e),
                };
                // Ensure hash is calculated for error log, if not already done
                BlockValidationError::EVM {
                    hash: transaction.recalculate_hash(),
                    error: Box::new(new_err),
                }
            })?;
            evm.db_mut().commit(state);

            // append gas used
            cumulative_gas_used += result.gas_used();

            // Push transaction changeset and calculate header bloom filter for receipt.
            receipts.push(
                #[allow(clippy::needless_update)] // side-effect of optimism fields
                Receipt {
                    tx_type: transaction.tx_type(),
                    // Success flag was added in `EIP-658: Embedding transaction status code in
                    // receipts`.
                    success: result.is_success(),
                    cumulative_gas_used,
                    // convert to reth log
                    logs: result.into_logs(),
                    ..Default::default()
                },
            );
        }

        let requests = if self
            .chain_spec
            .is_prague_active_at_timestamp(block.timestamp)
        {
            // Collect all EIP-6110 deposits
            let deposit_requests = parse_deposits_from_receipts(&self.chain_spec, &receipts)?;

            // Collect all EIP-7685 requests
            let withdrawal_requests =
                apply_withdrawal_requests_contract_call(&self.evm_config, &mut evm)?;

            // Collect all EIP-7251 requests
            let consolidation_requests =
                apply_consolidation_requests_contract_call(&self.evm_config, &mut evm)?;

            [
                deposit_requests,
                withdrawal_requests,
                consolidation_requests,
            ]
            .concat()
        } else {
            vec![]
        };

        Ok(EthExecuteOutput {
            receipts,
            requests,
            gas_used: cumulative_gas_used,
        })
    }
}
