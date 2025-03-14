extern crate alloc;
use crate::evm_config::GnosisEvmConfig;
use crate::gnosis::{add_blob_fee_collection_to_balance_increments, apply_post_block_system_calls};
use crate::spec::GnosisChainSpec;
use alloc::{boxed::Box, sync::Arc};
use alloy_consensus::{BlockHeader, Transaction as _};
use alloy_eips::eip7685::Requests;
use alloy_eips::{eip6110, eip7002, eip7251};
use alloy_primitives::Address;
use reth_chainspec::EthereumHardforks;
use reth_errors::ConsensusError;
use reth_ethereum_consensus::validate_block_post_execution;
use reth_evm::system_calls::OnStateHook;
use reth_evm::{
    execute::{
        BlockExecutionError, BlockExecutionStrategy, BlockExecutionStrategyFactory,
        BlockValidationError, ExecuteOutput,
    },
    system_calls::SystemCaller,
    ConfigureEvm, Database, Evm,
};
use reth_evm_ethereum::eip6110::parse_deposits_from_receipts;
use reth_node_ethereum::BasicBlockExecutorProvider;
use reth_primitives::Block;
use reth_primitives::EthPrimitives;
use reth_primitives::{Receipt, RecoveredBlock};
use reth_primitives_traits::{BlockBody, SignedTransaction};
use reth_revm::db::State;
use revm_primitives::{db::DatabaseCommit, ResultAndState};

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
    chain_spec: Arc<GnosisChainSpec>,
    evm_config: EvmConfig,
}

impl<EvmConfig> GnosisExecutionStrategyFactory<EvmConfig> {
    // Create a new executor strategy factory
    pub fn new(chain_spec: Arc<GnosisChainSpec>, evm_config: EvmConfig) -> eyre::Result<Self> {
        Ok(Self {
            chain_spec,
            evm_config,
        })
    }
}

impl<EvmConfig> BlockExecutionStrategyFactory for GnosisExecutionStrategyFactory<EvmConfig>
where
    EvmConfig: Clone
        + Unpin
        + Sync
        + Send
        + 'static
        + ConfigureEvm<
            Header = alloy_consensus::Header,
            Transaction = reth_primitives::TransactionSigned,
        >,
{
    type Primitives = EthPrimitives;
    type Strategy<DB: Database> = GnosisExecutionStrategy<DB, EvmConfig>;

    fn create_strategy<DB>(&self, db: DB) -> Self::Strategy<DB>
    where
        DB: Database,
    {
        let state = State::builder()
            .with_database(db)
            .with_bundle_update()
            .without_state_clear()
            .build();
        GnosisExecutionStrategy::new(state, self.chain_spec.clone(), self.evm_config.clone())
    }
}

// Block execution strategy for Gnosis
#[allow(missing_debug_implementations)]
pub struct GnosisExecutionStrategy<DB, EvmConfig>
where
    EvmConfig: Clone,
{
    /// The chainspec
    chain_spec: Arc<GnosisChainSpec>,
    /// How to create an EVM.
    evm_config: EvmConfig,
    /// Current state for block execution.
    state: State<DB>,
    /// Utility to call system smart contracts.
    system_caller: SystemCaller<EvmConfig, GnosisChainSpec>,
    /// BlockRewards contract address
    block_rewards_contract: Address,
    /// EIP-1559 and EIP-4844 collector address
    fee_collector_contract: Address,
}

impl<DB, EvmConfig> GnosisExecutionStrategy<DB, EvmConfig>
where
    EvmConfig: Clone,
{
    pub fn new(state: State<DB>, chain_spec: Arc<GnosisChainSpec>, evm_config: EvmConfig) -> Self {
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

        let fee_collector_contract = chain_spec
            .genesis()
            .config
            .extra_fields
            .get("eip1559collector")
            .expect("no eip1559collector field");
        let fee_collector_contract: Address =
            serde_json::from_value(fee_collector_contract.clone())
                .expect("failed to parse eip1559collector field");

        Self {
            state,
            chain_spec,
            evm_config,
            system_caller,
            block_rewards_contract,
            fee_collector_contract,
        }
    }
}

