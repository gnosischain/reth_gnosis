use std::{any::Any, borrow::Cow, sync::Arc};

use alloy_consensus::{
    transaction::{TransactionEnvelope, TxHashRef},
    Transaction, TxReceipt,
};
use alloy_eip7928::{BlockAccessIndex, BlockAccessList};
use alloy_eips::{
    eip4895::Withdrawals, eip6110::DEPOSIT_REQUEST_TYPE, eip7002::WITHDRAWAL_REQUEST_TYPE,
    eip7251::CONSOLIDATION_REQUEST_TYPE, eip7685::Requests,
};
use alloy_primitives::{map::AddressMap, Address, Bytes, Log, TxKind, B256, U256};
use evm2::{
    ethereum::intrinsic_and_floor_gas,
    evm::{
        AccountInfo, Bal, PendingState, SystemTx, BEACON_ROOTS_ADDRESS,
        CONSOLIDATION_REQUEST_ADDRESS, HISTORY_STORAGE_ADDRESS, SYSTEM_ADDRESS,
        SYSTEM_CALL_GAS_LIMIT, WITHDRAWAL_REQUEST_ADDRESS,
    },
    registry::HandlerError,
    AnyError, Evm, EvmFeatures, TxResult, TxResultWithState,
};
use reth_chainspec::EthereumHardforks;
use reth_evm::{
    execute::{map_database_error, map_handler_error},
    BlockExecutionError, BlockExecutionOutput, BlockExecutor, BlockExecutorFactory,
    BlockTransactionResult, BlockValidationError, CommitChanges, ExecutorTx, GasOutput,
    InternalBlockExecutionError, InvalidTxError, ReceiptBuilder, ReceiptBuilderCtx, RecoveredTx,
};
use reth_evm_ethereum::RethReceiptBuilder;
use reth_execution_types::{BlockExecutionResult, BlockState, StateUpdate};

use crate::errors::GnosisBlockExecutionError;
use crate::evm::{factory::GnosisEvmEnv, GnosisEvmFactory, GnosisEvmTypes};
use crate::gnosis::{
    block_rewards_call_data, decode_block_rewards, parse_deposit_requests, rewrite_aura_bytecodes,
    rewrite_bytecodes, withdrawals_call_data,
};
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
    pub rolling_finality: Arc<std::sync::Mutex<crate::aura::finality::RollingFinality>>,
    /// POSDAO activation block number. Rolling finality is only consulted
    /// when `block_num >= posdao_transition`. Before POSDAO, `InitiateChange`
    /// events use immediate finalization at `N+1`. Required (no Option,
    /// no sentinel — lifted from the chain spec's `aura.posdaoTransition`).
    pub posdao_transition: u64,
    /// AuRa pre-merge bytecode rewrites to apply at this exact block, if any.
    pub aura_bytecode_rewrites: Option<std::collections::BTreeMap<Address, Bytes>>,
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

/// `getValidators()` selector.
const GET_VALIDATORS_SELECTOR: [u8; 4] = [0xb7, 0xab, 0x4d, 0xb5];
/// `finalizeChange()` selector.
const FINALIZE_CHANGE_SELECTOR: [u8; 4] = [0x75, 0x28, 0x62, 0x11];
/// `keccak256("InitiateChange(bytes32,address[])")`, emitted by POSDAO validator contracts.
const INITIATE_CHANGE_TOPIC: B256 =
    alloy_primitives::b256!("55252fa6eee4741b4e24a74a70e9c11fd2c2281df8d6ea13126ff845f7825c89");

type StateUpdateHook = Option<Box<dyn FnMut(StateUpdate) + Send>>;

