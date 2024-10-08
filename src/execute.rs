use crate::errors::GnosisBlockExecutionError;
use crate::ethereum::{EthEvmExecutor, EthExecuteOutput};
use crate::gnosis::{apply_block_rewards_contract_call, apply_withdrawals_contract_call};
use alloy_primitives::{Address, BlockNumber, U256};
use eyre::eyre;
use reth::primitives::Withdrawals;
use reth::providers::ExecutionOutcome;
use reth::{
    api::ConfigureEvm,
    providers::ProviderError,
    revm::{
        batch::BlockBatchRecord,
        db::states::bundle_state::BundleRetention,
        primitives::{BlockEnv, CfgEnvWithHandlerCfg, EnvWithHandlerCfg},
        Database, State,
    },
};
use reth_chainspec::{ChainSpec, EthereumHardforks};
use reth_ethereum_consensus::validate_block_post_execution;
use reth_evm::execute::{
    BatchExecutor, BlockExecutionError, BlockExecutionInput, BlockExecutionOutput,
    BlockExecutorProvider, BlockValidationError, Executor,
};
use reth_evm::system_calls::{NoopHook, OnStateHook};
use reth_primitives::{BlockWithSenders, Header, Receipt, Receipts};
use reth_prune_types::PruneModes;
use revm::Evm;
use std::fmt::Display;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct GnosisExecutorProvider<EvmConfig: Clone> {
    chain_spec: Arc<ChainSpec>,
    evm_config: EvmConfig,
    /// AuRa BlockRewards contract address for its system call
    block_rewards_contract: Address,
}

impl<EvmConfig: Clone> GnosisExecutorProvider<EvmConfig> {
    /// Creates a new executor provider.
    pub fn new(chain_spec: Arc<ChainSpec>, evm_config: EvmConfig) -> eyre::Result<Self> {
        let block_rewards_contract = chain_spec
            .genesis()
            .config
            .extra_fields
            .get("blockRewardsContract")
            .ok_or(eyre!("blockRewardsContract not defined"))?;
        let block_rewards_contract: Address =
            serde_json::from_value(block_rewards_contract.clone())
                .map_err(|e| BlockExecutionError::other(Box::new(e)))?;

        Ok(Self {
            chain_spec,
            evm_config,
            block_rewards_contract,
        })
    }
}

// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthExecutorProvider
impl<EvmConfig: Clone> GnosisExecutorProvider<EvmConfig>
where
    EvmConfig: ConfigureEvm,
{
    fn gnosis_executor<DB>(&self, db: DB) -> GnosisBlockExecutor<EvmConfig, DB>
    where
        DB: Database<Error: Into<ProviderError> + Display>,
    {
        GnosisBlockExecutor::new(
            self.block_rewards_contract,
            self.chain_spec.clone(),
            self.evm_config.clone(),
            State::builder()
                .with_database(db)
                .with_bundle_update()
                .without_state_clear()
                .build(),
        )
    }
}

// Trait required by ExecutorBuilder
// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthExecutorProvider
impl<EvmConfig: Clone> BlockExecutorProvider for GnosisExecutorProvider<EvmConfig>
where
    EvmConfig: ConfigureEvm<Header = Header>,
{
    type Executor<DB: Database<Error: Into<ProviderError> + Display>> =
        GnosisBlockExecutor<EvmConfig, DB>;
    type BatchExecutor<DB: Database<Error: Into<ProviderError> + Display>> =
        GnosisBatchExecutor<EvmConfig, DB>;

    fn executor<DB>(&self, db: DB) -> Self::Executor<DB>
    where
        DB: Database<Error: Into<ProviderError> + Display>,
    {
        self.gnosis_executor(db)
    }

    fn batch_executor<DB>(&self, db: DB) -> Self::BatchExecutor<DB>
    where
        DB: Database<Error: Into<ProviderError> + Display>,
    {
        let executor = self.gnosis_executor(db);
        GnosisBatchExecutor {
            executor,
            batch_record: BlockBatchRecord::default(),
            // stats: BlockExecutorStats::default(),
        }
    }
}

// Struct required for BlockExecutorProvider trait
// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBlockExecutor
#[derive(Debug)]
pub struct GnosisBlockExecutor<EvmConfig, DB> {
    /// Chain specific evm config that's used to execute a block.
    executor: EthEvmExecutor<EvmConfig>,
    /// The state to use for execution
    state: State<DB>,
    /// AuRa BlockRewards contract address for its system call
    block_rewards_contract: Address,
}

// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBlockExecutor
impl<EvmConfig, DB> GnosisBlockExecutor<EvmConfig, DB> {
    /// Creates a new Ethereum block executor.
    pub fn new(
        block_rewards_contract: Address,
        chain_spec: Arc<ChainSpec>,
        evm_config: EvmConfig,
        state: State<DB>,
    ) -> Self {
        Self {
            executor: EthEvmExecutor {
                chain_spec,
                evm_config,
            },
            state,
            block_rewards_contract,
        }
    }

    #[inline]
    fn chain_spec(&self) -> &ChainSpec {
        &self.executor.chain_spec
    }

    fn chain_spec_clone(&self) -> Arc<ChainSpec> {
        self.executor.chain_spec.clone()
    }

    /// Returns mutable reference to the state that wraps the underlying database.
    #[allow(unused)]
    fn state_mut(&mut self) -> &mut State<DB> {
        &mut self.state
    }
}

// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBlockExecutor
impl<EvmConfig, DB> GnosisBlockExecutor<EvmConfig, DB>
where
    EvmConfig: ConfigureEvm<Header = Header>,
    DB: Database<Error: Into<ProviderError> + Display>,
{
    /// Configures a new evm configuration and block environment for the given block.
    ///
    /// # Caution
    ///
    /// This does not initialize the tx environment.
    fn evm_env_for_block(&self, header: &Header, total_difficulty: U256) -> EnvWithHandlerCfg {
        let mut cfg = CfgEnvWithHandlerCfg::new(Default::default(), Default::default());
        let mut block_env = BlockEnv::default();
        self.executor.evm_config.fill_cfg_and_block_env(
            &mut cfg,
            &mut block_env,
            // self.chain_spec(),
            header,
            total_difficulty,
        );

        EnvWithHandlerCfg::new_with_cfg_env(cfg, block_env, Default::default())
    }

    fn execute_without_verification(
        &mut self,
        block: &BlockWithSenders,
        total_difficulty: U256,
    ) -> Result<EthExecuteOutput, BlockExecutionError> {
        // 1. prepare state on new block
        self.on_new_block(&block.header);

        // 2. configure the evm and execute
        let env = self.evm_env_for_block(&block.header, total_difficulty);
        let output = {
            let evm = self.executor.evm_config.evm_with_env(&mut self.state, env);
            self.executor
                .execute_state_transitions(block, evm, None::<NoopHook>)
        }?;

        // 3. apply post execution changes
        self.post_execution(block, total_difficulty)?;

        Ok(output)
    }

    /// Execute a single block and apply the state changes to the internal state.
    ///
    /// Returns the receipts of the transactions in the block, the total gas used and the list of
    /// EIP-7685 [requests](Request).
    ///
    /// Returns an error if execution fails.
    fn execute_without_verification_with_state_hook<F>(
        &mut self,
        block: &BlockWithSenders,
        total_difficulty: U256,
        state_hook: Option<F>,
    ) -> Result<EthExecuteOutput, BlockExecutionError>
    where
        F: OnStateHook,
    {
        // 1. prepare state on new block
        self.on_new_block(&block.header);

        // 2. configure the evm and execute
        let env = self.evm_env_for_block(&block.header, total_difficulty);
        let output = {
            let evm = self.executor.evm_config.evm_with_env(&mut self.state, env);
            self.executor.execute_state_transitions(block, evm, state_hook)
        }?;

        // 3. apply post execution changes
        self.post_execution(block, total_difficulty)?;

        Ok(output)
    }

    fn on_new_block(&mut self, header: &Header) {
        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag = self
            .chain_spec()
            .is_spurious_dragon_active_at_block(header.number);
        self.state.set_state_clear_flag(state_clear_flag);
    }

    /// Apply post execution state changes that do not require an evm such as: block
    /// rewards, withdrawals, and irregular DAO hardfork state change
    // [Gnosis/fork:DIFF]
    pub fn post_execution(
        &mut self,
        block: &BlockWithSenders,
        total_difficulty: U256,
    ) -> Result<(), BlockExecutionError> {
        // [Gnosis/fork:DIFF]: Upstream code in EthBlockExecutor computes balance changes for:
        // - Pre-merge omer and block rewards
        // - Beacon withdrawal mints
        // - DAO hardfork drain balances
        //
        // For gnosis instead:
        // - Do NOT credit withdrawals as native token mint
        // - Call into deposit contract with withdrawal data
        // - Call block rewards contract for bridged xDAI mint

        let chain_spec = self.chain_spec_clone();
        let mut cfg = CfgEnvWithHandlerCfg::new(Default::default(), Default::default());
        let mut block_env = BlockEnv::default();
        self.executor.evm_config.fill_cfg_and_block_env(
            &mut cfg,
            &mut block_env,
            // self.chain_spec(),
            &block.header,
            total_difficulty,
        );

        gnosis_post_block_system_calls::<EvmConfig, DB>(
            &chain_spec,
            &self.executor.evm_config,
            &mut self.state,
            &cfg,
            &block_env,
            self.block_rewards_contract,
            block.timestamp,
            block.body.withdrawals.as_ref(),
            block.beneficiary,
        )?;

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn gnosis_post_block_system_calls<EvmConfig, DB>(
    chain_spec: &ChainSpec,
    evm_config: &EvmConfig,
    db: &mut State<DB>,
    initialized_cfg: &CfgEnvWithHandlerCfg,
    initialized_block_env: &BlockEnv,
    block_rewards_contract: Address,
    block_timestamp: u64,
    withdrawals: Option<&Withdrawals>,
    coinbase: Address,
) -> Result<(), BlockExecutionError>
where
    EvmConfig: ConfigureEvm,
    DB: Database<Error: Into<ProviderError> + Display>,
{
    let mut evm = Evm::builder()
        .with_db(db)
        .with_env_with_handler_cfg(EnvWithHandlerCfg::new_with_cfg_env(
            initialized_cfg.clone(),
            initialized_block_env.clone(),
            Default::default(),
        ))
        .build();

    // [Gnosis/fork:DIFF]: Upstream code in EthBlockExecutor computes balance changes for:
    // - Pre-merge omer and block rewards
    // - Beacon withdrawal mints
    // - DAO hardfork drain balances
    //
    // For gnosis instead:
    // - Do NOT credit withdrawals as native token mint
    // - Call into deposit contract with withdrawal data
    // - Call block rewards contract for bridged xDAI mint

    if chain_spec.is_shanghai_active_at_timestamp(block_timestamp) {
        // let withdrawals = withdrawals.ok_or(BlockExecutionError::other::<BlockExecutionError::Internal>::(
        //     "block has no withdrawals field".to_owned().into(),
        // ))?;
        let withdrawals = withdrawals.ok_or(
            // BlockValidationError::WithdrawalRequestsContractCall {
            //     message: "block has no withdrawals field".to_owned().into(),
            // }
            GnosisBlockExecutionError::CustomErrorMessage { message: "block has no withdrawals field".to_owned() }
        )?;
        apply_withdrawals_contract_call(evm_config, chain_spec, withdrawals, &mut evm)?;
    }

    let balance_increments = apply_block_rewards_contract_call(
        evm_config,
        block_rewards_contract,
        block_timestamp,
        coinbase,
        &mut evm,
    )?;

    // increment balances
    evm.context
        .evm
        .db
        .increment_balances(balance_increments)
        .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;

    Ok(())
}

// Trait required by BlockExecutorProvider associated type Executor
impl<EvmConfig, DB> Executor<DB> for GnosisBlockExecutor<EvmConfig, DB>
where
    EvmConfig: ConfigureEvm<Header = Header>,
    DB: Database<Error: Into<ProviderError> + Display>,
{
    type Input<'a> = BlockExecutionInput<'a, BlockWithSenders>;
    type Output = BlockExecutionOutput<Receipt>;
    type Error = BlockExecutionError;

    // [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBlockExecutor
    fn execute(mut self, input: Self::Input<'_>) -> Result<Self::Output, Self::Error> {
        // No diff with EthBlockExecutor
        let BlockExecutionInput {
            block,
            total_difficulty,
        } = input;
        let EthExecuteOutput {
            receipts,
            requests,
            gas_used,
        } = self.execute_without_verification(block, total_difficulty)?;

        // NOTE: we need to merge keep the reverts for the bundle retention
        self.state.merge_transitions(BundleRetention::Reverts);

        Ok(BlockExecutionOutput {
            state: self.state.take_bundle(),
            receipts,
            requests,
            gas_used,
        })
    }

    fn execute_with_state_witness<F>(
        mut self,
        input: Self::Input<'_>,
        mut witness: F,
    ) -> Result<Self::Output, Self::Error>
    where
        F: FnMut(&State<DB>),
    {
        let BlockExecutionInput {
            block,
            total_difficulty,
        } = input;
        let EthExecuteOutput {
            receipts,
            requests: _,
            gas_used,
        } = self.execute_without_verification(block, total_difficulty)?;

        // NOTE: we need to merge keep the reverts for the bundle retention
        self.state.merge_transitions(BundleRetention::Reverts);
        witness(&self.state);

        Ok(BlockExecutionOutput {
            state: self.state.take_bundle(),
            receipts,
            requests: vec![],
            gas_used,
        })
    }

    fn execute_with_state_hook<F>(
        mut self,
        input: Self::Input<'_>,
        state_hook: F,
    ) -> Result<Self::Output, Self::Error>
    where
        F: OnStateHook,
    {
        let BlockExecutionInput { block, total_difficulty } = input;
        let EthExecuteOutput { receipts, requests, gas_used } = self
            .execute_without_verification_with_state_hook(
                block,
                total_difficulty,
                Some(state_hook),
            )?;

        // NOTE: we need to merge keep the reverts for the bundle retention
        self.state.merge_transitions(BundleRetention::Reverts);
        Ok(BlockExecutionOutput { state: self.state.take_bundle(), receipts, requests, gas_used })
    }
}

/// An executor for a batch of blocks.
///
/// State changes are tracked until the executor is finalized.
#[derive(Debug)]
// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
pub struct GnosisBatchExecutor<EvmConfig, DB> {
    /// The executor used to execute blocks.
    executor: GnosisBlockExecutor<EvmConfig, DB>,
    /// Keeps track of the batch and record receipts based on the configured prune mode
    batch_record: BlockBatchRecord,
    // stats: BlockExecutorStats,
}

// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
impl<EvmConfig, DB> GnosisBatchExecutor<EvmConfig, DB> {
    /// Returns the receipts of the executed blocks.
    pub const fn receipts(&self) -> &Receipts {
        self.batch_record.receipts()
    }

    /// Returns mutable reference to the state that wraps the underlying database.
    #[allow(unused)]
    fn state_mut(&mut self) -> &mut State<DB> {
        self.executor.state_mut()
    }
}

// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
impl<EvmConfig, DB> BatchExecutor<DB> for GnosisBatchExecutor<EvmConfig, DB>
where
    EvmConfig: ConfigureEvm<Header = Header>,
    DB: Database<Error: Into<ProviderError> + Display>,
{
    type Input<'a> = BlockExecutionInput<'a, BlockWithSenders>;
    type Output = ExecutionOutcome;
    type Error = BlockExecutionError;

    // [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
    fn execute_and_verify_one(&mut self, input: Self::Input<'_>) -> Result<(), Self::Error> {
        let BlockExecutionInput {
            block,
            total_difficulty,
        } = input;
        let EthExecuteOutput {
            receipts,
            requests,
            gas_used: _,
        } = self
            .executor
            .execute_without_verification(block, total_difficulty)?;

        validate_block_post_execution(block, self.executor.chain_spec(), &receipts, &requests)?;

        // prepare the state according to the prune mode
        let retention = self.batch_record.bundle_retention(block.number);
        self.executor.state.merge_transitions(retention);

        // store receipts in the set
        self.batch_record.save_receipts(receipts)?;

        // store requests in the set
        self.batch_record.save_requests(requests);

        if self.batch_record.first_block().is_none() {
            self.batch_record.set_first_block(block.number);
        }

        Ok(())
    }

    // [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
    fn finalize(mut self) -> Self::Output {
        // self.stats.log_debug();

        ExecutionOutcome::new(
            self.executor.state.take_bundle(),
            self.batch_record.take_receipts(),
            self.batch_record.first_block().unwrap_or_default(),
            self.batch_record.take_requests(),
        )
    }

    // [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
    fn set_tip(&mut self, tip: BlockNumber) {
        self.batch_record.set_tip(tip);
    }

    // [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
    fn set_prune_modes(&mut self, prune_modes: PruneModes) {
        self.batch_record.set_prune_modes(prune_modes);
    }

    // [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
    fn size_hint(&self) -> Option<usize> {
        Some(self.executor.state.bundle_state.size_hint())
    }
}
