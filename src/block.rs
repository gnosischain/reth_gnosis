use std::borrow::Cow;

use alloy_consensus::{Transaction, TxReceipt};
use alloy_eips::eip4895::Withdrawals;
use alloy_eips::eip7002::WITHDRAWAL_REQUEST_TYPE;
use alloy_eips::eip7251;
use alloy_eips::{eip7685::Requests, Encodable2718};
use alloy_evm::block::ExecutableTx;
use alloy_evm::{
    block::state_changes::balance_increment_state,
    eth::eip6110::{self, parse_deposits_from_receipts},
    FromTxWithEncoded,
};
use alloy_evm::{Database, Evm};
use alloy_primitives::B256;
use reth_chainspec::EthereumHardforks;
use reth_errors::{BlockExecutionError, BlockValidationError};
use reth_evm::{
    block::{
        BlockExecutor, BlockExecutorFactory, BlockExecutorFor, StateChangePostBlockSource,
        StateChangeSource, SystemCaller,
    },
    eth::{
        receipt_builder::{AlloyReceiptBuilder, ReceiptBuilder, ReceiptBuilderCtx},
        spec::EthExecutorSpec,
    },
    EvmFactory, FromRecoveredTx, OnStateHook,
};
use reth_provider::BlockExecutionResult;
use revm::context::Block;
use revm::{context::result::ResultAndState, DatabaseCommit, Inspector};
use revm_database::State;
use revm_primitives::{Address, Log};

use crate::evm::factory::GnosisEvmFactory;
use crate::gnosis::apply_post_block_system_calls;
use crate::spec::gnosis_spec::GnosisChainSpec;

/// Gnosis-specific block execution context.
/// Extends the standard Ethereum context with parent timestamp for hardfork activation checks.
#[derive(Debug, Clone)]
pub struct GnosisBlockExecutionCtx<'a> {
    /// Hash of the parent block.
    pub parent_hash: B256,
    /// Parent beacon block root (for EIP-4788).
    pub parent_beacon_block_root: Option<B256>,
    /// Withdrawals for this block.
    pub withdrawals: Option<Cow<'a, Withdrawals>>,
    /// Parent block timestamp - used for detecting hardfork activation boundaries.
    pub parent_timestamp: u64,
}

// REF: https://github.com/alloy-rs/evm/blob/99d5b552c131e3419448c214e09474bf4f0d1e4b/crates/op-evm/src/block/mod.rs#L42
/// Block executor for Gnosis.
#[derive(Debug)]
pub struct GnosisBlockExecutor<'a, Evm, R: ReceiptBuilder> {
    /// Reference to the specification object.
    spec: GnosisChainSpec,

    /// Context for block execution.
    pub ctx: GnosisBlockExecutionCtx<'a>,
    /// Inner EVM.
    evm: Evm,
    /// Utility to call system smart contracts.
    system_caller: SystemCaller<GnosisChainSpec>,
    /// Receipt builder.
    receipt_builder: R,

    /// Receipts of executed transactions.
    receipts: Vec<R::Receipt>,
    /// Total gas used by transactions in this block.
    gas_used: u64,

    // Gnosis-specific fields
    block_rewards_address: Address,
}

impl<'a, Evm, R> GnosisBlockExecutor<'a, Evm, R>
where
    R: ReceiptBuilder,
{
    /// Creates a new [`GnosisBlockExecutor`]
    pub fn new(
        evm: Evm,
        ctx: GnosisBlockExecutionCtx<'a>,
        spec: &GnosisChainSpec,
        receipt_builder: R,
        block_rewards_address: Address,
    ) -> Self {
        Self {
            evm,
            ctx,
            receipts: Vec::new(),
            gas_used: 0,
            system_caller: SystemCaller::new(spec.clone()),
            spec: spec.clone(),
            receipt_builder,
            block_rewards_address,
        }
    }
}

