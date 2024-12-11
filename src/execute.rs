extern crate alloc;
use crate::evm_config::GnosisEvmConfig;

use crate::gnosis::apply_post_block_system_calls;
use alloc::{boxed::Box, sync::Arc};
use alloy_consensus::{Header, Transaction as _};
use alloy_eips::eip7685::Requests;
use alloy_primitives::Address;
use core::fmt::Display;
use reth_chainspec::ChainSpec;
use reth_chainspec::EthereumHardforks;
use reth_errors::ConsensusError;
use reth_ethereum_consensus::validate_block_post_execution;
use reth_evm::system_calls::OnStateHook;
use reth_evm::TxEnvOverrides;
use reth_evm::{
    execute::{
        BlockExecutionError, BlockExecutionStrategy, BlockExecutionStrategyFactory,
        BlockValidationError, ExecuteOutput, ProviderError,
    },
    system_calls::SystemCaller,
    ConfigureEvm,
};
use reth_evm_ethereum::eip6110::parse_deposits_from_receipts;
use reth_node_ethereum::BasicBlockExecutorProvider;
use reth_primitives::EthPrimitives;
use reth_primitives::{BlockWithSenders, Receipt};
use reth_revm::db::State;
use revm_primitives::{
    db::{Database, DatabaseCommit},
    BlockEnv, CfgEnvWithHandlerCfg, EnvWithHandlerCfg, ResultAndState, U256,
};

// We need to have block rewards contract address in the executor provider
// because it's used in the post execution system calls.

// in post_execution,
// [Gnosis/fork:DIFF]: Upstream code in EthBlockExecutor computes balance changes for:
// - Pre-merge omer and block rewards
// - Beacon withdrawal mints
// - DAO hardfork drain balances
//
// For gnosis instead:
// - Do NOT credit withdrawals as native token mint
// - Call into deposit contract with withdrawal data
// - Call block rewards contract for bridged xDAI mint

// Factory for [`GnosisExecutionStrategy`]
#[derive(Debug, Clone)]
pub struct GnosisExecutionStrategyFactory<EvmConfig = GnosisEvmConfig> {
    chain_spec: Arc<ChainSpec>,
    evm_config: EvmConfig,
}

impl<EvmConfig> GnosisExecutionStrategyFactory<EvmConfig> {
    // Create a new executor strategy factory
    pub fn new(chain_spec: Arc<ChainSpec>, evm_config: EvmConfig) -> eyre::Result<Self> {
        Ok(Self {
            chain_spec,
            evm_config,
        })
    }
}

impl<EvmConfig> BlockExecutionStrategyFactory for GnosisExecutionStrategyFactory<EvmConfig>
where
    EvmConfig:
        Clone + Unpin + Sync + Send + 'static + ConfigureEvm<Header = alloy_consensus::Header>,
{
    type Strategy<DB: Database<Error: Into<ProviderError> + Display>> =
        GnosisExecutionStrategy<DB, EvmConfig>;

    fn create_strategy<DB>(&self, db: DB) -> Self::Strategy<DB>
    where
        DB: Database<Error: Into<ProviderError> + Display>,
    {
        let state = State::builder()
            .with_database(db)
            .with_bundle_update()
            .without_state_clear()
            .build();
        GnosisExecutionStrategy::new(state, self.chain_spec.clone(), self.evm_config.clone())
    }

    type Primitives = EthPrimitives;
}

// Block execution strategy for Gnosis
#[allow(missing_debug_implementations)]
pub struct GnosisExecutionStrategy<DB, EvmConfig>
where
    EvmConfig: Clone,
{
    /// The chainspec
    chain_spec: Arc<ChainSpec>,
    /// How to create an EVM.
    evm_config: EvmConfig,
    /// Current state for block execution.
    state: State<DB>,
    /// Utility to call system smart contracts.
    system_caller: SystemCaller<EvmConfig, ChainSpec>,
    /// BlockRewards contract address
    block_rewards_contract: Address,
    /// Optional overrides for the transactions environment.
    tx_env_overrides: Option<Box<dyn TxEnvOverrides>>,
}

