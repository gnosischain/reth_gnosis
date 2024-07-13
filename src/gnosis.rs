use std::collections::HashMap;

use alloy_sol_macro::sol;
use alloy_sol_types::SolCall;
use reth::{
    primitives::{address, Address, Bytes, Withdrawal, U256},
    revm::{
        interpreter::Host,
        primitives::{Env, ExecutionResult, Output, ResultAndState, TransactTo, TxEnv},
        Database, DatabaseCommit, Evm,
    },
};
use reth_chainspec::ChainSpec;
use reth_evm::execute::BlockExecutionError;

pub const SYSTEM_ADDRESS: Address = address!("fffffffffffffffffffffffffffffffffffffffe");

// TODO: customize from genesis or somewhere
pub const BLOCK_REWARDS_CONTRACT: Address = address!("481c034c6d9441db23ea48de68bcae812c5d39ba");

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
pub fn apply_withdrawals_contract_call<EXT, DB: Database + DatabaseCommit>(
    chain_spec: &ChainSpec,
    _block_timestamp: u64,
    withdrawals: &[Withdrawal],
    evm: &mut Evm<'_, EXT, DB>,
) -> Result<(), BlockExecutionError>
where
    DB::Error: std::fmt::Display,
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
    fill_tx_env_with_system_contract_call(
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
pub fn apply_block_rewards_contract_call<EXT, DB: Database + DatabaseCommit>(
    _chain_spec: &ChainSpec,
    _block_timestamp: u64,
    coinbase: Address,
    evm: &mut Evm<'_, EXT, DB>,
) -> Result<HashMap<Address, u128>, BlockExecutionError>
where
    DB::Error: std::fmt::Display,
{
    // get previous env
    let previous_env = Box::new(evm.context.env().clone());

    // modify env for pre block call
    fill_tx_env_with_system_contract_call(
        &mut evm.context.evm.env,
        SYSTEM_ADDRESS,
        BLOCK_REWARDS_CONTRACT,
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
                format!("withdrawal contract system call error: {}", e).into(),
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
                format!("withdrawal contract system call revert {}", output).into(),
            ));
        }
        ExecutionResult::Halt { reason, .. } => {
            return Err(BlockExecutionError::Other(
                format!("withdrawal contract system call halt {:?}", reason).into(),
            ));
        }
    };

    let result = rewardCall::abi_decode_returns(output_bytes.as_ref(), true).map_err(|e| {
        BlockExecutionError::Other(
            format!("error parsing withdrawal contract system call return {}", e).into(),
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

/// Fill transaction environment with the system caller and the system contract address and message
/// data.
///
/// This is a system operation and therefore:
///  * the call must execute to completion
///  * the call does not count against the block‚Äôs gas limit
///  * the call does not follow the EIP-1559 burn semantics - no value should be transferred as part
///    of the call
///  * if no code exists at the provided address, the call will fail silently
fn fill_tx_env_with_system_contract_call(
    env: &mut Env,
    caller: Address,
    contract: Address,
    data: Bytes,
) {
    env.tx = TxEnv {
        caller,
        transact_to: TransactTo::Call(contract),
        // Explicitly set nonce to None so revm does not do any nonce checks
        nonce: None,
        gas_limit: 30_000_000,
        value: U256::ZERO,
        data,
        // Setting the gas price to zero enforces that no value is transferred as part of the call,
        // and that the call will not count against the block's gas limit
        gas_price: U256::ZERO,
        // The chain ID check is not relevant here and is disabled if set to None
        chain_id: None,
        // Setting the gas priority fee to None ensures the effective gas price is derived from the
        // `gas_price` field, which we need to be zero
        gas_priority_fee: None,
        access_list: Vec::new(),
        // blob fields can be None for this tx
        blob_hashes: Vec::new(),
        max_fee_per_blob_gas: None,
        authorization_list: None,
    };

    // ensure the block gas limit is >= the tx
    env.block.gas_limit = U256::from(env.tx.gas_limit);

    // disable the base fee check for this call by setting the base fee to zero
    env.block.basefee = U256::ZERO;
}
