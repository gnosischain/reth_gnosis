use std::borrow::Cow;

use alloy_consensus::{Transaction, TransactionEnvelope, TxReceipt};
use alloy_eips::eip4895::Withdrawals;
use alloy_eips::eip7002::WITHDRAWAL_REQUEST_TYPE;
use alloy_eips::eip7251;
use alloy_eips::{eip7685::Requests, Encodable2718};
use alloy_evm::block::ExecutableTx;
use alloy_evm::eth::EthTxResult;
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
    EvmFactory, FromRecoveredTx, OnStateHook, RecoveredTx,
};
use reth_provider::BlockExecutionResult;
use revm::context::Block;
use revm::{context::result::ResultAndState, DatabaseCommit, Inspector};
use revm_database::{DatabaseCommitExt, State};
use revm_primitives::{Address, Log};

use crate::evm::factory::GnosisEvmFactory;
use crate::gnosis::{apply_post_block_system_calls, rewrite_bytecodes};
use crate::spec::gnosis_spec::{GnosisChainSpec, GnosisHardForks};

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
    /// If set, call finalizeChange() on this validator contract address.
    /// This is needed at the first block of a new validator epoch (when the validator
    /// set type transitions from list to safeContract/contract).
    pub finalize_change_address: Option<Address>,
    /// Validator contract address for InitiateChange event detection.
    /// If set, after block execution, check receipts for InitiateChange events
    /// from this address and set pending_finalize for the next block.
    pub validator_contract: Option<Address>,
    /// Rolling finality tracker for InitiateChange finalization (POSDAO only).
    pub rolling_finality: std::sync::Arc<std::sync::Mutex<crate::aura::finality::RollingFinality>>,
    /// POSDAO transition block number. Rolling finality is only used after this block.
    /// Before POSDAO, InitiateChange events use immediate finalization (N+1).
    pub posdao_transition: Option<u64>,
    /// Override for block rewards contract address (from AuRa transitions).
    /// If set, overrides the fixed address in the executor factory.
    pub block_rewards_override: Option<Address>,
    /// AuRa pre-merge bytecode rewrites to apply at this exact block (if any).
    /// Map of contract_address -> new_bytecode.
    pub aura_bytecode_rewrites:
        Option<std::collections::BTreeMap<Address, alloy_primitives::Bytes>>,
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

    /// Blob gas used by the block.
    /// Before cancun activation, this is always 0.
    pub blob_gas_used: u64,

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
            blob_gas_used: 0,
            system_caller: SystemCaller::new(spec.clone()),
            spec: spec.clone(),
            receipt_builder,
            block_rewards_address,
        }
    }

    /// Decode an ABI-encoded address array from getValidators() return data.
    /// Format: offset(32) + length(32) + addresses(32 each, zero-padded).
    fn decode_address_array(data: &[u8]) -> Result<Vec<Address>, ()> {
        if data.len() < 64 {
            return Err(());
        }
        let offset = u64::from_be_bytes(data[24..32].try_into().map_err(|_| ())?) as usize;
        if offset + 32 > data.len() {
            return Err(());
        }
        let length =
            u64::from_be_bytes(data[offset + 24..offset + 32].try_into().map_err(|_| ())?) as usize;
        let mut addresses = Vec::with_capacity(length);
        for i in 0..length {
            let start = offset + 32 + i * 32;
            if start + 32 > data.len() {
                return Err(());
            }
            let addr = Address::from_slice(&data[start + 12..start + 32]);
            addresses.push(addr);
        }
        Ok(addresses)
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
    type Result = EthTxResult<E::HaltReason, <R::Transaction as TransactionEnvelope>::TxType>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        // Initialize rolling finality tracker if needed (POSDAO only).
        let block_num = self.evm.block().number().to::<u64>();
        let is_posdao = self.ctx.posdao_transition.is_some_and(|t| block_num >= t);

        // Initialize validator set via getValidators() if empty.
        // The transact_system_call result IS committed — it's a view function so
        // the only state changes are cache entries (nonce/beneficiary cleaned up by
        // transact_system_call's cleanup logic).
        if is_posdao {
            if let Some(validator_contract) = self.ctx.validator_contract {
                let needs_init = self
                    .ctx
                    .rolling_finality
                    .lock()
                    .map(|rf| rf.validator_count() == 0)
                    .unwrap_or(false);
                if needs_init {
                    self.evm.db_mut().set_state_clear_flag(false);
                    let get_validators_data =
                        alloy_primitives::Bytes::from_static(&[0xb7, 0xab, 0x4d, 0xb5]);
                    if let Ok(revm::context::result::ResultAndState { result, state }) =
                        self.evm.transact_system_call(
                            alloy_eips::eip4788::SYSTEM_ADDRESS,
                            validator_contract,
                            get_validators_data,
                        )
                    {
                        self.evm.db_mut().commit(state);
                        if let revm::context::result::ExecutionResult::Success { output, .. } =
                            result
                        {
                            if let Ok(validators) = Self::decode_address_array(&output.into_data())
                            {
                                tracing::info!(
                                    target: "reth::gnosis",
                                    block = block_num,
                                    num_validators = validators.len(),
                                    "Initialized rolling finality via getValidators()"
                                );
                                if let Ok(mut rf) = self.ctx.rolling_finality.lock() {
                                    rf.set_validators(validators);
                                }
                            }
                        }
                    }
                    let state_clear_flag = self.spec.is_spurious_dragon_active_at_block(block_num);
                    self.evm.db_mut().set_state_clear_flag(state_clear_flag);
                }
            }
        }

        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag = self
            .spec
            .is_spurious_dragon_active_at_block(self.evm.block().number().saturating_to());
        self.evm.db_mut().set_state_clear_flag(state_clear_flag);

        // Only apply bytecode rewrites at the hardfork activation block
        // (active in current block but NOT active in parent block)
        let current_timestamp: u64 = self.evm.block().timestamp().to();
        let is_balancer_active_now = self
            .spec
            .is_balancer_hardfork_active_at_timestamp(current_timestamp);
        let was_balancer_active_in_parent = self
            .spec
            .is_balancer_hardfork_active_at_timestamp(self.ctx.parent_timestamp);

        if is_balancer_active_now && !was_balancer_active_in_parent {
            if let Some(config) = self.spec.balancer_hardfork_config.as_ref() {
                rewrite_bytecodes(&mut self.evm, config);
            }
        }

        // AuRa pre-merge bytecode rewrites at specific block heights
        // (e.g., Gnosis token contract upgrade at block 21,735,000).
        if let Some(rewrites) = self.ctx.aura_bytecode_rewrites.as_ref() {
            tracing::info!(
                target: "reth::gnosis",
                block = self.evm.block().number().to::<u64>(),
                count = rewrites.len(),
                "Applying AuRa bytecode rewrites"
            );
            crate::gnosis::rewrite_aura_bytecodes(&mut self.evm, rewrites);
        }

        // AuRa: call finalizeChange() on validator contract at epoch boundaries.
        // This must happen before any other execution in the block.
        if let Some(validator_contract) = self.ctx.finalize_change_address {
            let block_num: u64 = self.evm.block().number().to();
            tracing::info!(
                target: "reth::gnosis",
                block = block_num,
                validator = %validator_contract,
                "Calling finalizeChange() on validator contract"
            );
            // Nethermind: EIP-158 (state clear) is DISABLED for AuRa system calls.
            self.evm.db_mut().set_state_clear_flag(false);

            // finalizeChange() selector = 0x75286211
            let finalize_data = alloy_primitives::Bytes::from_static(&[0x75, 0x28, 0x62, 0x11]);
            let result = self.evm.transact_system_call(
                alloy_eips::eip4788::SYSTEM_ADDRESS,
                validator_contract,
                finalize_data,
            );
            match result {
                Ok(revm::context::result::ResultAndState { state, .. }) => {
                    self.evm.db_mut().commit(state);

                    // After finalizeChange, refresh the active validator set by calling
                    // getValidators(). This is a view function — the transact_system_call
                    // cleanup reverts nonce/beneficiary/fee_collector changes. The commit
                    // only adds read-cache entries (no actual state modifications).
                    let get_validators_data =
                        alloy_primitives::Bytes::from_static(&[0xb7, 0xab, 0x4d, 0xb5]);
                    if let Ok(revm::context::result::ResultAndState {
                        result: vr,
                        state: vs,
                    }) = self.evm.transact_system_call(
                        alloy_eips::eip4788::SYSTEM_ADDRESS,
                        validator_contract,
                        get_validators_data,
                    ) {
                        // Commit the read-only state (just cache entries)
                        self.evm.db_mut().commit(vs);
                        if let revm::context::result::ExecutionResult::Success { output, .. } = vr {
                            if let Ok(validators) = Self::decode_address_array(&output.into_data())
                            {
                                tracing::info!(
                                    target: "reth::gnosis",
                                    block = block_num,
                                    num_validators = validators.len(),
                                    "Refreshed validators via getValidators() after finalizeChange"
                                );
                                if let Ok(mut rf) = self.ctx.rolling_finality.lock() {
                                    rf.set_validators(validators);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        target: "reth::gnosis",
                        "finalizeChange() call failed: {e}, continuing"
                    );
                }
            }

            // Restore state clear flag
            let state_clear_flag = self
                .spec
                .is_spurious_dragon_active_at_block(self.evm.block().number().saturating_to());
            self.evm.db_mut().set_state_clear_flag(state_clear_flag);
        }

        self.system_caller
            .apply_blockhashes_contract_call(self.ctx.parent_hash, &mut self.evm)?;
        self.system_caller
            .apply_beacon_root_contract_call(self.ctx.parent_beacon_block_root, &mut self.evm)?;

        Ok(())
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::Result, BlockExecutionError> {
        let (tx_env, tx) = tx.into_parts();

        // The sum of the transaction's gas limit, Tg, and the gas utilized in this block prior,
        // must be no greater than the block's gasLimit.
        let block_available_gas = self.evm.block().gas_limit() - self.gas_used;

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
        let result = self.evm.transact(tx_env).map_err(|err| {
            let hash = tx.tx().trie_hash();
            BlockExecutionError::evm(err, hash)
        })?;

        Ok(EthTxResult {
            result,
            blob_gas_used: tx.tx().blob_gas_used().unwrap_or_default(),
            tx_type: tx.tx().tx_type(),
        })
    }

    fn commit_transaction(&mut self, output: Self::Result) -> Result<u64, BlockExecutionError> {
        let EthTxResult {
            result: ResultAndState { result, state },
            blob_gas_used,
            tx_type,
        } = output;

        self.system_caller
            .on_state(StateChangeSource::Transaction(self.receipts.len()), &state);

        let gas_used = result.gas_used();

        // append gas used
        self.gas_used += gas_used;

        // only determine cancun fields when active
        if self
            .spec
            .is_cancun_active_at_timestamp(self.evm.block().timestamp().saturating_to())
        {
            self.blob_gas_used = self.blob_gas_used.saturating_add(blob_gas_used);
        }

        // Push transaction changeset and calculate header bloom filter for receipt.
        self.receipts
            .push(self.receipt_builder.build_receipt(ReceiptBuilderCtx {
                tx_type,
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
            .is_prague_active_at_timestamp(self.evm.block().timestamp().to())
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
        // Nethermind: EIP-158 (state clear) is DISABLED for AuRa system calls.
        self.evm.db_mut().set_state_clear_flag(false);

        // Use the AuRa-specific reward contract if available, otherwise fall back to default
        let reward_address = self
            .ctx
            .block_rewards_override
            .unwrap_or(self.block_rewards_address);
        let (balance_increments, _, reward_logs) = apply_post_block_system_calls(
            &self.spec,
            reward_address,
            deposit_contract,
            timestamp.to(),
            withdrawals,
            beneficiary,
            &mut self.evm,
            &mut self.system_caller,
        )?;

        // Check receipts AND reward system call logs for InitiateChange events.
        // In POSDAO, the reward contract calls the validator contract which emits
        // InitiateChange. These events are in the reward system call logs, NOT in
        // user transaction receipts.
        // InitiateChange event topic: keccak256("InitiateChange(bytes32,address[])")
        // = 0x55252fa6eee4741b4e24a74a70e9c11fd2c2281df8d6ea13126ff845f7825c89
        if let Some(validator_contract) = self.ctx.validator_contract {
            let initiate_change_topic = alloy_primitives::b256!(
                "55252fa6eee4741b4e24a74a70e9c11fd2c2281df8d6ea13126ff845f7825c89"
            );

            // Check user transaction receipts
            let has_initiate_change_in_receipts = self.receipts.iter().any(|receipt| {
                receipt.logs().iter().any(|log| {
                    log.address == validator_contract
                        && log.topics().first() == Some(&initiate_change_topic)
                })
            });

            // Check reward system call logs (POSDAO: reward() -> validator contract -> InitiateChange)
            let has_initiate_change_in_reward = reward_logs.iter().any(|log| {
                log.address == validator_contract
                    && log.topics().first() == Some(&initiate_change_topic)
            });

            if has_initiate_change_in_receipts || has_initiate_change_in_reward {
                let block_num: u64 = self.evm.block().number().to();
                let is_posdao = self.ctx.posdao_transition.is_some_and(|t| block_num >= t);

                if is_posdao {
                    tracing::info!(
                        target: "reth::gnosis",
                        block = block_num,
                        validator = %validator_contract,
                        "InitiateChange event detected (POSDAO), adding to rolling finality"
                    );
                    if let Ok(mut rf) = self.ctx.rolling_finality.lock() {
                        rf.add_pending_transition(block_num, validator_contract);
                    }
                } else {
                    tracing::info!(
                        target: "reth::gnosis",
                        block = block_num,
                        validator = %validator_contract,
                        "InitiateChange event detected (pre-POSDAO), immediate finalize at N+1"
                    );
                    if let Ok(mut rf) = self.ctx.rolling_finality.lock() {
                        rf.set_immediate_finalize(block_num + 1, validator_contract);
                    }
                }
            }
        }

        // Push this block's signer into the rolling finality tracker (POSDAO only).
        {
            let block_num: u64 = self.evm.block().number().to();
            let is_posdao = self.ctx.posdao_transition.is_some_and(|t| block_num >= t);
            if is_posdao {
                let signer = self.evm.block().beneficiary();
                if let Ok(mut rf) = self.ctx.rolling_finality.lock() {
                    rf.push(block_num, signer);
                }
            }
        }

        // Restore state clear flag for subsequent operations
        let state_clear_flag = self
            .spec
            .is_spurious_dragon_active_at_block(self.evm.block().number().saturating_to());
        self.evm.db_mut().set_state_clear_flag(state_clear_flag);
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
                blob_gas_used: self.blob_gas_used,
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

    fn receipts(&self) -> &[Self::Receipt] {
        &self.receipts
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