impl<DB, EvmConfig> BlockExecutionStrategy for GnosisExecutionStrategy<DB, EvmConfig>
where
    DB: Database,
    EvmConfig: ConfigureEvm<
        Header = alloy_consensus::Header,
        Transaction = reth_primitives::TransactionSigned,
    >,
{
    type DB = DB;
    type Error = BlockExecutionError;
    type Primitives = EthPrimitives;

    fn apply_pre_execution_changes(
        &mut self,
        block: &RecoveredBlock<Block>,
    ) -> Result<(), Self::Error> {
        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag =
            (*self.chain_spec).is_spurious_dragon_active_at_block(block.number());
        self.state.set_state_clear_flag(state_clear_flag);

        let mut evm = self
            .evm_config
            .evm_for_block(&mut self.state, block.header());

        self.system_caller
            .apply_pre_execution_changes(block.header(), &mut evm)?;

        Ok(())
    }

    fn execute_transactions(
        &mut self,
        block: &RecoveredBlock<Block>,
    ) -> Result<ExecuteOutput<Receipt>, Self::Error> {
        let mut evm = self
            .evm_config
            .evm_for_block(&mut self.state, block.header());

        let mut cumulative_gas_used = 0;
        let mut receipts = Vec::with_capacity(block.body().transaction_count());
        for (sender, transaction) in block.transactions_with_sender() {
            // The sum of the transaction’s gas limit, Tg, and the gas utilized in this block prior,
            // must be no greater than the block’s gasLimit.
            let block_available_gas = block.gas_limit() - cumulative_gas_used;
            if transaction.gas_limit() > block_available_gas {
                return Err(
                    BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                        transaction_gas_limit: transaction.gas_limit(),
                        block_available_gas,
                    }
                    .into(),
                );
            }

            let tx_env = self.evm_config.tx_env(transaction, *sender);

            // Execute transaction.
            let result_and_state = evm.transact(tx_env).map_err(move |err| {
                // Ensure hash is calculated for error log, if not already done
                BlockValidationError::EVM {
                    hash: transaction.recalculate_hash(),
                    error: Box::new(err),
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
        block: &RecoveredBlock<Block>,
        receipts: &[Receipt],
    ) -> Result<Requests, Self::Error> {
        let mut evm = self
            .evm_config
            .evm_for_block(&mut self.state, block.header());

        let blob_fee_to_collect = if self
            .chain_spec
            .is_prague_active_at_timestamp(block.timestamp)
        {
            let blob_gasprice = evm.block().get_blob_gasprice().unwrap_or(0);
            let blob_gas_used = block.blob_gas_used().unwrap_or(0) as u128;
            blob_gas_used * blob_gasprice
        } else {
            0
        };

        let (mut balance_increments, withdrawal_requests) = apply_post_block_system_calls(
            &self.chain_spec,
            self.block_rewards_contract,
            block.timestamp,
            block.body().withdrawals.as_ref(),
            block.beneficiary,
            &mut evm,
        )?;

        if self
            .chain_spec
            .is_prague_active_at_timestamp(block.timestamp)
        {
            add_blob_fee_collection_to_balance_increments(
                &mut balance_increments,
                self.fee_collector_contract,
                blob_fee_to_collect,
            );
        }

        let requests = if self
            .chain_spec
            .is_prague_active_at_timestamp(block.timestamp)
        {
            // Collect all EIP-6110 deposits

            let mut requests = Requests::default();

            let deposit_requests = parse_deposits_from_receipts(&self.chain_spec, receipts)?;
            if !deposit_requests.is_empty() {
                requests.push_request_with_type(eip6110::DEPOSIT_REQUEST_TYPE, deposit_requests);
            }

            if !withdrawal_requests.is_empty() {
                requests
                    .push_request_with_type(eip7002::WITHDRAWAL_REQUEST_TYPE, withdrawal_requests);
            }

            // Collect all EIP-7251 requests
            let consolidation_requests = self
                .system_caller
                .apply_consolidation_requests_contract_call(&mut evm)?;
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

        drop(evm);

        self.state
            .increment_balances(balance_increments.clone())
            .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;

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
        block: &RecoveredBlock<Block>,
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
        chain_spec: Arc<GnosisChainSpec>,
    ) -> BasicBlockExecutorProvider<GnosisExecutionStrategyFactory> {
        let evm_config = GnosisEvmConfig::new(chain_spec.clone());
        BasicBlockExecutorProvider::new(
            GnosisExecutionStrategyFactory::new(chain_spec, evm_config).unwrap(),
        )
    }
}
