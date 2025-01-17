use std::sync::Arc;

use alloy_consensus::{Header, EMPTY_OMMER_ROOT_HASH};
use alloy_eips::{
    eip4844::MAX_DATA_GAS_PER_BLOCK, eip7002::WITHDRAWAL_REQUEST_TYPE,
    eip7251::CONSOLIDATION_REQUEST_TYPE, eip7685::Requests, merge::BEACON_NONCE,
};
use eyre::eyre;
use reth::{
    api::{FullNodeTypes, NodeTypesWithEngine},
    builder::{components::PayloadServiceBuilder, BuilderContext, PayloadBuilderConfig},
    payload::{
        EthBuiltPayload, EthPayloadBuilderAttributes, PayloadBuilderError, PayloadBuilderHandle,
        PayloadBuilderService,
    },
    revm::database::StateProviderDatabase,
    transaction_pool::{
        error::InvalidPoolTransactionError, noop::NoopTransactionPool, BestTransactions,
        BestTransactionsAttributes, PoolTransaction, TransactionPool, ValidPoolTransaction,
    },
};
use reth_basic_payload_builder::{
    commit_withdrawals, is_better_payload, BasicPayloadJobGenerator,
    BasicPayloadJobGeneratorConfig, BuildArguments, BuildOutcome, PayloadBuilder, PayloadConfig,
};
use reth_chain_state::ExecutedBlock;
use reth_chainspec::EthereumHardforks;
use reth_errors::RethError;
use reth_evm::{system_calls::SystemCaller, ConfigureEvm, ConfigureEvmEnv, NextBlockEnvAttributes};
use reth_evm_ethereum::eip6110::parse_deposits_from_receipts;
use reth_node_ethereum::EthEngineTypes;
use reth_primitives::{
    proofs::{self},
    Block, BlockBody, BlockExt, EthPrimitives, InvalidTransactionError, Receipt, TransactionSigned,
};
use reth_provider::{
    CanonStateSubscriptions, ChainSpecProvider, ExecutionOutcome, StateProviderFactory,
};
use reth_trie::HashedPostState;
use revm::{
    db::{states::bundle_state::BundleRetention, State},
    DatabaseCommit,
};
use revm_primitives::{
    calc_excess_blob_gas, Address, BlockEnv, CfgEnvWithHandlerCfg, EVMError, EnvWithHandlerCfg,
    InvalidTransaction, ResultAndState, TxEnv, U256,
};
use tracing::{debug, trace, warn};

use crate::{
    evm_config::GnosisEvmConfig, gnosis::apply_post_block_system_calls, spec::GnosisChainSpec,
};

type BestTransactionsIter<Pool> = Box<
    dyn BestTransactions<Item = Arc<ValidPoolTransaction<<Pool as TransactionPool>::Transaction>>>,
>;

/// A basic Gnosis payload service builder
#[derive(Debug, Default, Clone)]
pub struct GnosisPayloadServiceBuilder {
    // The EVM configuration to use for the payload builder.
}

impl GnosisPayloadServiceBuilder {
    /// Create a new instance with the given evm config.
    pub const fn new() -> Self {
        Self {}
    }
}

