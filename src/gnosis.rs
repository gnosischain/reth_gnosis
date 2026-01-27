use crate::errors::GnosisBlockExecutionError;
use crate::spec::gnosis_spec::{BalancerHardforkConfig, GnosisHardForks};
use alloy_consensus::constants::KECCAK_EMPTY;
use alloy_eips::eip4895::{Withdrawal, Withdrawals};
use alloy_primitives::U256;
use alloy_primitives::{map::HashMap, Address, Bytes};
use alloy_sol_macro::sol;
use alloy_sol_types::SolCall;
use reth_evm::{
    block::{StateChangePostBlockSource, StateChangeSource, SystemCaller},
    eth::spec::EthExecutorSpec,
    execute::{BlockExecutionError, InternalBlockExecutionError},
    Evm,
};
use revm::context::Block;
use revm::Database;
use revm::{
    context::result::{ExecutionResult, Output, ResultAndState},
    DatabaseCommit,
};
use revm_state::{Account, AccountInfo};
use std::fmt::Display;

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

/// Applies the post-block call to the withdrawal / deposit contract, using the given block.
/// Ref: <https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md>
#[inline]
fn apply_withdrawals_contract_call<SPEC>(
    withdrawal_contract_address: Address,
    withdrawals: &[Withdrawal],
    evm: &mut impl Evm<DB: DatabaseCommit, Error: Display>,
    system_caller: &mut SystemCaller<SPEC>,
) -> Result<Bytes, BlockExecutionError>
where
    SPEC: EthExecutorSpec + GnosisHardForks,
{
    // TODO: Only do the call post-merge
    // TODO: Should this call be made for the genesis block?

    let ResultAndState { result, mut state } = match evm.transact_system_call(
        alloy_eips::eip4788::SYSTEM_ADDRESS,
        withdrawal_contract_address,
        executeSystemWithdrawalsCall {
            maxFailedWithdrawalsToProcess: U256::from(4),
            _amounts: withdrawals.iter().map(|w| w.amount).collect::<Vec<_>>(),
            _addresses: withdrawals.iter().map(|w| w.address).collect::<Vec<_>>(),
        }
        .abi_encode()
        .into(),
    ) {
        Ok(res) => res,
        Err(e) => {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!("withdrawal contract system call revert: {e}").into(),
                ),
            ))
        }
    };

    // TODO: Should check the execution is successful? Is an Ok from transact() enough?

    // Clean-up post system tx context
    state.remove(&alloy_eips::eip4788::SYSTEM_ADDRESS);
    state.remove(&evm.block().beneficiary());

    system_caller.invoke_hook_with(|hook| {
        hook.on_state(
            StateChangeSource::PostBlock(StateChangePostBlockSource::WithdrawalRequestsContract),
            &state,
        );
    });

    evm.db_mut().commit(state);

    match result {
        ExecutionResult::Success { output, .. } => Ok(output.into_data()),
        ExecutionResult::Revert { output, .. } => Err(BlockExecutionError::Internal(
            InternalBlockExecutionError::Other(format!("execution reverted: {output}").into()),
        )),
        ExecutionResult::Halt { reason, .. } => Err(BlockExecutionError::Internal(
            InternalBlockExecutionError::Other(format!("execution halted: {reason:?}").into()),
        )),
    }
}