// REF: https://github.com/alloy-rs/evm2/blob/66687c18b34d3bdbb899f5eb7605070615c03475/crates/evm2/src/block/ethereum/mod.rs
// (reth v2.11.0 runs Ethereum blocks on this evm2 executor; its helpers are in `execution.rs`.)
/// Block executor for Gnosis.
///
/// Mirrors evm2's Ethereum `EthBlockExecutor`, which reth's wraps (including state streaming and
/// block access list plumbing), with Gnosis system calls, fee handling and AuRa logic on top.
#[expect(
    missing_debug_implementations,
    reason = "evm2::Evm has no Debug impl for its erased fields"
)]
pub struct GnosisBlockExecutor<'a, R: ReceiptBuilder> {
    /// Reference to the specification object.
    spec: &'a GnosisChainSpec,
    /// Context for block execution.
    pub ctx: GnosisBlockExecutionCtx<'a>,
    /// Inner EVM.
    evm: Evm<'a, GnosisEvmTypes>,
    /// Receipt builder.
    receipt_builder: R,
    /// Block rewards contract used unless `ctx.block_rewards_override` is set.
    block_rewards_address: Address,
    /// Accumulated state changes of the block.
    block_state: BlockState,
    /// Optional hook streaming every committed state change (used for parallel state roots).
    state_update_hook: StateUpdateHook,
    /// Receipts of executed transactions.
    receipts: Vec<R::Receipt>,
    /// Total receipt gas used by transactions in this block.
    cumulative_gas_used: u64,
    /// Regular gas used, for EIP-8037 separate block gas accounting.
    block_regular_gas_used: u64,
    /// State gas used, for EIP-8037 separate block gas accounting.
    block_state_gas_used: u64,
    /// Whether the block meters regular and state gas separately (EIP-8037).
    separate_block_gas: bool,
    /// Blob gas used by the block. Before cancun activation, this is always 0.
    pub blob_gas_used: u64,
}

/// Detached Gnosis transaction result with the metadata needed to commit its receipt.
#[derive(Debug)]
pub struct GnosisTransactionResultWithState<TxType> {
    result: TxResultWithState<GnosisEvmTypes>,
    tx_type: TxType,
    blob_gas_used: u64,
}

impl<TxType> BlockTransactionResult<GnosisEvmTypes> for GnosisTransactionResultWithState<TxType> {
    fn result(&self) -> &TxResultWithState<GnosisEvmTypes> {
        &self.result
    }

    fn into_result(self) -> TxResultWithState<GnosisEvmTypes> {
        self.result
    }
}

impl<'a, R: ReceiptBuilder> GnosisBlockExecutor<'a, R> {
    /// Creates a new [`GnosisBlockExecutor`].
    pub fn new(
        evm: Evm<'a, GnosisEvmTypes>,
        ctx: GnosisBlockExecutionCtx<'a>,
        spec: &'a GnosisChainSpec,
        receipt_builder: R,
        block_rewards_address: Address,
    ) -> Self {
        let separate_block_gas = evm.version().feature(EvmFeatures::EIP8037);
        Self {
            spec,
            ctx,
            evm,
            receipt_builder,
            block_rewards_address,
            block_state: BlockState::new(),
            state_update_hook: None,
            receipts: Vec::new(),
            cumulative_gas_used: 0,
            block_regular_gas_used: 0,
            block_state_gas_used: 0,
            separate_block_gas,
            blob_gas_used: 0,
        }
    }

    /// Returns the accumulated block state.
    pub const fn block_state(&self) -> &BlockState {
        &self.block_state
    }

    fn block_number(&self) -> u64 {
        self.evm.block().number.saturating_to()
    }

    fn block_timestamp(&self) -> u64 {
        self.evm.block().timestamp.saturating_to()
    }

    fn emit_state(&mut self, update: StateUpdate) {
        if update.is_empty() {
            return;
        }
        if let Some(hook) = self.state_update_hook.as_mut() {
            hook(update);
        }
    }

    /// Commits constructed state changes to the EVM overlay and the block state, streaming them
    /// to the state hook.
    ///
    /// Every state change of the block must go through this or [`Self::commit_detached`] /
    /// [`Self::system_call`]: the engine computes the state root from the streamed updates.
    fn commit_pending_state(&mut self, state: &PendingState) {
        let mut update = StateUpdate::default();
        let stream = self.state_update_hook.is_some();
        for changes in state.account_changes() {
            self.block_state
                .commit_account_changes(changes, stream.then_some(&mut update));
        }
        self.evm.overlay_db_mut().commit_pending(state);
        self.emit_state(update);
    }

    /// Commits a detached transaction result and returns its execution outcome.
    fn commit_detached(
        &mut self,
        output: TxResultWithState<GnosisEvmTypes>,
    ) -> TxResult<GnosisEvmTypes> {
        let TxResultWithState {
            result,
            pending_state,
            ..
        } = output;
        let mut update = StateUpdate::default();
        let stream = self.state_update_hook.is_some();
        for changes in pending_state.account_changes() {
            self.block_state
                .commit_account_changes(changes, stream.then_some(&mut update));
        }
        // Reattach the finalized transaction so evm2 recycles its allocations, as reth does.
        self.evm.state_mut().set_pending_state(pending_state);
        self.evm.state_mut().commit_transaction();
        self.emit_state(update);
        result
    }