impl<Node, Pool> PayloadServiceBuilder<Node, Pool> for GnosisPayloadServiceBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypesWithEngine<
            Engine = EthEngineTypes,
            ChainSpec = GnosisChainSpec,
            Primitives = EthPrimitives,
        >,
    >,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>
        + Unpin
        + 'static,
{
    async fn spawn_payload_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<PayloadBuilderHandle<<Node::Types as NodeTypesWithEngine>::Engine>> {
        let chain_spec = ctx.chain_spec();
        let block_rewards_contract = chain_spec
            .genesis()
            .config
            .extra_fields
            .get("blockRewardsContract")
            .ok_or(eyre!("blockRewardsContract not defined"))?;
        let block_rewards_contract: Address =
            serde_json::from_value(block_rewards_contract.clone())?;

        let collector_address = ctx
            .config()
            .chain
            .genesis()
            .config
            .extra_fields
            .get("eip1559collector")
            .ok_or(eyre!("no eip1559collector field"))?;
        let collector_address: Address = serde_json::from_value(collector_address.clone())?;

        let payload_builder = GnosisPayloadBuilder::new(
            GnosisEvmConfig::new(collector_address, chain_spec),
            block_rewards_contract,
        );
        let conf = ctx.payload_builder_config();

        let payload_job_config = BasicPayloadJobGeneratorConfig::default()
            .interval(conf.interval())
            .deadline(conf.deadline())
            .max_payload_tasks(conf.max_payload_tasks())
            .extradata(conf.extradata_bytes());

        let payload_generator = BasicPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            pool,
            ctx.task_executor().clone(),
            payload_job_config,
            payload_builder,
        );
        let (payload_service, payload_builder) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        ctx.task_executor()
            .spawn_critical("payload builder service", Box::pin(payload_service));

        Ok(payload_builder)
    }
}

/// Ethereum payload builder
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GnosisPayloadBuilder<GnosisEvmConfig> {
    /// The type responsible for creating the evm.
    evm_config: GnosisEvmConfig,
    /// AuRa BlockRewards contract address for its system call
    block_rewards_contract: Address,
}

impl<EvmConfig> GnosisPayloadBuilder<EvmConfig>
where
    EvmConfig: ConfigureEvmEnv<Header = Header>,
{
    pub const fn new(evm_config: EvmConfig, block_rewards_contract: Address) -> Self {
        Self {
            evm_config,
            block_rewards_contract,
        }
    }
}

impl<EvmConfig> GnosisPayloadBuilder<EvmConfig>
where
    EvmConfig: ConfigureEvmEnv<Header = Header>,
{
    /// Returns the configured [`CfgEnvWithHandlerCfg`] and [`BlockEnv`] for the targeted payload
    /// (that has the `parent` as its parent).
    pub fn cfg_and_block_env(
        &self,
        config: &PayloadConfig<EthPayloadBuilderAttributes>,
        parent: &Header,
    ) -> Result<(CfgEnvWithHandlerCfg, BlockEnv), EvmConfig::Error> {
        let next_attributes = NextBlockEnvAttributes {
            timestamp: config.attributes.timestamp,
            suggested_fee_recipient: config.attributes.suggested_fee_recipient,
            prev_randao: config.attributes.prev_randao,
        };
        self.evm_config
            .next_cfg_and_block_env(parent, next_attributes)
    }
}

// Default implementation of [PayloadBuilder] for unit type
impl<EvmConfig, Pool, Client> PayloadBuilder<Pool, Client> for GnosisPayloadBuilder<EvmConfig>
where
    EvmConfig: ConfigureEvm<Header = Header>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = GnosisChainSpec>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
{
    type Attributes = EthPayloadBuilderAttributes;
    type BuiltPayload = EthBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Pool, Client, EthPayloadBuilderAttributes, EthBuiltPayload>,
    ) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError> {
        let (cfg_env, block_env) = self
            .cfg_and_block_env(&args.config, &args.config.parent_header)
            .map_err(PayloadBuilderError::other)?;
        let pool = args.pool.clone();
        default_ethereum_payload(
            self.evm_config.clone(),
            args,
            cfg_env,
            block_env,
            self.block_rewards_contract,
            |attributes| pool.best_transactions_with_attributes(attributes),
        )
    }

    fn build_empty_payload(
        &self,
        client: &Client,
        config: PayloadConfig<Self::Attributes>,
    ) -> Result<EthBuiltPayload, PayloadBuilderError> {
        let args = BuildArguments {
            client,
            config,
            pool: NoopTransactionPool::default(),
            cached_reads: Default::default(),
            cancel: Default::default(),
            best_payload: None,
        };
        let (cfg_env, block_env) = self
            .cfg_and_block_env(&args.config, &args.config.parent_header)
            .map_err(PayloadBuilderError::other)?;

        let pool = args.pool.clone();
        default_ethereum_payload(
            self.evm_config.clone(),
            args,
            cfg_env,
            block_env,
            self.block_rewards_contract,
            |attributes| pool.best_transactions_with_attributes(attributes),
        )?
        .into_payload()
        .ok_or_else(|| PayloadBuilderError::MissingPayload)
    }
}