/// Applies the post-block call to the block rewards POSDAO contract, using the given block,
/// Ref: <https://github.com/gnosischain/specs/blob/master/execution/posdao-post-merge.md>
#[inline]
fn apply_block_rewards_contract_call<SPEC>(
    block_rewards_contract: Address,
    coinbase: Address,
    evm: &mut impl Evm<DB: DatabaseCommit, Error: Display>,
    system_caller: &mut SystemCaller<SPEC>,
) -> Result<HashMap<Address, u128>, BlockExecutionError>
where
    SPEC: EthExecutorSpec + GnosisHardForks,
{
    let ResultAndState { result, state } = match evm.transact_system_call(
        alloy_eips::eip4788::SYSTEM_ADDRESS,
        block_rewards_contract,
        rewardCall {
            benefactors: vec![coinbase],
            // Type 0 = RewardAuthor
            kind: vec![0],
        }
        .abi_encode()
        .into(),
    ) {
        Ok(res) => res,
        Err(e) => {
            return Err(BlockExecutionError::from(
                GnosisBlockExecutionError::CustomErrorMessage {
                    message: format!("block rewards contract system call error: {e}"),
                },
            ));
        }
    };

    if state.get(&block_rewards_contract).unwrap().info.code_hash == KECCAK_EMPTY {
        return Ok(HashMap::default());
    }

    let output_bytes = match result {
        ExecutionResult::Success { output, .. } => match output {
            Output::Call(output_bytes) |
            // Should never happen, we craft a transaction without constructor code
            Output::Create(output_bytes, _) => output_bytes,
        },
        ExecutionResult::Revert { output, .. } => {
            return Err(BlockExecutionError::from(
                GnosisBlockExecutionError::CustomErrorMessage {
                    message: format!("block rewards contract system call revert {output}"),
                },
            ));
        }
        ExecutionResult::Halt { reason, .. } => {
            return Err(BlockExecutionError::from(
                GnosisBlockExecutionError::CustomErrorMessage {
                    message: format!("block rewards contract system call halt {reason:?}"),
                },
            ));
        }
    };

    let result = rewardCall::abi_decode_returns(output_bytes.as_ref()).map_err(|e| {
        BlockExecutionError::from(GnosisBlockExecutionError::CustomErrorMessage {
            message: format!(
                "error parsing block rewards contract system call return {:?}: {}",
                hex::encode(output_bytes),
                e
            ),
        })
    })?;

    system_caller.invoke_hook_with(|hook| {
        hook.on_state(
            StateChangeSource::PostBlock(StateChangePostBlockSource::WithdrawalRequestsContract),
            &state,
        );
    });

    evm.db_mut().commit(state);

    // TODO: How to get function return call from evm.transact()?
    let mut balance_increments = HashMap::default();
    for (address, amount) in result
        .receiversNative
        .iter()
        .zip(result.rewardsNative.iter())
    {
        // TODO: .to panics if the return value is too large
        *balance_increments.entry(*address).or_default() += amount.to::<u128>();
    }

    Ok(balance_increments)
}

// TODO: this can be simplified by using the existing apply_post_execution_changes
// which does all of the same things
//
// [Gnosis/fork:DIFF]: Upstream code in EthBlockExecutor computes balance changes for:
// - Pre-merge omer and block rewards
// - Beacon withdrawal mints
// - DAO hardfork drain balances
//
// Gnosis post-block system calls:
// - Do NOT credit withdrawals as native token mint
// - Call into deposit contract with withdrawal data
// - Call block rewards contract for bridged xDAI mint
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_post_block_system_calls<SPEC>(
    chain_spec: &SPEC,
    block_rewards_contract: Address,
    withdrawal_contract: Address,
    block_timestamp: u64,
    withdrawals: Option<&Withdrawals>,
    coinbase: Address,
    evm: &mut impl Evm<DB: Database + DatabaseCommit>,
    system_caller: &mut SystemCaller<SPEC>,
) -> Result<(HashMap<alloy_primitives::Address, u128>, Bytes), BlockExecutionError>
where
    SPEC: EthExecutorSpec + GnosisHardForks,
{
    let mut withdrawal_requests = Bytes::new();

    if chain_spec.is_shanghai_active_at_timestamp(block_timestamp) {
        let withdrawals = withdrawals.ok_or(GnosisBlockExecutionError::CustomErrorMessage {
            message: "block has no withdrawals field".to_owned(),
        })?;
        withdrawal_requests =
            apply_withdrawals_contract_call(withdrawal_contract, withdrawals, evm, system_caller)?;
    }

    let balance_increments =
        apply_block_rewards_contract_call(block_rewards_contract, coinbase, evm, system_caller)?;

    Ok((balance_increments, withdrawal_requests))
}

pub fn rewrite_bytecodes(
    evm: &mut impl Evm<DB: Database + DatabaseCommit>,
    balancer_hardfork_config: &BalancerHardforkConfig,
) {
    let mut state: HashMap<Address, Account> = Default::default();
    for (addr, code, expected_code_hash) in &balancer_hardfork_config.config {
        let original_account_info = evm
            .db_mut()
            .basic(*addr)
            .unwrap_or_default()
            .unwrap_or_default();
        if &original_account_info.code_hash == expected_code_hash {
            // No need to rewrite
            tracing::trace!(">>> Skipping rewrite for address: {}", addr);
            tracing::trace!("    Code hash matches expected: {}", expected_code_hash);
            continue;
        }
        let modified_account_info = AccountInfo {
            code_hash: if let Some(ref code) = code {
                code.hash_slow()
            } else {
                KECCAK_EMPTY
            },
            code: code.clone(),
            ..original_account_info
        };
        let account = Account {
            info: modified_account_info,
            storage: HashMap::default(),
            status: revm_state::AccountStatus::Touched,
            transaction_id: 0,
            original_info: Box::new(original_account_info.clone()),
        };
        tracing::info!(
            "Rewriting Bytecode >>> Addr: {}; From: {}; To: {}",
            addr,
            original_account_info.code_hash,
            account.info.code_hash
        );
        state.insert(*addr, account);
    }

    // commit the modified accounts to the EVM database
    evm.db_mut().commit(state);
}
