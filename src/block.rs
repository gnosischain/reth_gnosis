use std::borrow::Cow;

use alloy_consensus::{Transaction, TransactionEnvelope, TxReceipt};
use alloy_eips::eip4895::Withdrawals;
use alloy_eips::eip7002::WITHDRAWAL_REQUEST_TYPE;
use alloy_eips::eip7251;
use alloy_eips::{eip7685::Requests, Encodable2718};
use alloy_evm::block::{ExecutableTx, GasOutput, StateDB};
use alloy_evm::eth::EthTxResult;
use alloy_evm::Evm;
use alloy_evm::{
    block::state_changes::balance_increment_state,
    eth::eip6110::{self, parse_deposits_from_receipts},
    FromTxWithEncoded,
};
use alloy_primitives::B256;
use reth_chainspec::EthereumHardforks;
use reth_errors::{BlockExecutionError, BlockValidationError};
use reth_evm::execute::InternalBlockExecutionError;
use reth_evm::{
    block::{
        BlockExecutor, BlockExecutorFactory, StateChangePostBlockSource, StateChangeSource,
        SystemCaller,
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
use revm_database::DatabaseCommitExt;
use revm_primitives::{Address, Log};

use crate::evm::factory::GnosisEvmFactory;
use crate::gnosis::{apply_post_block_system_calls, rewrite_bytecodes};
use crate::spec::gnosis_spec::{GnosisChainSpec, GnosisHardForks};

/// Per-block context for AuRa-execution-mode blocks (pre-merge blocks of an
/// AuRa chain). `Some(...)` exactly when the block is in pre-merge AuRa mode;
/// `None` for post-merge blocks of AuRa chains and for any block of a
/// non-AuRa chain (including chains that are post-merge from genesis).
#[derive(Debug, Clone)]
pub struct AuraExecutionCtx {
    /// If set, call `finalizeChange()` on this validator contract address.
    /// Needed at the first block of a new validator epoch (when the validator
    /// set transitions from list to safe-contract/contract).
    pub finalize_change_address: Option<Address>,
    /// Validator contract address for `InitiateChange` event detection.
    /// If set, after block execution, scan receipts for events from this
    /// address and feed the rolling-finality tracker.
    pub validator_contract: Option<Address>,
    /// Rolling finality tracker for `InitiateChange` finalization (POSDAO).
    pub rolling_finality: std::sync::Arc<std::sync::Mutex<crate::aura::finality::RollingFinality>>,
    /// POSDAO activation block number. Rolling finality is only consulted
    /// when `block_num >= posdao_transition`. Before POSDAO, `InitiateChange`
    /// events use immediate finalization at `N+1`. Required (no Option,
    /// no sentinel — lifted from the chain spec's `aura.posdaoTransition`).
    pub posdao_transition: u64,
    /// AuRa pre-merge bytecode rewrites to apply at this exact block, if any.
    pub aura_bytecode_rewrites:
        Option<std::collections::BTreeMap<Address, alloy_primitives::Bytes>>,
}

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
    /// AuRa-execution-mode context. `Some` only for pre-merge blocks of AuRa
    /// chains. `None` for post-merge blocks (regardless of chain) and for any
    /// block of a chain without an `aura` config section.
    pub aura: Option<AuraExecutionCtx>,
    /// Override for block-rewards contract address from AuRa
    /// `block_reward_contract_transitions`. Stays at top level (not under
    /// `aura`) because Gnosis post-merge still uses the POSDAO reward
    /// contract — the override applies pre- AND post-merge for AuRa chains.
    pub block_rewards_override: Option<Address>,
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
}

/// System-call helpers. Live in their own impl block because they need
/// the `E: Evm<DB: StateDB>` bound that the constructor / decoder above don't.
impl<'a, E, R> GnosisBlockExecutor<'a, E, R>
where
    E: Evm<DB: StateDB>,
    R: ReceiptBuilder,
{
    /// Run a system call from `SYSTEM_ADDRESS` to `contract` and commit the
    /// resulting state diff. SYSTEM_ADDRESS preservation is handled inside
    /// `evm/factory.rs::transact_system_call`.
    fn system_call_and_commit(
        &mut self,
        contract: Address,
        data: alloy_primitives::Bytes,
    ) -> Result<revm::context::result::ExecutionResult<E::HaltReason>, E::Error> {
        let revm::context::result::ResultAndState { result, state } = self
            .evm
            .transact_system_call(alloy_eips::eip4788::SYSTEM_ADDRESS, contract, data)?;
        self.evm.db_mut().commit(state);
        Ok(result)
    }

    /// Call `getValidators()` on the validator contract, commit the result,
    /// decode the returned address list, and seed the rolling-finality tracker.
    ///
    /// Every step is consensus-affecting: a stale local validator set produces
    /// a state-root mismatch when `finalizeChange` next fires, but only after
    /// some unbounded delay. Surfacing the failure here preserves determinism
    /// and gets the operator a clear loader-time/sync-time error instead of a
    /// confusing trie divergence later.
    fn refresh_validators_via_get_validators(
        &mut self,
        validator_contract: Address,
        block_num: u64,
        log_label: &'static str,
    ) -> Result<(), BlockExecutionError> {
        // getValidators() selector = 0xb7ab4db5
        let get_validators_data = alloy_primitives::Bytes::from_static(&[0xb7, 0xab, 0x4d, 0xb5]);
        let result = self
            .system_call_and_commit(validator_contract, get_validators_data)
            .map_err(|e| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("AuRa getValidators() syscall failed at block {block_num}: {e}").into(),
                ))
            })?;
        let output = match result {
            revm::context::result::ExecutionResult::Success { output, .. } => output,
            other => {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!(
                            "AuRa getValidators() at block {block_num} did not succeed: {other:?}"
                        )
                        .into(),
                    ),
                ));
            }
        };
        let validators = decode_address_array(&output.into_data()).map_err(|_| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!("AuRa getValidators() at block {block_num} returned undecodable data")
                    .into(),
            ))
        })?;
        tracing::info!(
            target: "reth::gnosis",
            block = block_num,
            num_validators = validators.len(),
            "{}", log_label,
        );
        // Only callers in the `if let Some(aura) = self.ctx.aura` branch
        // invoke this helper, so `self.ctx.aura` is always `Some` here.
        // Read defensively in case a future caller forgets that contract.
        if let Some(aura) = self.ctx.aura.as_ref() {
            let mut rf = aura.rolling_finality.lock().map_err(|_| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    "AuRa rolling-finality mutex poisoned".into(),
                ))
            })?;
            rf.set_validators(validators);
        }
        Ok(())
    }
}

