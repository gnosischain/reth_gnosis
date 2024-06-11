use crate::gnosis::{apply_block_rewards_contract_call, apply_withdrawals_contract_call};
use reth::{
    api::ConfigureEvm,
    primitives::{
        Address, BlockNumber, BlockWithSenders, ChainSpec, Header, PruneModes, Receipt, Receipts,
        Request, U256,
    },
    providers::ProviderError,
    revm::{
        batch::{BlockBatchRecord, BlockExecutorStats},
        db::states::bundle_state::BundleRetention,
        primitives::{BlockEnv, CfgEnvWithHandlerCfg, EnvWithHandlerCfg, ResultAndState},
        state_change::{
            apply_beacon_root_contract_call, apply_blockhashes_update,
            apply_withdrawal_requests_contract_call,
        },
        Database, DatabaseCommit, Evm, State,
    },
};
use reth_ethereum_consensus::validate_block_post_execution;
use reth_evm::execute::{
    BatchBlockExecutionOutput, BatchExecutor, BlockExecutionError, BlockExecutionInput,
    BlockExecutionOutput, BlockExecutorProvider, BlockValidationError, Executor,
};
use reth_evm_ethereum::eip6110::parse_deposits_from_receipts;
use std::{collections::HashMap, sync::Arc};

/// Helper container type for EVM with chain spec.
// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
#[derive(Debug, Clone)]
struct GnosisEvmExecutor<EvmConfig> {
    /// The chainspec
    chain_spec: Arc<ChainSpec>,
    /// How to create an EVM.
    evm_config: EvmConfig,
}

// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
impl<EvmConfig> GnosisEvmExecutor<EvmConfig>
where
    EvmConfig: ConfigureEvm,
{
    // [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthEvmExecutor
    fn execute_state_transitions<Ext, DB>(
        &self,
        block: &BlockWithSenders,
        mut evm: Evm<'_, Ext, &mut State<DB>>,
    ) -> Result<EthExecuteOutput, BlockExecutionError>
    where
        DB: Database<Error = ProviderError>,
    {
        // apply pre execution changes
        apply_beacon_root_contract_call(
            &self.chain_spec,
            block.timestamp,
            block.number,
            block.parent_beacon_block_root,
            &mut evm,
        )?;
        apply_blockhashes_update(
            evm.db_mut(),
            &self.chain_spec,
            block.timestamp,
            block.number,
            block.parent_hash,
        )?;

        // execute transactions
        let mut cumulative_gas_used = 0;
        let mut receipts = Vec::with_capacity(block.body.len());
        for (sender, transaction) in block.transactions_with_sender() {
            // The sum of the transaction‚Äôs gas limit, Tg, and the gas utilized in this block prior,
            // must be no greater than the block‚Äôs gasLimit.
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

            EvmConfig::fill_tx_env(evm.tx_mut(), transaction, *sender);

            // Execute transaction.
            let ResultAndState { result, state } = evm.transact().map_err(move |err| {
                // Ensure hash is calculated for error log, if not already done
                BlockValidationError::EVM {
                    hash: transaction.recalculate_hash(),
                    error: err.into(),
                }
            })?;
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

        let requests = if self
            .chain_spec
            .is_prague_active_at_timestamp(block.timestamp)
        {
            // Collect all EIP-6110 deposits
            let deposit_requests = parse_deposits_from_receipts(&self.chain_spec, &receipts)?;

            // Collect all EIP-7685 requests
            let withdrawal_requests = apply_withdrawal_requests_contract_call(&mut evm)?;

            [deposit_requests, withdrawal_requests].concat()
        } else {
            vec![]
        };

        Ok(EthExecuteOutput {
            receipts,
            requests,
            gas_used: cumulative_gas_used,
        })
    }
}

#[derive(Debug, Clone)]
pub struct GnosisExecutorProvider<EvmConfig: Clone> {
    chain_spec: Arc<ChainSpec>,
    evm_config: EvmConfig,
}

impl<EvmConfig: Clone> GnosisExecutorProvider<EvmConfig> {
    /// Creates a new executor provider.
    pub fn new(chain_spec: Arc<ChainSpec>, evm_config: EvmConfig) -> Self {
        Self {
            chain_spec,
            evm_config,
        }
    }
}

// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthExecutorProvider
impl<EvmConfig: Clone> GnosisExecutorProvider<EvmConfig>
where
    EvmConfig: ConfigureEvm,
{
    fn gnosis_executor<DB>(&self, db: DB) -> GnosisBlockExecutor<EvmConfig, DB>
    where
        DB: Database<Error = ProviderError>,
    {
        GnosisBlockExecutor::new(
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
    EvmConfig: ConfigureEvm,
{
    type Executor<DB: Database<Error = ProviderError>> = GnosisBlockExecutor<EvmConfig, DB>;
    type BatchExecutor<DB: Database<Error = ProviderError>> = GnosisBatchExecutor<EvmConfig, DB>;

    fn executor<DB>(&self, db: DB) -> Self::Executor<DB>
    where
        DB: Database<Error = ProviderError>,
    {
        self.gnosis_executor(db)
    }

    fn batch_executor<DB>(&self, db: DB, prune_modes: PruneModes) -> Self::BatchExecutor<DB>
    where
        DB: Database<Error = ProviderError>,
    {
        let executor = self.gnosis_executor(db);
        GnosisBatchExecutor {
            executor,
            batch_record: BlockBatchRecord::new(prune_modes),
            stats: BlockExecutorStats::default(),
        }
    }
}

// Struct required for BlockExecutorProvider trait
// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBlockExecutor
#[derive(Debug)]
pub struct GnosisBlockExecutor<EvmConfig, DB> {
    /// Chain specific evm config that's used to execute a block.
    executor: GnosisEvmExecutor<EvmConfig>,
    /// The state to use for execution
    state: State<DB>,
}

// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBlockExecutor
impl<EvmConfig, DB> GnosisBlockExecutor<EvmConfig, DB> {
    /// Creates a new Ethereum block executor.
    pub fn new(chain_spec: Arc<ChainSpec>, evm_config: EvmConfig, state: State<DB>) -> Self {
        Self {
            executor: GnosisEvmExecutor {
                chain_spec,
                evm_config,
            },
            state,
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
    EvmConfig: ConfigureEvm,
    DB: Database<Error = ProviderError>,
{
    /// Configures a new evm configuration and block environment for the given block.
    ///
    /// # Caution
    ///
    /// This does not initialize the tx environment.
    fn evm_env_for_block(&self, header: &Header, total_difficulty: U256) -> EnvWithHandlerCfg {
        let mut cfg = CfgEnvWithHandlerCfg::new(Default::default(), Default::default());
        let mut block_env = BlockEnv::default();
        EvmConfig::fill_cfg_and_block_env(
            &mut cfg,
            &mut block_env,
            self.chain_spec(),
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
            self.executor.execute_state_transitions(block, evm)
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

    /// Apply post execution state changes that do not require an [EVM](Evm), such as: block
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

        {
            let env = self.evm_env_for_block(&block.header, total_difficulty);
            let mut evm = self.executor.evm_config.evm_with_env(&mut self.state, env);

            apply_withdrawals_contract_call(
                &chain_spec,
                block.timestamp,
                block
                    .withdrawals
                    .as_ref()
                    .ok_or(BlockExecutionError::Other(
                        "block has no withdrawals field".to_owned().into(),
                    ))?,
                &mut evm,
            )?;
        }

        let balance_increments: HashMap<Address, u128> = {
            let env = self.evm_env_for_block(&block.header, total_difficulty);
            let mut evm = self.executor.evm_config.evm_with_env(&mut self.state, env);

            apply_block_rewards_contract_call(
                &chain_spec,
                block.timestamp,
                block.beneficiary,
                &mut evm,
            )?
        };

        // increment balances
        self.state
            .increment_balances(balance_increments)
            .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;

        Ok(())
    }
}

/// Helper type for the output of executing a block.
// [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthExecuteOutput
#[derive(Debug, Clone)]
struct EthExecuteOutput {
    receipts: Vec<Receipt>,
    requests: Vec<Request>,
    gas_used: u64,
}

// Trait required by BlockExecutorProvider associated type Executor
impl<EvmConfig, DB> Executor<DB> for GnosisBlockExecutor<EvmConfig, DB>
where
    EvmConfig: ConfigureEvm,
    DB: Database<Error = ProviderError>,
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
    stats: BlockExecutorStats,
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
    EvmConfig: ConfigureEvm,
    DB: Database<Error = ProviderError>,
{
    type Input<'a> = BlockExecutionInput<'a, BlockWithSenders>;
    type Output = BatchBlockExecutionOutput;
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
        self.stats.log_debug();

        BatchBlockExecutionOutput::new(
            self.executor.state.take_bundle(),
            self.batch_record.take_receipts(),
            self.batch_record.take_requests(),
            self.batch_record.first_block().unwrap_or_default(),
        )
    }

    // [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
    fn set_tip(&mut self, tip: BlockNumber) {
        self.batch_record.set_tip(tip);
    }

    // [Gnosis/fork] Copy paste code from crates/ethereum/evm/src/execute.rs::EthBatchExecutor
    fn size_hint(&self) -> Option<usize> {
        Some(self.executor.state.bundle_state.size_hint())
    }
}
