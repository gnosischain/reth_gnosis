use std::borrow::Cow;
use std::collections::HashMap;

use alloy_consensus::{Transaction, TxReceipt};
use alloy_eips::{eip7002, eip7251};
use alloy_evm::{Database, Evm};
use alloy_evm::{block::state_changes::balance_increment_state, eth::eip6110::{self, parse_deposits_from_receipts}};
use alloy_eips::{eip7685::Requests, Encodable2718};
use reth_errors::{BlockExecutionError, BlockValidationError};
use reth_primitives::Recovered;
use reth_provider::BlockExecutionResult;
use revm::context::Block;
use revm_database::State;
use reth_evm::{block::{BlockExecutor, BlockExecutorFactory, BlockExecutorFor, StateChangePostBlockSource, StateChangeSource, SystemCaller}, eth::{receipt_builder::{AlloyReceiptBuilder, ReceiptBuilder, ReceiptBuilderCtx}, spec::{EthExecutorSpec, EthSpec}, EthBlockExecutionCtx}, state_change::post_block_balance_increments, EvmFactory, FromRecoveredTx, OnStateHook};
use revm::{context::result::{ExecutionResult, ResultAndState}, DatabaseCommit, Inspector};
use revm_primitives::{address, Address, Log};

use crate::evm::evm::GnosisEvmFactory;
use crate::gnosis::apply_post_block_system_calls;

const BLOCK_REWARDS_CONTRACT: Address = address!("2000000000000000000000000000000000000001");
const FEE_COLLECTOR: Address = address!("1559000000000000000000000000000000000000");

/// Block executor for Ethereum.
#[derive(Debug)]
pub struct GnosisBlockExecutor<'a, Evm, Spec, R: ReceiptBuilder> {
    /// Reference to the specification object.
    spec: Spec,

    /// Context for block execution.
    pub ctx: EthBlockExecutionCtx<'a>,
    /// Inner EVM.
    evm: Evm,
    /// Utility to call system smart contracts.
    system_caller: SystemCaller<Spec>,
    /// Receipt builder.
    receipt_builder: R,

    /// Receipts of executed transactions.
    receipts: Vec<R::Receipt>,
    /// Total gas used by transactions in this block.
    gas_used: u64,
}

impl<'a, Evm, Spec, R> GnosisBlockExecutor<'a, Evm, Spec, R>
where
    Spec: Clone,
    R: ReceiptBuilder,
{
    /// Creates a new [`EthBlockExecutor`]
    pub fn new(evm: Evm, ctx: EthBlockExecutionCtx<'a>, spec: Spec, receipt_builder: R) -> Self {
        Self {
            evm,
            ctx,
            receipts: Vec::new(),
            gas_used: 0,
            system_caller: SystemCaller::new(spec.clone()),
            spec,
            receipt_builder,
        }
    }
}

fn print_type_of<T>(_: &T) {
    //disab dbg!("debjit debug > typename {}", std::any::type_name::<T>());
}