// REF: https://github.com/alloy-rs/evm/blob/99d5b552c131e3419448c214e09474bf4f0d1e4b/crates/evm/src/eth/block.rs#L81
// ALong with the usual logic, we introduce some Gnosis-specific logic here (Denoted as such)
impl<'db, DB, E, R> BlockExecutor for GnosisBlockExecutor<'_, E, R>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    R: ReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt<Log = Log>>,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag = self
            .spec
            .is_spurious_dragon_active_at_block(self.evm.block().number.saturating_to());
        self.evm.db_mut().set_state_clear_flag(state_clear_flag);

        self.system_caller
            .apply_blockhashes_contract_call(self.ctx.parent_hash, &mut self.evm)?;
        self.system_caller
            .apply_beacon_root_contract_call(self.ctx.parent_beacon_block_root, &mut self.evm)?;

        Ok(())
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<ResultAndState<<Self::Evm as Evm>::HaltReason>, BlockExecutionError> {
        // The sum of the transaction's gas limit, Tg, and the gas utilized in this block prior,
        // must be no greater than the block's gasLimit.
        let block_available_gas = self.evm.block().gas_limit - self.gas_used;

        if tx.tx().gas_limit() > block_available_gas {
            return Err(
                BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                    transaction_gas_limit: tx.tx().gas_limit(),
                    block_available_gas,
                }
                .into(),
            );
        }

        // Execute transaction and return the result
        self.evm.transact(&tx).map_err(|err| {
            let hash = tx.tx().trie_hash();
            BlockExecutionError::evm(err, hash)
        })
    }

    fn commit_transaction(
        &mut self,
        output: ResultAndState<<Self::Evm as Evm>::HaltReason>,
        tx: impl ExecutableTx<Self>,
    ) -> Result<u64, BlockExecutionError> {
        let ResultAndState { result, state } = output;

        self.system_caller
            .on_state(StateChangeSource::Transaction(self.receipts.len()), &state);

        let gas_used = result.gas_used();

        // append gas used
        self.gas_used += gas_used;

        // Push transaction changeset and calculate header bloom filter for receipt.
        self.receipts
            .push(self.receipt_builder.build_receipt(ReceiptBuilderCtx {
                tx: tx.tx(),
                evm: &self.evm,
                result,
                state: &state,
                cumulative_gas_used: self.gas_used,
            }));

        // Commit the state changes.
        self.evm.db_mut().commit(state);

        Ok(gas_used)
    }

    fn finish(
        mut self,
    ) -> Result<(Self::Evm, BlockExecutionResult<R::Receipt>), BlockExecutionError> {
        let deposit_contract = self.spec.deposit_contract_address();
        let deposit_contract = deposit_contract.unwrap_or_else(|| {
            panic!("Deposit contract address is not set in the chain specification");
        });
        let timestamp = self.evm.block().timestamp();
        let withdrawals = self.ctx.withdrawals.as_deref();
        let beneficiary = self.evm.block().beneficiary();

        let requests = if self
            .spec
            .is_prague_active_at_timestamp(self.evm.block().timestamp.to())
        {
            // Collect all EIP-6110 deposits
            let deposit_requests = parse_deposits_from_receipts(&self.spec, &self.receipts)?;

            let mut requests = Requests::default();

            if !deposit_requests.is_empty() {
                requests.push_request_with_type(eip6110::DEPOSIT_REQUEST_TYPE, deposit_requests);
            }

            let withdrawal_requests = self
                .system_caller
                .apply_withdrawal_requests_contract_call(&mut self.evm)?;
            if !withdrawal_requests.is_empty() {
                requests.push_request_with_type(WITHDRAWAL_REQUEST_TYPE, withdrawal_requests);
            }

            // Collect all EIP-7251 requests
            let consolidation_requests = self
                .system_caller
                .apply_consolidation_requests_contract_call(&mut self.evm)?;
            if !consolidation_requests.is_empty() {
                requests.push_request_with_type(
                    eip7251::CONSOLIDATION_REQUEST_TYPE,
                    consolidation_requests,
                );
            }
            requests
        } else {
            Requests::default()
        };

        // Gnosis-specific // Start
        let (balance_increments, _) = apply_post_block_system_calls(
            &self.spec,
            self.block_rewards_address,
            deposit_contract,
            timestamp.to(),
            withdrawals,
            beneficiary,
            &mut self.evm,
            &mut self.system_caller,
        )?;
        // Gnosis-specific // End

        // increment balances
        self.evm
            .db_mut()
            .increment_balances(balance_increments.clone())
            .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;

        // call state hook with changes due to balance increments.
        self.system_caller.try_on_state_with(|| {
            balance_increment_state(&balance_increments, self.evm.db_mut()).map(|state| {
                (
                    StateChangeSource::PostBlock(StateChangePostBlockSource::BalanceIncrements),
                    Cow::Owned(state),
                )
            })
        })?;

        Ok((
            self.evm,
            BlockExecutionResult {
                receipts: self.receipts,
                requests,
                gas_used: self.gas_used,
            },
        ))
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.system_caller.with_state_hook(hook);
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        &mut self.evm
    }

    fn evm(&self) -> &Self::Evm {
        &self.evm
    }
}

/// Ethereum block executor factory.
#[derive(Debug, Clone, Default)]
pub struct GnosisBlockExecutorFactory<R = AlloyReceiptBuilder, EvmFactory = GnosisEvmFactory> {
    /// Receipt builder.
    receipt_builder: R,
    /// Chain specification.
    spec: GnosisChainSpec,
    /// EVM factory.
    evm_factory: EvmFactory,

    // Gnosis-specific fields to be used in GnosisBlockExecutor
    block_rewards_address: Address,
}

impl<R, EvmFactory> GnosisBlockExecutorFactory<R, EvmFactory> {
    /// Creates a new [`GnosisBlockExecutorFactory`] with the given spec, [`EvmFactory`], and
    /// [`ReceiptBuilder`].
    pub const fn new(
        receipt_builder: R,
        spec: GnosisChainSpec,
        evm_factory: EvmFactory,
        block_rewards_address: Address,
    ) -> Self {
        Self {
            receipt_builder,
            spec,
            evm_factory,
            block_rewards_address,
        }
    }

    /// Exposes the receipt builder.
    pub const fn receipt_builder(&self) -> &R {
        &self.receipt_builder
    }

    /// Exposes the chain specification.
    pub const fn spec(&self) -> &GnosisChainSpec {
        &self.spec
    }

    /// Exposes the EVM factory.
    pub const fn evm_factory(&self) -> &EvmFactory {
        &self.evm_factory
    }
}

impl<R, EvmF> BlockExecutorFactory for GnosisBlockExecutorFactory<R, EvmF>
where
    R: ReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt<Log = Log>>,
    EvmF: EvmFactory<Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>>,
    Self: 'static,
{
    type EvmFactory = EvmF;
    type ExecutionCtx<'a> = GnosisBlockExecutionCtx<'a>;
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;

    fn evm_factory(&self) -> &Self::EvmFactory {
        &self.evm_factory
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: EvmF::Evm<&'a mut State<DB>, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> impl BlockExecutorFor<'a, Self, DB, I>
    where
        DB: Database + 'a,
        I: Inspector<EvmF::Context<&'a mut State<DB>>> + 'a,
    {
        GnosisBlockExecutor::new(
            evm,
            ctx,
            &self.spec,
            &self.receipt_builder,
            self.block_rewards_address,
        )
    }
}
