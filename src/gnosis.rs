use std::collections::HashMap;

use crate::{errors::GnosisBlockExecutionError, spec::GnosisChainSpec};
use alloy_consensus::constants::KECCAK_EMPTY;
use alloy_eips::eip4895::{Withdrawal, Withdrawals};
use alloy_primitives::{address, Address, Bytes};
use alloy_sol_macro::sol;
use alloy_sol_types::SolCall;
use reth::revm::primitives::{ExecutionResult, Output};
use reth_chainspec::EthereumHardforks;
use reth_errors::BlockValidationError;
use reth_evm::{
    execute::{BlockExecutionError, InternalBlockExecutionError},
    Evm,
};
use revm_primitives::{db::DatabaseCommit, ResultAndState, U256};
use revm_primitives::{Account, AccountInfo, AccountStatus};
use std::fmt::Display;

pub const SYSTEM_ADDRESS: Address = address!("fffffffffffffffffffffffffffffffffffffffe");

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

/// Applies the post-block call to the withdrawal / deposit contract, using the given block,
/// [`GnosisChainSpec`], EVM.
///
/// Ref: <https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md>
#[inline]
fn apply_withdrawals_contract_call(
    chain_spec: &GnosisChainSpec,
    withdrawals: &[Withdrawal],
    evm: &mut impl Evm<DB: DatabaseCommit, Error: Display>,
) -> Result<Bytes, BlockExecutionError> {
    // TODO: how is the deposit contract address passed to here?
    let withdrawal_contract_address = chain_spec
        .deposit_contract
        .as_ref()
        .ok_or(GnosisBlockExecutionError::CustomErrorMessage {
            message: "deposit_contract not set".to_owned(),
        })?
        .address;

    // TODO: Only do the call post-merge
    // TODO: Should this call be made for the genesis block?

    let ResultAndState { result, mut state } = match evm.transact_system_call(
        SYSTEM_ADDRESS,
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
                    format!("withdrawal contract system call revert: {}", e).into(),
                ),
            ))
        }
    };

    // TODO: Should check the execution is successful? Is an Ok from transact() enough?

    // Clean-up post system tx context
    state.remove(&SYSTEM_ADDRESS);
    state.remove(&evm.block().coinbase);

    evm.db_mut().commit(state);

    match result {
        ExecutionResult::Success { output, .. } => Ok(output.into_data()),
        ExecutionResult::Revert { output, .. } => {
            Err(BlockValidationError::WithdrawalRequestsContractCall {
                message: format!("execution reverted: {output}"),
            }
            .into())
        }
        ExecutionResult::Halt { reason, .. } => {
            Err(BlockValidationError::WithdrawalRequestsContractCall {
                message: format!("execution halted: {reason:?}"),
            }
            .into())
        }
    }
}

/// Applies the post-block call to the block rewards POSDAO contract, using the given block,
/// [`GnosisChainSpec`], EVM.
///
/// Ref: <https://github.com/gnosischain/specs/blob/master/execution/posdao-post-merge.md>
#[inline]
fn apply_block_rewards_contract_call(
    // evm_config: &EvmConfig,
    block_rewards_contract: Address,
    _block_timestamp: u64,
    coinbase: Address,
    evm: &mut impl Evm<DB: DatabaseCommit, Error: Display>,
) -> Result<HashMap<Address, u128>, BlockExecutionError> {
    let ResultAndState { result, mut state } = match evm.transact_system_call(
        SYSTEM_ADDRESS,
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
                    message: format!("block rewards contract system call error: {}", e),
                },
            ));
        }
    };

    if state.get(&block_rewards_contract).unwrap().info.code_hash == KECCAK_EMPTY {
        return Ok(HashMap::new());
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
                    message: format!("block rewards contract system call revert {}", output),
                },
            ));
        }
        ExecutionResult::Halt { reason, .. } => {
            return Err(BlockExecutionError::from(
                GnosisBlockExecutionError::CustomErrorMessage {
                    message: format!("block rewards contract system call halt {:?}", reason),
                },
            ));
        }
    };

    let result = rewardCall::abi_decode_returns(output_bytes.as_ref(), true).map_err(|e| {
        BlockExecutionError::from(GnosisBlockExecutionError::CustomErrorMessage {
            message: format!(
                "error parsing block rewards contract system call return {:?}: {}",
                hex::encode(output_bytes),
                e
            ),
        })
    })?;

    // in gnosis aura, system account needs to be included in the state and not removed (despite EIP-158/161, even if empty)
    // here we have a generalized check if system account is in state, or needs to be created

    // keeping this generalized, instead of only in block 1
    // (AccountStatus::Touched | AccountStatus::LoadedAsNotExisting) means the account is not in the state
    let should_create = state.get(&SYSTEM_ADDRESS).is_none_or(|system_account| {
        // true if account not in state (either None, or Touched | LoadedAsNotExisting)
        system_account.status == (AccountStatus::Touched | AccountStatus::LoadedAsNotExisting)
    });

    // this check needs to be there in every call, so instead of making it into a function which is called from post_execution, we can just include it in the rewards function
    if should_create {
        let account = Account {
            info: AccountInfo::default(),
            storage: Default::default(),
            // we force the account to be created by changing the status
            status: AccountStatus::Touched | AccountStatus::Created,
        };
        state.insert(SYSTEM_ADDRESS, account);
    } else {
        // clear the system address account from state transitions, else EIP-158/161 (impl in revm) removes it from state
        state.remove(&SYSTEM_ADDRESS);
    }

    state.remove(&evm.block().coinbase);
    evm.db_mut().commit(state);

    // TODO: How to get function return call from evm.transact()?
    let mut balance_increments = HashMap::new();
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

// Post-pectra, the blob fee is collected by the fee collector contract instead of getting burned
fn add_blob_fee_collection_to_balance_increments(
    balance_increments: &mut HashMap<Address, u128>,
    chain_spec: &GnosisChainSpec,
    blob_fee: u128,
) {
    let fee_collector_contract = chain_spec
        .genesis()
        .config
        .extra_fields
        .get("eip1559collector")
        .expect("no eip1559collector field");
    let fee_collector_contract: Address = serde_json::from_value(fee_collector_contract.clone())
        .expect("failed to parse eip1559collector field");
    *balance_increments
        .entry(fee_collector_contract)
        .or_default() += blob_fee;
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
pub(crate) fn apply_post_block_system_calls(
    chain_spec: &GnosisChainSpec,
    // evm_config: &EvmConfig,
    block_rewards_contract: Address,
    block_timestamp: u64,
    withdrawals: Option<&Withdrawals>,
    coinbase: Address,
    evm: &mut impl Evm<DB: DatabaseCommit, Error: Display>,
    blob_fee: u128,
) -> Result<(HashMap<alloy_primitives::Address, u128>, Bytes), BlockExecutionError> {
    let mut withdrawal_requests = Bytes::new();

    if chain_spec.is_shanghai_active_at_timestamp(block_timestamp) {
        let withdrawals = withdrawals.ok_or(GnosisBlockExecutionError::CustomErrorMessage {
            message: "block has no withdrawals field".to_owned(),
        })?;
        withdrawal_requests = apply_withdrawals_contract_call(chain_spec, withdrawals, evm)?;
    }

    let mut balance_increments =
        apply_block_rewards_contract_call(block_rewards_contract, block_timestamp, coinbase, evm)?;

    if chain_spec.is_prague_active_at_timestamp(block_timestamp) {
        add_blob_fee_collection_to_balance_increments(
            &mut balance_increments,
            chain_spec,
            blob_fee,
        );
    }

    Ok((balance_increments, withdrawal_requests))
}
