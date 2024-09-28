use std::collections::HashMap;

use alloy_sol_macro::sol;
use alloy_sol_types::SolCall;
use reth::{
    primitives::{address, Address, Withdrawal, U256},
    revm::{
        interpreter::Host,
        primitives::{ExecutionResult, Output, ResultAndState},
        Database, DatabaseCommit, Evm,
    },
};
use reth_chainspec::ChainSpec;
use reth_evm::{execute::BlockExecutionError, ConfigureEvm};

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
        .ok_or(BlockExecutionError::Other(
            "deposit_contract not set".to_owned().into(),
        ))?
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
            return Err(BlockExecutionError::Other(
                format!("withdrawal contract system call revert: {}", e).into(),
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
            return Err(BlockExecutionError::Other(
                format!("block rewards contract system call error: {}", e).into(),
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
            return Err(BlockExecutionError::Other(
                format!("block rewards contract system call revert {}", output).into(),
            ));
        }
        ExecutionResult::Halt { reason, .. } => {
            return Err(BlockExecutionError::Other(
                format!("block rewards contract system call halt {:?}", reason).into(),
            ));
        }
    };

    let result = rewardCall::abi_decode_returns(output_bytes.as_ref(), true).map_err(|e| {
        BlockExecutionError::Other(
            format!(
                "error parsing block rewards contract system call return {:?}: {}",
                hex::encode(output_bytes),
                e
            )
            .into(),
        )
    })?;

    // Clean-up post system tx context
    state.remove(&SYSTEM_ADDRESS);
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