/// ABI-decode an `address[]` from `getValidators()` return data.
/// Layout: `offset_to_array (32B BE u256) || length (32B BE u256) || addr[length]` where
/// each address occupies 32 bytes (zero-padded high 12 bytes, address in low 20).
/// Note: only the low 8 bytes of each 32-byte word are read; offsets/lengths
/// above 2^64 will silently truncate (acceptable for realistic getValidators data).
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

// REF: https://github.com/alloy-rs/evm/blob/99d5b552c131e3419448c214e09474bf4f0d1e4b/crates/evm/src/eth/block.rs#L81
// ALong with the usual logic, we introduce some Gnosis-specific logic here (Denoted as such)
impl<E, R> BlockExecutor for GnosisBlockExecutor<'_, E, R>
where
    E: Evm<DB: StateDB, Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>>,
    R: ReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt<Log = Log>>,
    <R::Transaction as TransactionEnvelope>::TxType: Send + 'static,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;
    type Result = EthTxResult<E::HaltReason, <R::Transaction as TransactionEnvelope>::TxType>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        let block_num = self.evm.block().number().to::<u64>();

        // AuRa-execution-mode block (= pre-merge block of an AuRa chain).
        // For post-merge blocks and non-AuRa chains (including chains that
        // are post-merge from genesis), `self.ctx.aura` is None and this
        // entire branch is skipped — execution falls through to the standard
        // blockhashes / beacon-root system calls below.
        if let Some(aura) = self.ctx.aura.clone() {
            let is_posdao = block_num >= aura.posdao_transition;

            // Initialize validator set via getValidators() if empty.
            // The transact_system_call result IS committed — it's a view function so
            // the only state changes are cache entries (nonce/beneficiary cleaned up
            // by transact_system_call's cleanup logic).
            if is_posdao {
                if let Some(validator_contract) = aura.validator_contract {
                    let needs_init = aura
                        .rolling_finality
                        .lock()
                        .map_err(|_| {
                            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                                "AuRa rolling-finality mutex poisoned".into(),
                            ))
                        })?
                        .validator_count()
                        == 0;
                    if needs_init {
                        self.refresh_validators_via_get_validators(
                            validator_contract,
                            block_num,
                            "Initialized rolling finality via getValidators()",
                        )?;
                    }
                }
            }

            // AuRa pre-merge bytecode rewrites at specific block heights
            // (e.g., Gnosis token contract upgrade at block 21,735,000).
            if let Some(rewrites) = aura.aura_bytecode_rewrites.as_ref() {
                tracing::info!(
                    target: "reth::gnosis",
                    block = block_num,
                    count = rewrites.len(),
                    "Applying AuRa bytecode rewrites"
                );
                crate::gnosis::rewrite_aura_bytecodes(&mut self.evm, rewrites);
            }

            // AuRa: call finalizeChange() on validator contract at epoch boundaries.
            // This must happen before any other execution in the block.
            if let Some(validator_contract) = aura.finalize_change_address {
                tracing::info!(
                    target: "reth::gnosis",
                    block = block_num,
                    validator = %validator_contract,
                    "Calling finalizeChange() on validator contract"
                );
                // finalizeChange() selector = 0x75286211
                let finalize_data = alloy_primitives::Bytes::from_static(&[0x75, 0x28, 0x62, 0x11]);
                self.system_call_and_commit(validator_contract, finalize_data)
                    .map_err(|e| {
                        BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                            format!(
                                "AuRa finalizeChange() syscall failed at block {block_num}: {e}"
                            )
                            .into(),
                        ))
                    })?;
                // After finalizeChange (POSDAO only), refresh the active validator
                // set via getValidators(). Pre-POSDAO blocks must NOT call this —
                // the committed system-call state pollutes the state trie.
                if is_posdao {
                    self.refresh_validators_via_get_validators(
                        validator_contract,
                        block_num,
                        "Refreshed validators via getValidators() after finalizeChange",
                    )?;
                }
            }
        }

        // Balancer-hardfork bytecode rewrites are chain-level (timestamp-based),
        // not AuRa-specific — applied for both pre- and post-merge blocks.
        // Only fires at the activation block (active in current, not in parent).
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

    fn commit_transaction(&mut self, output: Self::Result) -> GasOutput {
        let EthTxResult {
            result: ResultAndState { result, state },
            blob_gas_used,
            tx_type,
        } = output;

        self.system_caller
            .on_state(StateChangeSource::Transaction(self.receipts.len()), &state);

        let gas_used = result.tx_gas_used();

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

        GasOutput::new(gas_used)
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
        // Use the AuRa-specific reward contract if available, otherwise fall back to default.
        // `block_rewards_override` lives at top level (not under `aura`) because Gnosis
        // post-merge still uses POSDAO reward contracts — applies in both phases.
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

        // AuRa-execution-mode-only post-block work: InitiateChange detection +
        // signer push into rolling finality. Skipped for post-merge blocks and
        // non-AuRa chains.
        if let Some(aura) = &self.ctx.aura {
            let block_num: u64 = self.evm.block().number().to();
            let is_posdao = block_num >= aura.posdao_transition;

            // Check receipts AND reward system-call logs for InitiateChange events.
            // In POSDAO, the reward contract calls the validator contract which
            // emits InitiateChange. These events are in the reward syscall logs,
            // NOT in user-transaction receipts.
            // InitiateChange event topic = keccak256("InitiateChange(bytes32,address[])")
            // = 0x55252fa6eee4741b4e24a74a70e9c11fd2c2281df8d6ea13126ff845f7825c89
            if let Some(validator_contract) = aura.validator_contract {
                let initiate_change_topic = alloy_primitives::b256!(
                    "55252fa6eee4741b4e24a74a70e9c11fd2c2281df8d6ea13126ff845f7825c89"
                );

                let has_initiate_change_in_receipts = self.receipts.iter().any(|receipt| {
                    receipt.logs().iter().any(|log| {
                        log.address == validator_contract
                            && log.topics().first() == Some(&initiate_change_topic)
                    })
                });
                let has_initiate_change_in_reward = reward_logs.iter().any(|log| {
                    log.address == validator_contract
                        && log.topics().first() == Some(&initiate_change_topic)
                });

                if has_initiate_change_in_receipts || has_initiate_change_in_reward {
                    if is_posdao {
                        tracing::info!(
                            target: "reth::gnosis",
                            block = block_num,
                            validator = %validator_contract,
                            "InitiateChange event detected (POSDAO), adding to rolling finality"
                        );
                        if let Ok(mut rf) = aura.rolling_finality.lock() {
                            rf.add_pending_transition(block_num, validator_contract);
                        }
                    } else {
                        tracing::info!(
                            target: "reth::gnosis",
                            block = block_num,
                            validator = %validator_contract,
                            "InitiateChange event detected (pre-POSDAO), immediate finalize at N+1"
                        );
                        if let Ok(mut rf) = aura.rolling_finality.lock() {
                            rf.set_immediate_finalize(block_num + 1, validator_contract);
                        }
                    }
                }
            }

            // Push this block's signer into the rolling finality tracker (POSDAO only).
            if is_posdao {
                let signer = self.evm.block().beneficiary();
                if let Ok(mut rf) = aura.rolling_finality.lock() {
                    rf.push(block_num, signer);
                }
            }
        }

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
    <R::Transaction as TransactionEnvelope>::TxType: Send + 'static,
    Self: 'static,
{
    type EvmFactory = EvmF;
    type ExecutionCtx<'a> = GnosisBlockExecutionCtx<'a>;
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type TxExecutionResult =
        EthTxResult<EvmF::HaltReason, <R::Transaction as TransactionEnvelope>::TxType>;
    type Executor<'a, DB: StateDB, I: Inspector<EvmF::Context<DB>>> =
        GnosisBlockExecutor<'a, EvmF::Evm<DB, I>, &'a R>;

    fn evm_factory(&self) -> &Self::EvmFactory {
        &self.evm_factory
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: EvmF::Evm<DB, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> Self::Executor<'a, DB, I>
    where
        DB: StateDB,
        I: Inspector<EvmF::Context<DB>>,
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

#[cfg(test)]
mod tests {
    use super::decode_address_array;
    use alloy_primitives::Address;

    /// Build an ABI-encoded `address[]` payload for the given addresses.
    fn encode(addresses: &[Address]) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + addresses.len() * 32);
        // offset = 32 (the array starts right after the offset word)
        let mut offset = [0u8; 32];
        offset[31] = 32;
        out.extend_from_slice(&offset);
        // length
        let mut len_word = [0u8; 32];
        let len_be = (addresses.len() as u64).to_be_bytes();
        len_word[24..32].copy_from_slice(&len_be);
        out.extend_from_slice(&len_word);
        // each address — left-padded to 32 bytes
        for a in addresses {
            let mut word = [0u8; 32];
            word[12..32].copy_from_slice(a.as_slice());
            out.extend_from_slice(&word);
        }
        out
    }

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

    #[test]
    fn roundtrip_empty_and_nonempty() {
        for v in [vec![], vec![addr(0x01), addr(0x02), addr(0x03)]] {
            let data = encode(&v);
            assert_eq!(decode_address_array(&data).unwrap(), v);
        }
    }

    /// Each block exercises a distinct fail-branch in `decode_address_array`.
    /// Bundled because the rejection contract is one concept; a regression in
    /// any one path is equally a regression in "decoder rejects malformed input".
    #[test]
    fn rejects_malformed_inputs() {
        // < 64 bytes: can't hold offset + length words.
        assert!(decode_address_array(&[]).is_err(), "empty input");
        assert!(decode_address_array(&[0u8; 63]).is_err(), "63 bytes");

        // Truncated payload: claimed length=2 but only ~1.5 elements present.
        let mut truncated = encode(&[addr(0x01), addr(0x02)]);
        truncated.truncate(truncated.len() - 16);
        assert!(
            decode_address_array(&truncated).is_err(),
            "truncated payload"
        );

        // Offset past end: 1024-byte offset on a 64-byte buffer.
        let mut bad_offset = vec![0u8; 64];
        bad_offset[30] = 0x04; // bytes 24..32 (BE u64) = 1024
        assert!(
            decode_address_array(&bad_offset).is_err(),
            "offset past end"
        );
    }
}