    /// Runs a system call from `SYSTEM_ADDRESS` to `contract` and commits its state changes,
    /// whatever its outcome.
    ///
    /// Gnosis system calls (Nethermind's `SystemTransaction`) pay no fees and do not bump the
    /// caller nonce, which is evm2's system call semantics. The pre-evm2 implementation ran them
    /// as 30M gas transactions through the regular transaction handler, so this keeps that
    /// handler's observable gas semantics: intrinsic gas is deducted from the 30M limit, and the
    /// caller, the coinbase (from EIP-3651) and the precompiles start warm.
    ///
    /// On top of that, Gnosis exempts `SYSTEM_ADDRESS` from EIP-158 empty-account cleanup (see
    /// gnosischain/go-ethereum `core/state/statedb.go::Finalise`): a system call leaves it in
    /// state as an (empty) account.
    fn system_call(
        &mut self,
        contract: Address,
        data: Bytes,
    ) -> Result<TxResult<GnosisEvmTypes>, BlockExecutionError> {
        let (intrinsic_gas, _floor_gas) = intrinsic_and_floor_gas(
            self.evm.version(),
            SYSTEM_ADDRESS,
            TxKind::Call(contract),
            &data,
            0,
            0,
            U256::ZERO,
        );
        let beneficiary = self.evm.block().beneficiary;
        if self.evm.feature(EvmFeatures::EIP3651) {
            self.evm.state_mut().prewarm(&beneficiary);
        }
        self.evm.state_mut().prewarm(&SYSTEM_ADDRESS);
        self.evm.warm_precompiles();

        let tx = SystemTx::new(contract, data)
            .with_gas_limit(SYSTEM_CALL_GAS_LIMIT.saturating_sub(intrinsic_gas));
        let output = self
            .evm
            .system_call(tx)
            .map_err(map_handler_error)?
            .detach();
        let result = self.commit_detached(output);
        self.ensure_system_address_exists()?;
        Ok(result)
    }

    /// Creates `SYSTEM_ADDRESS` as an empty account if it does not exist yet.
    fn ensure_system_address_exists(&mut self) -> Result<(), BlockExecutionError> {
        if self
            .evm
            .read_account_info(&SYSTEM_ADDRESS)
            .map_err(map_database_error)?
            .is_some()
        {
            return Ok(());
        }
        let mut state = PendingState::default();
        state.insert_account(SYSTEM_ADDRESS, None, Some(AccountInfo::empty()));
        self.commit_pending_state(&state);
        Ok(())
    }

    /// Adds non-zero `increments` to account balances, creating accounts as needed.
    fn increment_balances(
        &mut self,
        increments: AddressMap<U256>,
    ) -> Result<(), BlockExecutionError> {
        let mut state = PendingState::default();
        for (address, increment) in increments {
            // revm's `State::increment_balances` skipped zero increments entirely.
            if increment.is_zero() {
                continue;
            }
            let original = self
                .evm
                .read_account_info(&address)
                .map_err(map_database_error)?;
            let mut current = original.clone().unwrap_or_else(AccountInfo::empty);
            current.balance = current.balance.saturating_add(increment);
            state.insert_account(address, original, Some(current));
        }
        self.commit_pending_state(&state);
        Ok(())
    }

    /// Records a committed transaction's receipt and gas usage.
    fn record_transaction(
        &mut self,
        outcome: TxResult<GnosisEvmTypes>,
        tx_type: <R::Transaction as TransactionEnvelope>::TxType,
        blob_gas_used: u64,
    ) -> GasOutput {
        let tx_gas_used = outcome.tx_gas_used();
        let regular_gas_used = outcome.execution_gas_spent();
        let state_gas_used = outcome.state_gas_spent();
        self.block_regular_gas_used = self.block_regular_gas_used.saturating_add(regular_gas_used);
        self.block_state_gas_used = self.block_state_gas_used.saturating_add(state_gas_used);
        self.cumulative_gas_used += tx_gas_used;
        self.blob_gas_used += blob_gas_used;
        self.receipts
            .push(
                self.receipt_builder
                    .build_receipt::<GnosisEvmTypes>(ReceiptBuilderCtx {
                        tx_type,
                        result: outcome,
                        cumulative_gas_used: self.cumulative_gas_used,
                    }),
            );
        GasOutput::new_with_regular(tx_gas_used, regular_gas_used, state_gas_used)
    }