impl<'db, DB, E, Spec, R> BlockExecutor for GnosisBlockExecutor<'_, E, Spec, R>
where
    DB: Database + 'db,
    E: Evm<DB = &'db mut State<DB>, Tx: FromRecoveredTx<R::Transaction>>,
    Spec: EthExecutorSpec,
    R: ReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt<Log = Log>>,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag =
            self.spec.is_spurious_dragon_active_at_block(self.evm.block().number);
        self.evm.db_mut().set_state_clear_flag(state_clear_flag);

        //disab dbg!("debjit debug > applying pre execution changes");

        self.system_caller.apply_blockhashes_contract_call(self.ctx.parent_hash, &mut self.evm)?;
        self.system_caller
            .apply_beacon_root_contract_call(self.ctx.parent_beacon_block_root, &mut self.evm)?;

        Ok(())
    }

    fn execute_transaction_with_result_closure(
        &mut self,
        tx: Recovered<&R::Transaction>,
        f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>),
    ) -> Result<u64, BlockExecutionError> {
        // The sum of the transaction's gas limit, Tg, and the gas utilized in this block prior,
        // must be no greater than the block's gasLimit.
        let block_available_gas = self.evm.block().gas_limit - self.gas_used;
        if tx.gas_limit() > block_available_gas {
            return Err(BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: tx.gas_limit(),
                block_available_gas,
            }
            .into());
        }

        if self.spec.is_prague_active_at_timestamp(self.evm.block().timestamp) {
            let blob_gas = tx.blob_gas_used().unwrap_or(0) as u128;
            let blob_gasprice = self.evm.block().blob_gasprice().unwrap_or(0);
            self.evm
                .db_mut()
                .increment_balances(HashMap::<_, _, revm_primitives::map::foldhash::fast::RandomState>::from_iter(vec![
                    (FEE_COLLECTOR, blob_gas * blob_gasprice),
                ]))
                .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;
        }

        // Execute transaction.
        let result_and_state =
            self.evm.transact(tx).map_err(|err| BlockExecutionError::evm(err, tx.trie_hash()))?;
        self.system_caller
            .on_state(StateChangeSource::Transaction(self.receipts.len()), &result_and_state.state);
        let ResultAndState { result, state } = result_and_state;

        f(&result);

        let gas_used = result.gas_used();

        // append gas used
        self.gas_used += gas_used;

        // Push transaction changeset and calculate header bloom filter for receipt.
        self.receipts.push(self.receipt_builder.build_receipt(ReceiptBuilderCtx {
            tx: &tx,
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

        //disab dbg!("debjit debug > finishing");

        let deposit_contract = self.spec.deposit_contract_address();
        let deposit_contract = deposit_contract.unwrap_or_else(|| {
            panic!("Deposit contract address is not set in the chain specification");
        });
        let timestamp = self.evm.block().timestamp();
        let withdrawals = self.ctx.withdrawals.as_deref();
        let beneficiary = self.evm.block().beneficiary();

        //disab dbg!("debjit debug >", self.spec.is_cancun_active_at_timestamp(self.evm.block().timestamp), self.spec.is_prague_active_at_timestamp(self.evm.block().timestamp));

        let (mut gnosis_balance_increments, withdrawal_requests) = apply_post_block_system_calls(
            &self.spec, 
            BLOCK_REWARDS_CONTRACT, 
            deposit_contract,
            timestamp,
            withdrawals,
            beneficiary,
            &mut self.evm,
        )?;

        let requests = if self.spec.is_prague_active_at_timestamp(self.evm.block().timestamp) {
            // Collect all EIP-6110 deposits
            let deposit_requests =
                parse_deposits_from_receipts(&self.spec, &self.receipts)?;

            let mut requests = Requests::default();

            if !deposit_requests.is_empty() {
                requests.push_request_with_type(eip6110::DEPOSIT_REQUEST_TYPE, deposit_requests);
            }

            // requests.extend(self.system_caller.apply_post_execution_changes(&mut self.evm)?);

            if !withdrawal_requests.is_empty() {
                requests
                    .push_request_with_type(eip7002::WITHDRAWAL_REQUEST_TYPE, withdrawal_requests);
            }

            // Collect all EIP-7251 requests
            let consolidation_requests = self.system_caller.apply_consolidation_requests_contract_call(&mut self.evm)?;
            if !consolidation_requests.is_empty() {
                requests.push_request_with_type(eip7251::CONSOLIDATION_REQUEST_TYPE, consolidation_requests);
            }
            requests
        } else {
            Requests::default()
        };

        let mut balance_increments  = post_block_balance_increments(
            &self.spec,
            self.evm.block(),
            self.ctx.ommers,
            self.ctx.withdrawals.as_deref(),
        );

        for (address, increment) in gnosis_balance_increments.drain() {
            *balance_increments.entry(address).or_default() += increment;
        }

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
            BlockExecutionResult { receipts: self.receipts, requests, gas_used: self.gas_used },
        ))
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.system_caller.with_state_hook(hook);
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        &mut self.evm
    }
}

/// Ethereum block executor factory.
#[derive(Debug, Clone, Default, Copy)]
pub struct GnosisBlockExecutorFactory<
    R = AlloyReceiptBuilder,
    Spec = EthSpec,
    EvmFactory = GnosisEvmFactory,
> {
    /// Receipt builder.
    receipt_builder: R,
    /// Chain specification.
    spec: Spec,
    /// EVM factory.
    evm_factory: EvmFactory,
}

impl<R, Spec, EvmFactory> GnosisBlockExecutorFactory<R, Spec, EvmFactory> {
    /// Creates a new [`EthBlockExecutorFactory`] with the given spec, [`EvmFactory`], and
    /// [`ReceiptBuilder`].
    pub const fn new(receipt_builder: R, spec: Spec, evm_factory: EvmFactory) -> Self {
        Self { receipt_builder, spec, evm_factory }
    }

    /// Exposes the receipt builder.
    pub const fn receipt_builder(&self) -> &R {
        &self.receipt_builder
    }

    /// Exposes the chain specification.
    pub const fn spec(&self) -> &Spec {
        &self.spec
    }

    /// Exposes the EVM factory.
    pub const fn evm_factory(&self) -> &EvmFactory {
        &self.evm_factory
    }
}

impl<R, Spec, EvmF> BlockExecutorFactory for GnosisBlockExecutorFactory<R, Spec, EvmF>
where
    R: ReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt<Log = Log>>,
    Spec: EthExecutorSpec,
    EvmF: EvmFactory<Tx: FromRecoveredTx<R::Transaction>>,
    Self: 'static,
{
    type EvmFactory = EvmF;
    type ExecutionCtx<'a> = EthBlockExecutionCtx<'a>;
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
        //disab dbg!("debjit debug > creating executor");
        GnosisBlockExecutor::new(evm, ctx, &self.spec, &self.receipt_builder)
    }
}
