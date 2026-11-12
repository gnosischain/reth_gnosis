//! Gnosis-specific block processing helpers.
//!
//! The block executor ([`crate::block`]) runs the system calls; this module builds their call data,
//! decodes their results, and computes the state changes for Gnosis bytecode rewrites.

use std::collections::BTreeMap;

use alloy_consensus::TxReceipt;
use alloy_eips::eip4895::Withdrawal;
use alloy_primitives::{map::AddressMap, Address, Bytes, Log, U256};
use alloy_sol_macro::sol;
use alloy_sol_types::{SolCall, SolEvent};
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, PendingState},
    DatabaseError, Evm, EvmTypes,
};
use reth_evm::execute::BlockValidationError;

use crate::errors::GnosisBlockExecutionError;
use crate::spec::gnosis_spec::BalancerHardforkConfig;

// Codegen from https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md
sol!(
    function executeSystemWithdrawals(
        uint256 maxFailedWithdrawalsToProcess,
        uint64[] calldata _amounts,
        address[] calldata _addresses
    );
);

sol!(
    function reward(
        address[] benefactors,
        uint16[] kind
    ) returns(
        address[] receiversNative,
        uint256[] memory rewardsNative
    );
);

sol! {
    #[allow(missing_docs)]
    event DepositEvent(
        bytes pubkey,
        bytes withdrawal_credentials,
        bytes amount,
        bytes signature,
        bytes index
    );
}

/// Number of failed withdrawals the withdrawal contract retries per block, per the Gnosis
/// withdrawals spec.
const MAX_FAILED_WITHDRAWALS_TO_PROCESS: u64 = 4;

/// Size of one EIP-6110 deposit request: 48-byte pubkey, 32-byte withdrawal credentials, 8-byte
/// amount, 96-byte signature, and 8-byte index.
const DEPOSIT_REQUEST_SIZE: usize = 48 + 32 + 8 + 96 + 8;

/// Returns the call data for the post-block withdrawal contract call.
///
/// Ref: <https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md>
pub(crate) fn withdrawals_call_data(withdrawals: &[Withdrawal]) -> Bytes {
    executeSystemWithdrawalsCall {
        maxFailedWithdrawalsToProcess: U256::from(MAX_FAILED_WITHDRAWALS_TO_PROCESS),
        _amounts: withdrawals.iter().map(|w| w.amount).collect(),
        _addresses: withdrawals.iter().map(|w| w.address).collect(),
    }
    .abi_encode()
    .into()
}

/// Returns the call data for the post-block block-rewards contract call.
///
/// Ref: <https://github.com/gnosischain/specs/blob/master/execution/posdao-post-merge.md>
pub(crate) fn block_rewards_call_data(coinbase: Address) -> Bytes {
    rewardCall {
        benefactors: vec![coinbase],
        // Type 0 = RewardAuthor
        kind: vec![0],
    }
    .abi_encode()
    .into()
}

/// Decodes the block-rewards contract output into native balance increments.
pub(crate) fn decode_block_rewards(
    output: &Bytes,
) -> Result<AddressMap<U256>, GnosisBlockExecutionError> {
    let rewards = rewardCall::abi_decode_returns(output).map_err(|e| {
        GnosisBlockExecutionError::CustomErrorMessage {
            message: format!(
                "error parsing block rewards contract system call return {:?}: {}",
                hex::encode(output),
                e
            ),
        }
    })?;

    let mut balance_increments = AddressMap::<U256>::default();
    for (address, amount) in rewards
        .receiversNative
        .iter()
        .zip(rewards.rewardsNative.iter())
    {
        let increment = balance_increments.entry(*address).or_default();
        *increment = increment.saturating_add(*amount);
    }
    Ok(balance_increments)
}