    const fn block_access_list_builder_enabled(&self) -> bool {
        self.evm.state().bal_builder().is_some()
    }

    #[inline]
    fn set_transaction_block_access_index(&mut self) {
        if self.block_access_list_builder_enabled() {
            let index = self.receipts.len() as u64 + 1;
            self.evm
                .state_mut()
                .set_bal_index(BlockAccessIndex::new(index));
        }
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
        let result = self
            .system_call(
                validator_contract,
                Bytes::from_static(&GET_VALIDATORS_SELECTOR),
            )
            .map_err(|e| {
                aura_error(format!(
                    "AuRa getValidators() syscall failed at block {block_num}: {e}"
                ))
            })?;
        if !result.status {
            return Err(aura_error(format!(
                "AuRa getValidators() at block {block_num} did not succeed: {:?} {}",
                result.stop, result.output
            )));
        }
        let validators = decode_address_array(&result.output).map_err(|_| {
            aura_error(format!(
                "AuRa getValidators() at block {block_num} returned undecodable data"
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
            let mut rf = aura
                .rolling_finality
                .lock()
                .map_err(|_| aura_error("AuRa rolling-finality mutex poisoned".into()))?;
            rf.set_validators(validators);
        }
        Ok(())
    }

    /// Runs the EIP-7002/EIP-7251 request contract call at `address` and returns its output.
    fn request_system_call(&mut self, address: Address) -> Result<Bytes, BlockExecutionError> {
        let result = self.system_call(address, Bytes::new())?;
        if result.status {
            return Ok(result.output);
        }
        let message = if result.is_revert() {
            format!("execution reverted: {}", result.output)
        } else {
            format!("execution halted: {:?}", result.stop)
        };
        Err(match address {
            WITHDRAWAL_REQUEST_ADDRESS => {
                BlockValidationError::WithdrawalRequestsContractCall { message }
            }
            _ => BlockValidationError::ConsolidationRequestsContractCall { message },
        }
        .into())
    }

    /// Gnosis post-block system calls.
    ///
    /// [Gnosis/fork:DIFF]: upstream Ethereum computes balance changes for pre-merge ommer and
    /// block rewards, beacon withdrawal mints, and the DAO hardfork drain. Gnosis instead:
    /// - does NOT credit withdrawals as native token mint,
    /// - calls the deposit contract with the withdrawal data,
    /// - calls the block rewards contract for the bridged xDAI mint.
    ///
    /// Returns the native balance increments and the logs of the block rewards call.
    fn apply_post_block_system_calls(
        &mut self,
        block_rewards_contract: Address,
        withdrawal_contract: Address,
    ) -> Result<(AddressMap<U256>, Vec<Log>), BlockExecutionError> {
        if self
            .spec
            .is_shanghai_active_at_timestamp(self.block_timestamp())
        {
            let call_data = {
                let withdrawals = self.ctx.withdrawals.as_deref().ok_or(
                    GnosisBlockExecutionError::CustomErrorMessage {
                        message: "block has no withdrawals field".to_owned(),
                    },
                )?;
                withdrawals_call_data(withdrawals)
            };
            // Ref: <https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md>
            let result = self
                .system_call(withdrawal_contract, call_data)
                .map_err(|e| {
                    gnosis_error(format!("withdrawal contract system call revert: {e}"))
                })?;
            if !result.status {
                return Err(gnosis_error(if result.is_revert() {
                    format!("execution reverted: {}", result.output)
                } else {
                    format!("execution halted: {:?}", result.stop)
                }));
            }
        }

        // Ref: <https://github.com/gnosischain/specs/blob/master/execution/posdao-post-merge.md>
        //
        // Without contract code there are no rewards, and the call's state changes are not
        // committed (as in the revm implementation).
        let rewards_contract_has_code = self
            .evm
            .read_account_info(&block_rewards_contract)
            .map_err(map_database_error)?
            .is_some_and(|info| !info.is_empty_code_hash());
        if !rewards_contract_has_code {
            return Ok((AddressMap::default(), Vec::new()));
        }

        let coinbase = self.evm.block().beneficiary;
        let result = self
            .system_call(block_rewards_contract, block_rewards_call_data(coinbase))
            .map_err(|e| gnosis_error(format!("block rewards contract system call error: {e}")))?;
        if !result.status {
            return Err(gnosis_error(if result.is_revert() {
                format!(
                    "block rewards contract system call revert {}",
                    result.output
                )
            } else {
                format!("block rewards contract system call halt {:?}", result.stop)
            }));
        }
        let balance_increments = decode_block_rewards(&result.output)?;
        Ok((balance_increments, result.logs))
    }

    /// AuRa-execution-mode-only post-block work: `InitiateChange` detection and signer push into
    /// rolling finality.
    fn apply_aura_post_block(&self, aura: &AuraExecutionCtx, reward_logs: &[Log]) {
        let block_num = self.block_number();
        let is_posdao = block_num >= aura.posdao_transition;

        // Check receipts AND reward system-call logs for InitiateChange events.
        // In POSDAO, the reward contract calls the validator contract which
        // emits InitiateChange. These events are in the reward syscall logs,
        // NOT in user-transaction receipts.
        if let Some(validator_contract) = aura.validator_contract {
            let is_initiate_change = |log: &Log| {
                log.address == validator_contract
                    && log.topics().first() == Some(&INITIATE_CHANGE_TOPIC)
            };
            let has_initiate_change = self
                .receipts
                .iter()
                .any(|receipt| receipt.logs().iter().any(is_initiate_change))
                || reward_logs.iter().any(is_initiate_change);

            if has_initiate_change {
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
            let signer = self.evm.block().beneficiary;
            if let Ok(mut rf) = aura.rolling_finality.lock() {
                rf.push(block_num, signer);
            }
        }
    }
}

fn aura_error(message: String) -> BlockExecutionError {
    BlockExecutionError::Internal(InternalBlockExecutionError::Other(message.into()))
}

fn gnosis_error(message: String) -> BlockExecutionError {
    GnosisBlockExecutionError::CustomErrorMessage { message }.into()
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

const fn final_block_gas_used(
    separate_block_gas: bool,
    cumulative_gas_used: u64,
    block_regular_gas_used: u64,
    block_state_gas_used: u64,
) -> u64 {
    if !separate_block_gas {
        cumulative_gas_used
    } else if block_regular_gas_used > block_state_gas_used {
        block_regular_gas_used
    } else {
        block_state_gas_used
    }
}

// REF: https://github.com/alloy-rs/evm2/blob/66687c18b34d3bdbb899f5eb7605070615c03475/crates/evm2/src/block/ethereum/mod.rs
// (reth v2.11.0 runs Ethereum blocks on this evm2 executor; its helpers are in `execution.rs`.)
// Along with the usual logic, we introduce some Gnosis-specific logic here (Denoted as such)
impl<'a, R> BlockExecutor for GnosisBlockExecutor<'a, R>
where
    R: ReceiptBuilder,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = Evm<'a, GnosisEvmTypes>;
    type TransactionResultWithState =
        GnosisTransactionResultWithState<<R::Transaction as TransactionEnvelope>::TxType>;
    type BlockAccessList = Bal;

    fn evm(&self) -> &Self::Evm {
        &self.evm
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        &mut self.evm
    }

    fn set_state_hook(&mut self, hook: impl FnMut(StateUpdate) + Send + 'static) -> bool {
        self.state_update_hook = Some(Box::new(hook));
        true
    }

    fn convert_block_access_list(
        block_access_list: &BlockAccessList,
    ) -> Result<Self::BlockAccessList, BlockExecutionError> {
        Bal::try_from(block_access_list.as_slice()).map_err(BlockExecutionError::other)
    }

    fn set_block_access_list(&mut self, block_access_list: Arc<Self::BlockAccessList>) {
        self.evm.state_mut().set_bal(block_access_list);
    }

    fn set_block_access_index(&mut self, index: BlockAccessIndex) {
        self.evm.state_mut().set_bal_index(index);
    }

    fn enable_block_access_list_builder(&mut self) {
        self.evm.state_mut().enable_bal_builder();
        self.evm.state_mut().reset_bal_index();
    }

    fn take_block_access_list(&mut self) -> Option<BlockAccessList> {
        self.evm.state_mut().take_bal_builder().map(Into::into)
    }

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        let block_num = self.block_number();

        // AuRa-execution-mode block (= pre-merge block of an AuRa chain).
        // For post-merge blocks and non-AuRa chains (including chains that
        // are post-merge from genesis), `self.ctx.aura` is None and this
        // entire branch is skipped — execution falls through to the standard
        // blockhashes / beacon-root system calls below.
        if let Some(aura) = self.ctx.aura.clone() {
            let is_posdao = block_num >= aura.posdao_transition;

            // Initialize validator set via getValidators() if empty.
            // The system call result IS committed — it's a view function so
            // the only state change is SYSTEM_ADDRESS creation.
            if is_posdao {
                if let Some(validator_contract) = aura.validator_contract {
                    let needs_init = aura
                        .rolling_finality
                        .lock()
                        .map_err(|_| aura_error("AuRa rolling-finality mutex poisoned".into()))?
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
                let state =
                    rewrite_aura_bytecodes(&mut self.evm, rewrites).map_err(map_database_error)?;
                self.commit_pending_state(&state);
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
                // The outcome is not checked: the call's state changes are committed either way.
                let _ = self
                    .system_call(
                        validator_contract,
                        Bytes::from_static(&FINALIZE_CHANGE_SELECTOR),
                    )
                    .map_err(|e| {
                        aura_error(format!(
                            "AuRa finalizeChange() syscall failed at block {block_num}: {e}"
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
        let current_timestamp = self.block_timestamp();
        let is_balancer_active_now = self
            .spec
            .is_balancer_hardfork_active_at_timestamp(current_timestamp);
        let was_balancer_active_in_parent = self
            .spec
            .is_balancer_hardfork_active_at_timestamp(self.ctx.parent_timestamp);
        if is_balancer_active_now && !was_balancer_active_in_parent {
            if let Some(config) = self.spec.balancer_hardfork_config.as_ref() {
                let state = rewrite_bytecodes(&mut self.evm, config).map_err(map_database_error)?;
                self.commit_pending_state(&state);
            }
        }

        // EIP-2935: history storage. Reverts and halts are accepted.
        if self.spec.is_prague_active_at_timestamp(current_timestamp) && block_num != 0 {
            let _ = self.system_call(HISTORY_STORAGE_ADDRESS, self.ctx.parent_hash.0.into())?;
        }

        // EIP-4788: beacon roots. Reverts and halts are accepted.
        if self.spec.is_cancun_active_at_timestamp(current_timestamp) {
            let parent_beacon_block_root = self
                .ctx
                .parent_beacon_block_root
                .ok_or(BlockValidationError::MissingParentBeaconBlockRoot)?;
            if block_num == 0 {
                if !parent_beacon_block_root.is_zero() {
                    return Err(
                        BlockValidationError::CancunGenesisParentBeaconBlockRootNotZero {
                            parent_beacon_block_root,
                        }
                        .into(),
                    );
                }
            } else {
                let _ =
                    self.system_call(BEACON_ROOTS_ADDRESS, parent_beacon_block_root.0.into())?;
            }
        }

        Ok(())
    }

    fn into_state(self) -> reth_evm::BundleState {
        self.block_state.into_bundle()
    }

    fn validate_transaction_gas_limit(
        &mut self,
        transaction_gas_limit: u64,
    ) -> Result<(), BlockExecutionError> {
        // The sum of the transaction's gas limit, Tg, and the gas utilized in this block prior,
        // must be no greater than the block's gasLimit.
        let block_gas_limit = self.evm.block().gas_limit.saturating_to::<u64>();
        let regular_limit = transaction_gas_limit.min(self.evm.version().tx_gas_limit_cap);
        let unavailable = if self.separate_block_gas {
            let regular_available = block_gas_limit.saturating_sub(self.block_regular_gas_used);
            let state_available = block_gas_limit.saturating_sub(self.block_state_gas_used);
            if regular_limit > regular_available {
                Some((transaction_gas_limit, regular_available))
            } else if transaction_gas_limit > state_available {
                Some((transaction_gas_limit, state_available))
            } else {
                None
            }
        } else {
            let available = block_gas_limit.saturating_sub(self.cumulative_gas_used);
            (regular_limit > available).then_some((transaction_gas_limit, available))
        };
        if let Some((transaction_gas_limit, block_available_gas)) = unavailable {
            return Err(
                BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                    transaction_gas_limit,
                    block_available_gas,
                }
                .into(),
            );
        }
        Ok(())
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        transaction: impl ExecutorTx<Self>,
        commit: impl FnOnce(&TxResult<GnosisEvmTypes>, evm2::TxStateChanges<'_>) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let (transaction, tx) = transaction.into_parts();
        self.set_transaction_block_access_index();
        self.validate_transaction_gas_limit(tx.tx().gas_limit())?;
        let blob_gas_used = tx.tx().blob_gas_used().unwrap_or_default();
        let tx_type = tx.tx().tx_type();

        let stream = self.state_update_hook.is_some();
        let mut update = StateUpdate::default();
        let executed = self
            .evm
            .transact(&transaction)
            .map_err(|err| map_transaction_error(err, *tx.tx().tx_hash()))?;
        if !commit(executed.result(), executed.changes()).should_commit() {
            let _ = executed.discard();
            return Ok(None);
        }
        for changes in executed.account_changes() {
            self.block_state
                .commit_account_changes(changes, stream.then_some(&mut update));
        }
        let outcome = executed.commit();
        self.emit_state(update);
        Ok(Some(self.record_transaction(
            outcome,
            tx_type,
            blob_gas_used,
        )))
    }

    fn execute_transaction_without_commit(
        &mut self,
        transaction: impl ExecutorTx<Self>,
    ) -> Result<Self::TransactionResultWithState, BlockExecutionError> {
        let (transaction, tx) = transaction.into_parts();
        self.set_transaction_block_access_index();
        self.validate_transaction_gas_limit(tx.tx().gas_limit())?;
        let blob_gas_used = tx.tx().blob_gas_used().unwrap_or_default();
        let tx_type = tx.tx().tx_type();
        let result = self
            .evm
            .transact(&transaction)
            .map(|executed| executed.detach())
            .map_err(|err| map_transaction_error(err, *tx.tx().tx_hash()))?;
        Ok(GnosisTransactionResultWithState {
            result,
            tx_type,
            blob_gas_used,
        })
    }

    fn commit_transaction(
        &mut self,
        output: Self::TransactionResultWithState,
    ) -> Result<GasOutput, BlockExecutionError> {
        let GnosisTransactionResultWithState {
            result,
            tx_type,
            blob_gas_used,
        } = output;
        self.set_transaction_block_access_index();
        let outcome = self.commit_detached(result);
        Ok(self.record_transaction(outcome, tx_type, blob_gas_used))
    }

    fn receipts(&self) -> &[Self::Receipt] {
        &self.receipts
    }

    fn finish_with_block_access_list(
        self,
    ) -> Result<(BlockExecutionOutput<Self::Receipt>, Option<BlockAccessList>), BlockExecutionError>
    {
        let (output, bal) = self.finish_with_prepared_block_access_list()?;
        Ok((output, bal.map(Into::into)))
    }

    fn finish_with_prepared_block_access_list(
        mut self,
    ) -> Result<(BlockExecutionOutput<Self::Receipt>, Option<Bal>), BlockExecutionError> {
        self.set_transaction_block_access_index();
        let deposit_contract = self.spec.deposit_contract_address().unwrap_or_else(|| {
            panic!("Deposit contract address is not set in the chain specification");
        });

        let requests = if self
            .spec
            .is_prague_active_at_timestamp(self.block_timestamp())
        {
            let mut requests = Requests::default();

            // EIP-6110 deposits
            let deposit_requests = parse_deposit_requests(deposit_contract, &self.receipts)?;
            if !deposit_requests.is_empty() {
                requests.push_request_with_type(DEPOSIT_REQUEST_TYPE, deposit_requests);
            }

            // EIP-7002 withdrawal requests
            let withdrawal_requests = self.request_system_call(WITHDRAWAL_REQUEST_ADDRESS)?;
            if !withdrawal_requests.is_empty() {
                requests.push_request_with_type(WITHDRAWAL_REQUEST_TYPE, withdrawal_requests);
            }

            // EIP-7251 consolidation requests
            let consolidation_requests = self.request_system_call(CONSOLIDATION_REQUEST_ADDRESS)?;
            if !consolidation_requests.is_empty() {
                requests.push_request_with_type(CONSOLIDATION_REQUEST_TYPE, consolidation_requests);
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
        let (balance_increments, reward_logs) =
            self.apply_post_block_system_calls(reward_address, deposit_contract)?;

        if let Some(aura) = &self.ctx.aura {
            self.apply_aura_post_block(aura, &reward_logs);
        }
        // Gnosis-specific // End

        self.increment_balances(balance_increments)?;

        let block_access_list = self.evm.state_mut().take_bal_builder();
        let gas_used = final_block_gas_used(
            self.separate_block_gas,
            self.cumulative_gas_used,
            self.block_regular_gas_used,
            self.block_state_gas_used,
        );
        let output = BlockExecutionOutput::new(
            BlockExecutionResult {
                receipts: self.receipts,
                requests,
                gas_used,
                blob_gas_used: self.blob_gas_used,
            },
            self.block_state.into_bundle(),
        );
        Ok((output, block_access_list))
    }
}

/// Transaction validation error returned by the evm2 transaction handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GnosisInvalidTxError(HandlerError);

impl core::fmt::Display for GnosisInvalidTxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl core::error::Error for GnosisInvalidTxError {}

impl InvalidTxError for GnosisInvalidTxError {
    fn is_nonce_too_low(&self) -> bool {
        matches!(self.0, HandlerError::InvalidNonce { expected, got } if got < expected)
    }

    fn is_gas_limit_too_high(&self) -> bool {
        matches!(
            self.0,
            HandlerError::GasLimitMoreThanBlock { .. }
                | HandlerError::TxGasLimitGreaterThanCap { .. }
        )
    }

    fn is_gas_limit_too_low(&self) -> bool {
        matches!(
            self.0,
            HandlerError::IntrinsicGasTooLow { .. } | HandlerError::GasLimitBelowFloor { .. }
        )
    }

    /// Exposes the wrapped [`HandlerError`] so RPC layers can recover the structured error.
    fn as_any(&self) -> &(dyn Any + 'static) {
        &self.0
    }
}

/// Maps a transaction handler error to a block error, as reth's Ethereum executor does:
/// validation failures become `InvalidTx`, database and fatal errors keep their classification.
fn map_transaction_error(error: HandlerError, hash: B256) -> BlockExecutionError {
    match error {
        HandlerError::Database(database_error) if database_error.is_fatal() => {
            InternalBlockExecutionError::EVM {
                hash,
                error: AnyError::new(HandlerError::Database(database_error)),
            }
            .into()
        }
        HandlerError::Database(database_error) => BlockValidationError::EVM {
            hash,
            error: AnyError::new(HandlerError::Database(database_error)),
        }
        .into(),
        error @ (HandlerError::Fatal(_) | HandlerError::WrongTransactionType { .. }) => {
            InternalBlockExecutionError::EVM {
                hash,
                error: AnyError::new(error),
            }
            .into()
        }
        error => BlockValidationError::InvalidTx {
            hash,
            error: Box::new(GnosisInvalidTxError(error)),
        }
        .into(),
    }
}

/// Gnosis block executor factory.
#[derive(Debug, Clone, Default)]
pub struct GnosisBlockExecutorFactory<R = RethReceiptBuilder> {
    /// Receipt builder.
    receipt_builder: R,
    /// Chain specification.
    spec: GnosisChainSpec,
    /// EVM factory.
    evm_factory: GnosisEvmFactory,

    // Gnosis-specific fields to be used in GnosisBlockExecutor
    block_rewards_address: Address,
}

impl<R> GnosisBlockExecutorFactory<R> {
    /// Creates a new [`GnosisBlockExecutorFactory`] with the given spec, [`GnosisEvmFactory`], and
    /// [`ReceiptBuilder`].
    pub const fn new(
        receipt_builder: R,
        spec: GnosisChainSpec,
        evm_factory: GnosisEvmFactory,
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
    pub const fn evm_factory(&self) -> &GnosisEvmFactory {
        &self.evm_factory
    }
}

impl<R> BlockExecutorFactory for GnosisBlockExecutorFactory<R>
where
    R: ReceiptBuilder + 'static,
{
    type EvmFactory = GnosisEvmFactory;
    type EvmTypes = GnosisEvmTypes;
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm<'a> = Evm<'a, GnosisEvmTypes>;
    type EvmEnv = GnosisEvmEnv;
    type ExecutionCtx<'a>
        = GnosisBlockExecutionCtx<'a>
    where
        Self: 'a;
    type Executor<'a>
        = GnosisBlockExecutor<'a, &'a R>
    where
        Self: 'a;

    fn create_executor<'a>(
        &'a self,
        evm: Self::Evm<'a>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> Self::Executor<'a>
    where
        Self: 'a,
    {
        GnosisBlockExecutor::new(
            evm,
            ctx,
            &self.spec,
            &self.receipt_builder,
            self.block_rewards_address,
        )
    }

    fn evm_factory(&self) -> &Self::EvmFactory {
        &self.evm_factory
    }

    fn evm_with_env<'a, DB>(&self, db: DB, evm_env: Self::EvmEnv) -> Self::Evm<'a>
    where
        DB: evm2::evm::DynDatabase + 'a,
    {
        self.evm_factory.create_evm(db, evm_env)
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