/// Constructs an Ethereum transaction payload from the transactions sent through the
/// Payload attributes by the sequencer. If the `no_tx_pool` argument is passed in
/// the payload attributes, the transaction pool will be ignored and the only transactions
/// included in the payload will be those sent through the attributes.
///
/// Given build arguments including an Ethereum client, transaction pool,
/// and configuration, this function creates a transaction payload. Returns
/// a result indicating success with the payload or an error in case of failure.
#[inline]
pub fn default_ethereum_payload<EvmConfig, Pool, Client, F>(
    evm_config: EvmConfig,
    args: BuildArguments<Pool, Client, EthPayloadBuilderAttributes, EthBuiltPayload>,
    initialized_cfg: CfgEnvWithHandlerCfg,
    initialized_block_env: BlockEnv,
    block_rewards_contract: Address,
    best_txs: F,
) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError>
where
    EvmConfig: ConfigureEvm<Header = Header>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = GnosisChainSpec>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
    F: FnOnce(BestTransactionsAttributes) -> BestTransactionsIter<Pool>,
{
    let BuildArguments {
        client,
        pool,
        mut cached_reads,
        config,
        cancel,
        best_payload,
    } = args;
    let chain_spec = client.chain_spec();
    let state_provider = client.state_by_block_hash(config.parent_header.hash())?;
    let state = StateProviderDatabase::new(state_provider);
    let mut db = State::builder()
        .with_database(cached_reads.as_db_mut(state))
        .with_bundle_update()
        .build();
    let PayloadConfig {
        parent_header,
        extra_data,
        attributes,
    } = config;

    debug!(target: "payload_builder", id=%attributes.id, parent_hash = ?parent_header.hash(), parent_number = parent_header.number, "building new payload");
    let mut cumulative_gas_used = 0;
    let mut sum_blob_gas_used = 0;
    let block_gas_limit: u64 = initialized_block_env.gas_limit.to::<u64>();
    let base_fee = initialized_block_env.basefee.to::<u64>();

    let mut executed_txs = Vec::new();
    let mut executed_senders = Vec::new();

    let mut best_txs = best_txs(BestTransactionsAttributes::new(
        base_fee,
        initialized_block_env
            .get_blob_gasprice()
            .map(|gasprice| gasprice as u64),
    ));
    let mut total_fees = U256::ZERO;

    let block_number = initialized_block_env.number.to::<u64>();

    let mut system_caller = SystemCaller::new(evm_config.clone(), chain_spec.clone());

    // apply eip-4788 pre block contract call
    system_caller
        .pre_block_beacon_root_contract_call(
            &mut db,
            &initialized_cfg,
            &initialized_block_env,
            attributes.parent_beacon_block_root,
        )
        .map_err(|err| {
            warn!(target: "payload_builder",
                parent_hash=%parent_header.hash(),
                %err,
                "failed to apply beacon root contract call for payload"
            );
            PayloadBuilderError::Internal(err.into())
        })?;

    // apply eip-2935 blockhashes update
    system_caller.pre_block_blockhashes_contract_call(
        &mut db,
        &initialized_cfg,
        &initialized_block_env,
        parent_header.hash(),
    )
    .map_err(|err| {
        warn!(target: "payload_builder", parent_hash=%parent_header.hash(), %err, "failed to update blockhashes for payload");
        PayloadBuilderError::Internal(err.into())
    })?;

    let env = EnvWithHandlerCfg::new_with_cfg_env(
        initialized_cfg.clone(),
        initialized_block_env.clone(),
        TxEnv::default(),
    );
    let mut evm = evm_config.evm_with_env(&mut db, env);

    let mut receipts = Vec::new();
    while let Some(pool_tx) = best_txs.next() {
        // ensure we still have capacity for this transaction
        if cumulative_gas_used + pool_tx.gas_limit() > block_gas_limit {
            // we can't fit this transaction into the block, so we need to mark it as invalid
            // which also removes all dependent transaction from the iterator before we can
            // continue
            best_txs.mark_invalid(
                &pool_tx,
                InvalidPoolTransactionError::ExceedsGasLimit(pool_tx.gas_limit(), block_gas_limit),
            );
            continue;
        }

        // check if the job was cancelled, if so we can exit early
        if cancel.is_cancelled() {
            return Ok(BuildOutcome::Cancelled);
        }

        // convert tx to a signed transaction
        let tx = pool_tx.to_consensus();

        // There's only limited amount of blob space available per block, so we need to check if
        // the EIP-4844 can still fit in the block
        if let Some(blob_tx) = tx.transaction.as_eip4844() {
            let tx_blob_gas = blob_tx.blob_gas();
            if sum_blob_gas_used + tx_blob_gas > MAX_DATA_GAS_PER_BLOCK {
                // we can't fit this _blob_ transaction into the block, so we mark it as
                // invalid, which removes its dependent transactions from
                // the iterator. This is similar to the gas limit condition
                // for regular transactions above.
                trace!(target: "payload_builder", tx=?tx.hash, ?sum_blob_gas_used, ?tx_blob_gas, "skipping blob transaction because it would exceed the max data gas per block");
                best_txs.mark_invalid(
                    &pool_tx,
                    InvalidPoolTransactionError::ExceedsGasLimit(
                        tx_blob_gas,
                        MAX_DATA_GAS_PER_BLOCK,
                    ),
                );
                continue;
            }
        }

        // Configure the environment for the tx.
        *evm.tx_mut() = evm_config.tx_env(tx.as_signed(), tx.signer());

        let ResultAndState { result, state } = match evm.transact() {
            Ok(res) => res,
            Err(err) => {
                match err {
                    EVMError::Transaction(err) => {
                        if matches!(err, InvalidTransaction::NonceTooLow { .. }) {
                            // if the nonce is too low, we can skip this transaction
                            trace!(target: "payload_builder", %err, ?tx, "skipping nonce too low transaction");
                        } else {
                            // if the transaction is invalid, we can skip it and all of its
                            // descendants
                            trace!(target: "payload_builder", %err, ?tx, "skipping invalid transaction and its descendants");
                            best_txs.mark_invalid(
                                &pool_tx,
                                InvalidPoolTransactionError::Consensus(
                                    InvalidTransactionError::TxTypeNotSupported,
                                ),
                            );
                        }

                        continue;
                    }
                    err => {
                        // this is an error that we should treat as fatal for this attempt
                        return Err(PayloadBuilderError::EvmExecutionError(err));
                    }
                }
            }
        };
        // commit changes
        evm.db_mut().commit(state);

        // add to the total blob gas used if the transaction successfully executed
        if let Some(blob_tx) = tx.transaction.as_eip4844() {
            let tx_blob_gas = blob_tx.blob_gas();
            sum_blob_gas_used += tx_blob_gas;

            // if we've reached the max data gas per block, we can skip blob txs entirely
            if sum_blob_gas_used == MAX_DATA_GAS_PER_BLOCK {
                best_txs.skip_blobs();
            }
        }

        let gas_used = result.gas_used();

        // add gas used by the transaction to cumulative gas used, before creating the receipt
        cumulative_gas_used += gas_used;

        // Push transaction changeset and calculate header bloom filter for receipt.
        #[allow(clippy::needless_update)] // side-effect of optimism fields
        receipts.push(Some(Receipt {
            tx_type: tx.tx_type(),
            success: result.is_success(),
            cumulative_gas_used,
            logs: result.into_logs().into_iter().map(Into::into).collect(),
            ..Default::default()
        }));

        // update add to total fees
        let miner_fee = tx
            .effective_tip_per_gas(Some(base_fee))
            .expect("fee is always valid; execution succeeded");
        total_fees += U256::from(miner_fee) * U256::from(gas_used);

        // append sender and transaction to the respective lists
        executed_senders.push(tx.signer());
        executed_txs.push(tx.into_signed());
    }

    // Release db
    drop(evm);

    // check if we have a better block
    if !is_better_payload(best_payload.as_ref(), total_fees) {
        // can skip building the block
        return Ok(BuildOutcome::Aborted {
            fees: total_fees,
            cached_reads,
        });
    }

    let env = EnvWithHandlerCfg::new_with_cfg_env(
        initialized_cfg.clone(),
        initialized_block_env.clone(),
        TxEnv::default(),
    );
    let mut evm = evm_config.evm_with_env(&mut db, env);

    // < GNOSIS SPECIFIC
    let balance_increments = apply_post_block_system_calls(
        &chain_spec,
        &evm_config,
        block_rewards_contract,
        attributes.timestamp,
        Some(&attributes.withdrawals),
        attributes.suggested_fee_recipient,
        &mut evm,
    )
    .map_err(|err| PayloadBuilderError::Internal(err.into()))?;
    // GNOSIS SPECIFIC >

    evm.context
        .evm
        .db
        .increment_balances(balance_increments)
        .map_err(|err| {
            warn!(target: "payload_builder",
                parent_hash=%parent_header.hash(),
                %err,
                "failed to increment balances for payload"
            );
            PayloadBuilderError::Internal(err.into())
        })?;

    drop(evm);

    // calculate the requests and the requests root
    let requests = if chain_spec.is_prague_active_at_timestamp(attributes.timestamp) {
        let deposit_requests = parse_deposits_from_receipts(&chain_spec, receipts.iter().flatten())
            .map_err(|err| PayloadBuilderError::Internal(RethError::Execution(err.into())))?;
        let withdrawal_requests = system_caller
            .post_block_withdrawal_requests_contract_call(
                &mut db,
                &initialized_cfg,
                &initialized_block_env,
            )
            .map_err(|err| PayloadBuilderError::Internal(err.into()))?;
        let consolidation_requests = system_caller
            .post_block_consolidation_requests_contract_call(
                &mut db,
                &initialized_cfg,
                &initialized_block_env,
            )
            .map_err(|err| PayloadBuilderError::Internal(err.into()))?;

        let mut requests = Requests::default();

        if !deposit_requests.is_empty() {
            requests.push_request(core::iter::once(0).chain(deposit_requests).collect());
        }

        if !withdrawal_requests.is_empty() {
            requests.push_request(
                core::iter::once(WITHDRAWAL_REQUEST_TYPE)
                    .chain(withdrawal_requests)
                    .collect(),
            );
        }

        if !consolidation_requests.is_empty() {
            requests.push_request(
                core::iter::once(CONSOLIDATION_REQUEST_TYPE)
                    .chain(consolidation_requests)
                    .collect(),
            );
        }

        Some(requests)
    } else {
        None
    };

    let withdrawals_root = commit_withdrawals(
        &mut db,
        &chain_spec,
        attributes.timestamp,
        &attributes.withdrawals,
    )?;

    // merge all transitions into bundle state, this would apply the withdrawal balance changes
    // and 4788 contract call
    db.merge_transitions(BundleRetention::Reverts);

    let requests_hash = requests.as_ref().map(|requests| requests.requests_hash());
    let execution_outcome = ExecutionOutcome::new(
        db.take_bundle(),
        vec![receipts].into(),
        block_number,
        vec![requests.clone().unwrap_or_default()],
    );
    let receipts_root = execution_outcome
        .receipts_root_slow(block_number)
        .expect("Number is in range");
    let logs_bloom = execution_outcome
        .block_logs_bloom(block_number)
        .expect("Number is in range");

    // calculate the state root
    let hashed_state = HashedPostState::from_bundle_state(&execution_outcome.state().state);
    let (state_root, trie_output) = {
        db.database
            .inner()
            .state_root_with_updates(hashed_state.clone())
            .inspect_err(|err| {
                warn!(target: "payload_builder",
                    parent_hash=%parent_header.hash(),
                    %err,
                    "failed to calculate state root for payload"
                );
            })?
    };

    // create the block header
    let transactions_root = proofs::calculate_transaction_root(&executed_txs);

    // initialize empty blob sidecars at first. If cancun is active then this will
    let mut blob_sidecars = Vec::new();
    let mut excess_blob_gas = None;
    let mut blob_gas_used = None;

    // only determine cancun fields when active
    if chain_spec.is_cancun_active_at_timestamp(attributes.timestamp) {
        // grab the blob sidecars from the executed txs
        blob_sidecars = pool
            .get_all_blobs_exact(
                executed_txs
                    .iter()
                    .filter(|tx| tx.is_eip4844())
                    .map(|tx| tx.hash())
                    .collect(),
            )
            .map_err(PayloadBuilderError::other)?;

        excess_blob_gas = if chain_spec.is_cancun_active_at_timestamp(parent_header.timestamp) {
            let parent_excess_blob_gas = parent_header.excess_blob_gas.unwrap_or_default();
            let parent_blob_gas_used = parent_header.blob_gas_used.unwrap_or_default();
            Some(calc_excess_blob_gas(
                parent_excess_blob_gas,
                parent_blob_gas_used,
            ))
        } else {
            // for the first post-fork block, both parent.blob_gas_used and
            // parent.excess_blob_gas are evaluated as 0
            Some(calc_excess_blob_gas(0, 0))
        };

        blob_gas_used = Some(sum_blob_gas_used);
    }

    let header = Header {
        parent_hash: parent_header.hash(),
        ommers_hash: EMPTY_OMMER_ROOT_HASH,
        beneficiary: initialized_block_env.coinbase,
        state_root,
        transactions_root,
        receipts_root,
        withdrawals_root,
        logs_bloom,
        timestamp: attributes.timestamp,
        mix_hash: attributes.prev_randao,
        nonce: BEACON_NONCE.into(),
        base_fee_per_gas: Some(base_fee),
        number: parent_header.number + 1,
        gas_limit: block_gas_limit,
        difficulty: U256::ZERO,
        gas_used: cumulative_gas_used,
        extra_data,
        parent_beacon_block_root: attributes.parent_beacon_block_root,
        blob_gas_used: blob_gas_used.map(Into::into),
        excess_blob_gas: excess_blob_gas.map(Into::into),
        requests_hash,
        target_blobs_per_block: None,
    };

    let withdrawals = chain_spec
        .is_shanghai_active_at_timestamp(attributes.timestamp)
        .then(|| attributes.withdrawals.clone());

    // seal the block
    let block = Block {
        header,
        body: BlockBody {
            transactions: executed_txs,
            ommers: vec![],
            withdrawals,
        },
    };

    let sealed_block = Arc::new(block.seal_slow());
    debug!(target: "payload_builder", id=%attributes.id, sealed_block_header = ?sealed_block.header, "sealed built block");

    // create the executed block data
    let executed = ExecutedBlock {
        block: sealed_block.clone(),
        senders: Arc::new(executed_senders),
        execution_output: Arc::new(execution_outcome),
        hashed_state: Arc::new(hashed_state),
        trie: Arc::new(trie_output),
    };

    let mut payload = EthBuiltPayload::new(
        attributes.id,
        sealed_block,
        total_fees,
        Some(executed),
        requests,
    );

    // extend the payload with the blob sidecars from the executed txs
    payload.extend_sidecars(blob_sidecars.into_iter().map(Arc::unwrap_or_clone));

    Ok(BuildOutcome::Better {
        payload,
        cached_reads,
    })
}