impl<DB, EvmConfig> GnosisExecutionStrategy<DB, EvmConfig>
where
    EvmConfig: Clone,
{
    pub fn new(state: State<DB>, chain_spec: Arc<ChainSpec>, evm_config: EvmConfig) -> Self {
        let system_caller = SystemCaller::new(evm_config.clone(), chain_spec.clone());
        let block_rewards_contract = chain_spec
            .genesis()
            .config
            .extra_fields
            .get("blockRewardsContract")
            .expect("blockRewardsContract not defined")
            .clone();
        let block_rewards_contract: Address = serde_json::from_value(block_rewards_contract)
            .expect("blockRewardsContract not an address");
        Self {
            state,
            chain_spec,
            evm_config,
            system_caller,
            block_rewards_contract,
            tx_env_overrides: None,
        }
    }
}

impl<DB, EvmConfig> GnosisExecutionStrategy<DB, EvmConfig>
where
    DB: Database<Error: Into<ProviderError> + Display>,
    EvmConfig: ConfigureEvm<Header = alloy_consensus::Header>,
{
    /// Configures a new evm configuration and block environment for the given block.
    ///
    /// Caution: this does not initialize the tx environment.
    fn evm_env_for_block(&self, header: &Header, total_difficulty: U256) -> EnvWithHandlerCfg {
        let mut cfg = CfgEnvWithHandlerCfg::new(Default::default(), Default::default());
        let mut block_env = BlockEnv::default();
        self.evm_config
            .fill_cfg_and_block_env(&mut cfg, &mut block_env, header, total_difficulty);

        EnvWithHandlerCfg::new_with_cfg_env(cfg, block_env, Default::default())
    }
}

