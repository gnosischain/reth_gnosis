use std::sync::Arc;

use reth::{
    api::{ConfigureEvm, ConfigureEvmEnv},
    primitives::{
        Address, BlockWithSenders, ChainSpec, Header, Receipt, Request, TransactionSigned, U256,
    },
    providers::ProviderError,
    revm::{
        db::states::bundle_state::BundleRetention,
        primitives::{CfgEnvWithHandlerCfg, TxEnv},
        Database, Evm, EvmBuilder, State,
    },
};
use reth_evm::execute::{
    BlockExecutionError, BlockExecutionInput, BlockExecutionOutput, BlockExecutorProvider, Executor,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct GnosisEvmConfig {}

// Trait required by ExecutorBuilder
impl ConfigureEvm for GnosisEvmConfig {
    type DefaultExternalContext<'a> = ();

    fn evm<'a, DB: Database + 'a>(
        &'a self,
        db: DB,
    ) -> Evm<'a, Self::DefaultExternalContext<'a>, DB> {
        EvmBuilder::default().with_db(db).build()
    }
}

// Trait required by ConfigureEvm
impl ConfigureEvmEnv for GnosisEvmConfig {
    fn fill_tx_env(_tx_env: &mut TxEnv, _transaction: &TransactionSigned, _sender: Address) {
        todo!();
    }

    fn fill_cfg_env(
        _cfg_env: &mut CfgEnvWithHandlerCfg,
        _chain_spec: &ChainSpec,
        _header: &Header,
        _total_difficulty: U256,
    ) {
        todo!();
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GnosisExecutorProvider<EvmConfig> {
    evm_config: EvmConfig,
}

impl<EvmConfig> GnosisExecutorProvider<EvmConfig> {
    /// Creates a new executor provider.
    pub fn new(_chain_spec: Arc<ChainSpec>, evm_config: EvmConfig) -> Self {
        Self { evm_config }
    }
}

// Trait required by ExecutorBuilder
impl<EvmConfig> BlockExecutorProvider for GnosisExecutorProvider<EvmConfig> {
    type Executor<DB: Database<Error = ProviderError>> = GnosisBlockExecutor<EvmConfig, DB>;
    type BatchExecutor<DB: Database<Error = ProviderError>> = GnosisBatchExecutor<EvmConfig, DB>;
}

// Struct required for BlockExecutorProvider trait
#[derive(Debug, Default, Clone, Copy)]
struct GnosisBlockExecutor<EvmConfig, DB> {
    /// Chain specific evm config that's used to execute a block.
    executor: GnosisEvmConfig<EvmConfig>,
    /// The state to use for execution
    state: State<DB>,
}

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

    /// No diff with EthBlockExecutor
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

    /// No diff with EthBlockExecutor
    fn on_new_block(&mut self, header: &Header) {
        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag = self
            .chain_spec()
            .is_spurious_dragon_active_at_block(header.number);
        self.state.set_state_clear_flag(state_clear_flag);
    }

    #[inline]
    fn chain_spec(&self) -> &ChainSpec {
        &self.executor.chain_spec
    }
}

/// Helper type for the output of executing a block.
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

    // EthBlockExecutor Executor impl in:
    // crates/ethereum/evm/src/execute.rs:352
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