/// Returns the concatenated EIP-6110 deposit requests emitted by `deposit_contract` in
/// `receipts`.
pub(crate) fn parse_deposit_requests<R>(
    deposit_contract: Address,
    receipts: &[R],
) -> Result<Vec<u8>, BlockValidationError>
where
    R: TxReceipt<Log = Log>,
{
    let mut out = Vec::new();
    for log in receipts.iter().flat_map(|receipt| receipt.logs()) {
        if log.address != deposit_contract
            || log.topics().first() != Some(&DepositEvent::SIGNATURE_HASH)
        {
            continue;
        }
        let deposit = DepositEvent::decode_log(log)
            .map_err(|err| BlockValidationError::DepositRequestDecode(err.to_string()))?;
        out.reserve(DEPOSIT_REQUEST_SIZE);
        out.extend_from_slice(deposit.pubkey.as_ref());
        out.extend_from_slice(deposit.withdrawal_credentials.as_ref());
        out.extend_from_slice(deposit.amount.as_ref());
        out.extend_from_slice(deposit.signature.as_ref());
        out.extend_from_slice(deposit.index.as_ref());
    }
    Ok(out)
}

/// Returns the state changes for an AuRa pre-merge bytecode rewrite map.
///
/// Used for chain hardforks like the Gnosis block 21,735,000 token contract upgrade. Accounts
/// that already have the target code are left untouched.
pub fn rewrite_aura_bytecodes<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    rewrites: &BTreeMap<Address, Bytes>,
) -> Result<PendingState, DatabaseError> {
    let mut state = PendingState::default();
    for (address, code) in rewrites {
        let original = evm.read_account_info(address)?;
        let bytecode = Bytecode::new_legacy(code.clone());
        let code_hash = bytecode.hash_slow();
        let original_code_hash = original
            .as_ref()
            .map_or(AccountInfo::empty().code_hash, |info| info.code_hash);
        if original_code_hash == code_hash {
            tracing::trace!(target: "reth::gnosis", %address, "skipping AuRa bytecode rewrite");
            continue;
        }
        tracing::info!(
            target: "reth::gnosis",
            %address,
            from = %original_code_hash,
            to = %code_hash,
            "AuRa bytecode rewrite"
        );
        insert_code_change(&mut state, *address, original, Some(bytecode));
    }
    Ok(state)
}

/// Returns the state changes for the Balancer hardfork bytecode rewrites.
///
/// Accounts that already have the expected code hash are left untouched.
pub fn rewrite_bytecodes<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    balancer_hardfork_config: &BalancerHardforkConfig,
) -> Result<PendingState, DatabaseError> {
    let mut state = PendingState::default();
    for (address, code, expected_code_hash) in &balancer_hardfork_config.config {
        let original = evm.read_account_info(address)?;
        let original_code_hash = original
            .as_ref()
            .map_or(AccountInfo::empty().code_hash, |info| info.code_hash);
        if &original_code_hash == expected_code_hash {
            tracing::trace!(
                target: "reth::gnosis",
                %address,
                code_hash = %expected_code_hash,
                "skipping Balancer bytecode rewrite"
            );
            continue;
        }
        tracing::info!(
            target: "reth::gnosis",
            %address,
            from = %original_code_hash,
            to = %expected_code_hash,
            "Balancer bytecode rewrite"
        );
        insert_code_change(&mut state, *address, original, code.clone());
    }
    Ok(state)
}

/// Records replacing the code of `address` with `code` (or removing it when `None`), keeping its
/// balance and nonce.
///
/// An account left empty by the rewrite is removed (EIP-161 empty-account cleanup) rather than
/// kept as an empty account.
fn insert_code_change(
    state: &mut PendingState,
    address: Address,
    original: Option<AccountInfo>,
    code: Option<Bytecode>,
) {
    let base = original.clone().unwrap_or_else(AccountInfo::empty);
    let mut current = AccountInfo::new(
        base.balance,
        base.nonce,
        AccountInfo::empty().code_hash,
        None,
    );
    if let Some(code) = code {
        current = current.with_code(code);
    }
    let current = (!current.is_empty()).then_some(current);
    state.insert_account(address, original, current);
}
