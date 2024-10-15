use std::collections::HashMap;

use crate::errors::GnosisBlockExecutionError;
use alloy_primitives::{address, Address, U256};
use alloy_sol_macro::sol;
use alloy_sol_types::SolCall;
use reth::{
    primitives::Withdrawal,
    revm::{
        interpreter::Host,
        primitives::{ExecutionResult, Output, ResultAndState},
        Database, DatabaseCommit, Evm,
    },
};
use reth_chainspec::ChainSpec;
use reth_errors::BlockValidationError;
use reth_evm::{execute::BlockExecutionError, ConfigureEvm};
use revm_primitives::{Account, AccountInfo, AccountStatus};

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
/// [`ChainSpec`], EVM.
///
/// Ref: <https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md>
#[inline]
pub fn apply_withdrawals_contract_call<EvmConfig, EXT, DB>(
    evm_config: &EvmConfig,
    chain_spec: &ChainSpec,
    withdrawals: &[Withdrawal],
    evm: &mut Evm<'_, EXT, DB>,
) -> Result<(), BlockExecutionError>
where
    DB: Database + DatabaseCommit,
    DB::Error: std::fmt::Display,
    EvmConfig: ConfigureEvm,
{
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

    // get previous env
    let previous_env = Box::new(evm.context.env().clone());

    // modify env for pre block call
    evm_config.fill_tx_env_system_contract_call(
        &mut evm.context.evm.env,
        SYSTEM_ADDRESS,
        withdrawal_contract_address,
        executeSystemWithdrawalsCall {
            maxFailedWithdrawalsToProcess: U256::from(4),
            _amounts: withdrawals.iter().map(|w| w.amount).collect::<Vec<_>>(),
            _addresses: withdrawals.iter().map(|w| w.address).collect::<Vec<_>>(),
        }
        .abi_encode()
        .into(),
    );

    let mut state = match evm.transact() {
        Ok(res) => res.state,
        Err(e) => {
            evm.context.evm.env = previous_env;
            return Err(BlockExecutionError::Validation(
                BlockValidationError::WithdrawalRequestsContractCall {
                    message: format!("withdrawal contract system call revert: {}", e),
                },
            ));
        }
    };

    // TODO: Should check the execution is successful? Is an Ok from transact() enough?

    // Clean-up post system tx context
    state.remove(&SYSTEM_ADDRESS);
    state.remove(&evm.block().coinbase);
    evm.context.evm.db.commit(state);
    // re-set the previous env
    evm.context.evm.env = previous_env;

    Ok(())
}

/// Applies the post-block call to the block rewards POSDAO contract, using the given block,
/// [`ChainSpec`], EVM.
///
/// Ref: <https://github.com/gnosischain/specs/blob/master/execution/posdao-post-merge.md>
#[inline]
pub fn apply_block_rewards_contract_call<EvmConfig, EXT, DB>(
    evm_config: &EvmConfig,
    block_rewards_contract: Address,
    _block_timestamp: u64,
    coinbase: Address,
    evm: &mut Evm<'_, EXT, DB>,
) -> Result<HashMap<Address, u128>, BlockExecutionError>
where
    DB: Database + DatabaseCommit,
    DB::Error: std::fmt::Display,
    EvmConfig: ConfigureEvm,
{
    // get previous env
    let previous_env = Box::new(evm.context.env().clone());

    // modify env for pre block call
    evm_config.fill_tx_env_system_contract_call(
        &mut evm.context.evm.env,
        SYSTEM_ADDRESS,
        block_rewards_contract,
        rewardCall {
            benefactors: vec![coinbase],
            // Type 0 = RewardAuthor
            kind: vec![0],
        }
        .abi_encode()
        .into(),
    );

    let ResultAndState { result, mut state } = match evm.transact() {
        Ok(res) => res,
        Err(e) => {
            evm.context.evm.env = previous_env;
            return Err(BlockExecutionError::from(
                GnosisBlockExecutionError::CustomErrorMessage {
                    message: format!("block rewards contract system call error: {}", e),
                },
            ));
        }
    };

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

    // figure out if we should create the system account
    let mut should_create = false;
    if let Some(system_account) = state.get(&SYSTEM_ADDRESS) {
        if system_account.status == (AccountStatus::Touched | AccountStatus::LoadedAsNotExisting) {
            should_create = true;
        }
    } else {
        should_create = true;
    }

    // system account call is only in rewards function because it will be called in every block
    // Clean-up post system tx context
    if should_create {
        // Populate system account on first block
        let account = Account {
            info: AccountInfo::default(),
            storage: Default::default(),
            status: AccountStatus::Touched | AccountStatus::Created,
        };
        state.insert(SYSTEM_ADDRESS, account);
    } else {
        // Conditionally clear the system address account to prevent being removed
        state.remove(&SYSTEM_ADDRESS);
    }

    state.remove(&evm.block().coinbase);
    evm.context.evm.db.commit(state);
    // re-set the previous env
    evm.context.evm.env = previous_env;

    // TODO: How to get function return call from evm.transact()?
    let mut balance_increments = HashMap::new();
    for (address, amount) in result
        .receiversNative
        .iter()
        .zip(result.rewardsNative.iter())
    {
        // TODO: .to panics if the return value is too large
        balance_increments.insert(*address, amount.to::<u128>());
    }

    Ok(balance_increments)
}