impl<DB, EvmConfig> BlockExecutionStrategy for GnosisExecutionStrategy<DB, EvmConfig>
where
    DB: Database<Error: Into<ProviderError> + Display>,
    EvmConfig: ConfigureEvm<Header = alloy_consensus::Header>,
{
    type DB = DB;
    type Error = BlockExecutionError;

    type Primitives = EthPrimitives;

    fn init(&mut self, tx_env_overrides: Box<dyn TxEnvOverrides>) {
        self.tx_env_overrides = Some(tx_env_overrides);
    }

    fn apply_pre_execution_changes(
        &mut self,
        block: &BlockWithSenders,
        total_difficulty: U256,
    ) -> Result<(), Self::Error> {
        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag =
            (*self.chain_spec).is_spurious_dragon_active_at_block(block.header.number);
        self.state.set_state_clear_flag(state_clear_flag);

        let env = self.evm_env_for_block(&block.header, total_difficulty);
        let mut evm = self.evm_config.evm_with_env(&mut self.state, env);

        self.system_caller
            .apply_pre_execution_changes(block, &mut evm)?;

        Ok(())
    }

    fn execute_transactions(
        &mut self,
        block: &BlockWithSenders,
        total_difficulty: U256,
    ) -> Result<ExecuteOutput<Receipt>, Self::Error> {
        let env = self.evm_env_for_block(&block.header, total_difficulty);
        let mut evm = self.evm_config.evm_with_env(&mut self.state, env);

        let mut cumulative_gas_used = 0;
        let mut receipts = Vec::with_capacity(block.body.transactions.len());
        for (sender, transaction) in block.transactions_with_sender() {
            // The sum of the transaction’s gas limit, Tg, and the gas utilized in this block prior,
            // must be no greater than the block’s gasLimit.
            let block_available_gas = block.header.gas_limit - cumulative_gas_used;
            if transaction.gas_limit() > block_available_gas {
                return Err(
                    BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                        transaction_gas_limit: transaction.gas_limit(),
                        block_available_gas,
                    }
                    .into(),
                );
            }

            self.evm_config
                .fill_tx_env(evm.tx_mut(), transaction, *sender);

            if let Some(tx_env_overrides) = &mut self.tx_env_overrides {
                tx_env_overrides.apply(evm.tx_mut());
            }

            // Execute transaction.
            let result_and_state = evm.transact().map_err(move |err| {
                let new_err = err.map_db_err(|e| e.into());
                // Ensure hash is calculated for error log, if not already done
                BlockValidationError::EVM {
                    hash: transaction.recalculate_hash(),
                    error: Box::new(new_err),
                }
            })?;
            self.system_caller.on_state(&result_and_state.state);
            let ResultAndState { result, state } = result_and_state;
            evm.db_mut().commit(state);

            // append gas used
            cumulative_gas_used += result.gas_used();

            // Push transaction changeset and calculate header bloom filter for receipt.
            receipts.push(
                #[allow(clippy::needless_update)] // side-effect of optimism fields
                Receipt {
                    tx_type: transaction.tx_type(),
                    // Success flag was added in `EIP-658: Embedding transaction status code in
                    // receipts`.
                    success: result.is_success(),
                    cumulative_gas_used,
                    // convert to reth log
                    logs: result.into_logs(),
                    ..Default::default()
                },
            );
        }
        Ok(ExecuteOutput {
            receipts,
            gas_used: cumulative_gas_used,
        })
    }

    fn apply_post_execution_changes(
        &mut self,
        block: &BlockWithSenders,
        total_difficulty: U256,
        receipts: &[Receipt],
    ) -> Result<Requests, Self::Error> {
        let env = self.evm_env_for_block(&block.header, total_difficulty);
        let mut evm = self.evm_config.evm_with_env(&mut self.state, env);

        let balance_increments = apply_post_block_system_calls(
            &self.chain_spec,
            &self.evm_config,
            self.block_rewards_contract,
            block.timestamp,
            block.body.withdrawals.as_ref(),
            block.beneficiary,
            &mut evm,
        )?;

        drop(evm);

        self.state
            .increment_balances(balance_increments.clone())
            .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;

        let requests = if self
            .chain_spec
            .is_prague_active_at_timestamp(block.timestamp)
        {
            // Collect all EIP-6110 deposits
            let deposit_requests = parse_deposits_from_receipts(&self.chain_spec, receipts)?;

            Requests::new(vec![deposit_requests])
        } else {
            Requests::default()
        };

        Ok(requests)
    }

    fn state_ref(&self) -> &State<DB> {
        &self.state
    }

    fn state_mut(&mut self) -> &mut State<DB> {
        &mut self.state
    }

    fn with_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.system_caller.with_state_hook(hook);
    }

    fn validate_block_post_execution(
        &self,
        block: &BlockWithSenders,
        receipts: &[Receipt],
        requests: &Requests,
    ) -> Result<(), ConsensusError> {
        validate_block_post_execution(block, &self.chain_spec.clone(), receipts, requests)
    }
}

/// Helper type with backwards compatible methods to obtain executor providers.
#[derive(Debug, Clone)]
pub struct GnosisExecutorProvider;

impl GnosisExecutorProvider {
    /// Creates a new default gnosis executor strategy factory.
    pub fn gnosis(
        chain_spec: Arc<ChainSpec>,
    ) -> BasicBlockExecutorProvider<GnosisExecutionStrategyFactory> {
        let collector_address = chain_spec
            .genesis()
            .config
            .extra_fields
            .get("eip1559collector")
            .unwrap();
        let collector_address: Address = serde_json::from_value(collector_address.clone()).unwrap();
        let evm_config = GnosisEvmConfig::new(collector_address, chain_spec.clone());
        BasicBlockExecutorProvider::new(
            GnosisExecutionStrategyFactory::new(chain_spec, evm_config).unwrap(),
        )
    }
}
