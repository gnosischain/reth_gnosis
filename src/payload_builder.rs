use std::sync::Arc;

use alloy_consensus::{
    proofs::calculate_withdrawals_root, Header, Transaction, Typed2718, EMPTY_OMMER_ROOT_HASH,
};
use alloy_eips::{
    eip4844::MAX_DATA_GAS_PER_BLOCK, eip6110, eip7685::Requests, merge::BEACON_NONCE,
};
use eyre::eyre;
use reth::{
    api::{FullNodeTypes, NodeTypesWithEngine},
    builder::{components::PayloadServiceBuilder, BuilderContext, PayloadBuilderConfig},
    payload::{
        EthBuiltPayload, EthPayloadBuilderAttributes, PayloadBuilderAttributes,
        PayloadBuilderError, PayloadBuilderHandle, PayloadBuilderService,
    },
    revm::database::StateProviderDatabase,
    transaction_pool::{
        error::InvalidPoolTransactionError, noop::NoopTransactionPool, BestTransactions,
        BestTransactionsAttributes, PoolTransaction, TransactionPool, ValidPoolTransaction,
    },
};
use reth_basic_payload_builder::{
    is_better_payload, BasicPayloadJobGenerator, BasicPayloadJobGeneratorConfig, BuildArguments,
    BuildOutcome, PayloadBuilder, PayloadConfig,
};
use reth_chainspec::EthereumHardforks;
use reth_errors::RethError;
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_evm::{
    system_calls::SystemCaller, ConfigureEvm, ConfigureEvmEnv, Evm, EvmEnv, NextBlockEnvAttributes,
};
use reth_evm_ethereum::eip6110::parse_deposits_from_receipts;
use reth_node_ethereum::EthEngineTypes;
use reth_primitives::{
    Block, BlockBody, EthPrimitives, InvalidTransactionError, Receipt, TransactionSigned,
};
use reth_primitives_traits::{proofs, Block as _, SignedTransaction};
use reth_provider::{
    CanonStateSubscriptions, ChainSpecProvider, ExecutionOutcome, StateProviderFactory,
};
use revm::{
    db::{states::bundle_state::BundleRetention, State},
    DatabaseCommit,
};
use revm_primitives::{Address, EVMError, InvalidTransaction, ResultAndState, U256};
use tracing::{debug, trace, warn};

use crate::{
    blobs::{CANCUN_BLOB_PARAMS, PRAGUE_BLOB_PARAMS},
    evm_config::GnosisEvmConfig,
    gnosis::apply_post_block_system_calls,
    spec::GnosisChainSpec,
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

        let conf = ctx.payload_builder_config();
        let gas_limit = chain_spec.genesis.gas_limit;
        let payload_builder = GnosisPayloadBuilder::new(
            GnosisEvmConfig::new(collector_address, chain_spec),
            EthereumBuilderConfig::new(conf.extra_data_bytes()).with_gas_limit(gas_limit),
            block_rewards_contract,
        );

        let payload_job_config = BasicPayloadJobGeneratorConfig::default()
            .interval(conf.interval())
            .deadline(conf.deadline())
            .max_payload_tasks(conf.max_payload_tasks());

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GnosisPayloadBuilder<GnosisEvmConfig> {
    /// The type responsible for creating the evm.
    evm_config: GnosisEvmConfig,
    /// AuRa BlockRewards contract address for its system call
    block_rewards_contract: Address,
    /// Payload builder configuration.
    builder_config: EthereumBuilderConfig,
}

impl<EvmConfig> GnosisPayloadBuilder<EvmConfig>
where
    EvmConfig: ConfigureEvmEnv<Header = Header>,
{
    pub const fn new(
        evm_config: EvmConfig,
        builder_config: EthereumBuilderConfig,
        block_rewards_contract: Address,
    ) -> Self {
        Self {
            evm_config,
            builder_config,
            block_rewards_contract,
        }
    }
}

impl<EvmConfig> GnosisPayloadBuilder<EvmConfig>
where
    EvmConfig: ConfigureEvmEnv<Header = Header>,
{
    /// Returns the configured [`EvmEnv`] for the targeted payload
    /// (that has the `parent` as its parent).
    fn evm_env(
        &self,
        config: &PayloadConfig<EthPayloadBuilderAttributes>,
        parent: &Header,
    ) -> Result<EvmEnv<EvmConfig::Spec>, EvmConfig::Error> {
        let next_attributes = NextBlockEnvAttributes {
            timestamp: config.attributes.timestamp(),
            suggested_fee_recipient: config.attributes.suggested_fee_recipient(),
            prev_randao: config.attributes.prev_randao(),
            gas_limit: self.builder_config.gas_limit(parent.gas_limit),
        };
        self.evm_config.next_evm_env(parent, next_attributes)
    }
}

// Default implementation of [PayloadBuilder] for unit type
impl<EvmConfig, Pool, Client> PayloadBuilder<Pool, Client> for GnosisPayloadBuilder<EvmConfig>
where
    EvmConfig: ConfigureEvm<Header = Header, Transaction = TransactionSigned>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = GnosisChainSpec>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
{
    type Attributes = EthPayloadBuilderAttributes;
    type BuiltPayload = EthBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Pool, Client, EthPayloadBuilderAttributes, EthBuiltPayload>,
    ) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError> {
        let evm_env = self
            .evm_env(&args.config, &args.config.parent_header)
            .map_err(PayloadBuilderError::other)?;
        let pool = args.pool.clone();
        default_ethereum_payload(
            self.evm_config.clone(),
            self.builder_config.clone(),
            args,
            evm_env,
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

        let evm_env = self
            .evm_env(&args.config, &args.config.parent_header)
            .map_err(PayloadBuilderError::other)?;

        let pool = args.pool.clone();

        default_ethereum_payload(
            self.evm_config.clone(),
            self.builder_config.clone(),
            args,
            evm_env,
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
    builder_config: EthereumBuilderConfig,
    args: BuildArguments<Pool, Client, EthPayloadBuilderAttributes, EthBuiltPayload>,
    evm_env: EvmEnv<EvmConfig::Spec>,
    block_rewards_contract: Address,
    best_txs: F,
) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError>
where
    EvmConfig: ConfigureEvm<Header = Header, Transaction = TransactionSigned>,
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
        attributes,
    } = config;

    debug!(target: "payload_builder", id=%attributes.id, parent_header = ?parent_header.hash(), parent_number = parent_header.number, "building new payload");
    let mut cumulative_gas_used = 0;
    let mut sum_blob_gas_used = 0;
    let block_gas_limit: u64 = evm_env.block_env.gas_limit.to::<u64>();
    let base_fee = evm_env.block_env.basefee.to::<u64>();

    let mut executed_txs = Vec::new();

    let mut best_txs = best_txs(BestTransactionsAttributes::new(
        base_fee,
        evm_env
            .block_env
            .get_blob_gasprice()
            .map(|gasprice| gasprice as u64),
    ));
    let mut total_fees = U256::ZERO;

    let block_number = evm_env.block_env.number.to::<u64>();
    let beneficiary = evm_env.block_env.coinbase;

    let mut system_caller = SystemCaller::new(evm_config.clone(), chain_spec.clone());

    let mut evm = evm_config.evm_with_env(&mut db, evm_env);

    system_caller
        .apply_pre_execution_changes(&parent_header, &mut evm)
        .map_err(|err| {
            warn!(target: "payload_builder",
                parent_hash=%parent_header.hash(),
                %err,
                "failed to apply pre-execution changes for payload"
            );
            PayloadBuilderError::Internal(err.into())
        })?;

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
        if let Some(blob_tx) = tx.as_eip4844() {
            let tx_blob_gas = blob_tx.blob_gas();
            if sum_blob_gas_used + tx_blob_gas > MAX_DATA_GAS_PER_BLOCK {
                // we can't fit this _blob_ transaction into the block, so we mark it as
                // invalid, which removes its dependent transactions from
                // the iterator. This is similar to the gas limit condition
                // for regular transactions above.
                trace!(target: "payload_builder", tx=?tx.hash(), ?sum_blob_gas_used, ?tx_blob_gas, "skipping blob transaction because it would exceed the max data gas per block");
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
        let tx_env = evm_config.tx_env(tx.tx(), tx.signer());

        let ResultAndState { result, state } = match evm.transact(tx_env) {
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
        if let Some(blob_tx) = tx.as_eip4844() {
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
        receipts.push(Receipt {
            tx_type: tx.tx_type(),
            success: result.is_success(),
            cumulative_gas_used,
            logs: result.into_logs().into_iter().collect(),
            ..Default::default()
        });

        // update add to total fees
        let miner_fee = tx
            .effective_tip_per_gas(base_fee)
            .expect("fee is always valid; execution succeeded");
        total_fees += U256::from(miner_fee) * U256::from(gas_used);

        // append transaction to the block body
        executed_txs.push(tx.into_tx());
    }

    // check if we have a better block
    if !is_better_payload(best_payload.as_ref(), total_fees) {
        // Release db
        drop(evm);

        // can skip building the block
        return Ok(BuildOutcome::Aborted {
            fees: total_fees,
            cached_reads,
        });
    }

    // let mut evm = evm_config.evm_with_env(&mut db, env);

    // < GNOSIS SPECIFIC
    let (balance_increments, _requests) = apply_post_block_system_calls(
        &chain_spec,
        // &evm_config,
        block_rewards_contract,
        attributes.timestamp,
        Some(&attributes.withdrawals),
        attributes.suggested_fee_recipient,
        &mut evm,
    )
    .map_err(|err| PayloadBuilderError::Internal(err.into()))?;
    // GNOSIS SPECIFIC >

    evm.db_mut()
        .increment_balances(balance_increments)
        .map_err(|err| {
            warn!(target: "payload_builder",
                parent_hash=%parent_header.hash(),
                %err,
                "failed to increment balances for payload"
            );
            PayloadBuilderError::Internal(err.into())
        })?;

    // calculate the requests and the requests root
    let requests = if chain_spec.is_prague_active_at_timestamp(attributes.timestamp) {
        let deposit_requests = parse_deposits_from_receipts(&chain_spec, receipts.iter())
            .map_err(|err| PayloadBuilderError::Internal(RethError::Execution(err.into())))?;

        let mut requests = Requests::default();

        if !deposit_requests.is_empty() {
            requests.push_request_with_type(eip6110::DEPOSIT_REQUEST_TYPE, deposit_requests);
        }

        requests.extend(
            system_caller
                .apply_post_execution_changes(&mut evm)
                .map_err(|err| PayloadBuilderError::Internal(err.into()))?,
        );

        Some(requests)
    } else {
        None
    };

    // Release db
    drop(evm);

    let withdrawals_root = Some(calculate_withdrawals_root(&attributes.withdrawals));

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
        .ethereum_receipts_root(block_number)
        .expect("Number is in range");
    let logs_bloom = execution_outcome
        .block_logs_bloom(block_number)
        .expect("Number is in range");

    // calculate the state root
    let hashed_state = db.database.db.hashed_post_state(execution_outcome.state());
    let (state_root, _) = {
        db.database
            .inner()
            .state_root_with_updates(hashed_state)
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
                    .map(|tx| *tx.tx_hash())
                    .collect(),
            )
            .map_err(PayloadBuilderError::other)?;

        excess_blob_gas = if chain_spec.is_cancun_active_at_timestamp(parent_header.timestamp) {
            let blob_params = if chain_spec.is_prague_active_at_timestamp(attributes.timestamp) {
                PRAGUE_BLOB_PARAMS
            } else {
                // cancun
                CANCUN_BLOB_PARAMS
            };
            parent_header.next_block_excess_blob_gas(blob_params)
        } else {
            // for the first post-fork block, both parent.blob_gas_used and
            // parent.excess_blob_gas are evaluated as 0
            Some(alloy_eips::eip4844::calc_excess_blob_gas(0, 0))
        };

        blob_gas_used = Some(sum_blob_gas_used);
    }

    let header = Header {
        parent_hash: parent_header.hash(),
        ommers_hash: EMPTY_OMMER_ROOT_HASH,
        beneficiary,
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
        extra_data: builder_config.extra_data,
        parent_beacon_block_root: attributes.parent_beacon_block_root,
        blob_gas_used,
        excess_blob_gas,
        requests_hash,
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
    debug!(target: "payload_builder", id=%attributes.id, sealed_block_header = ?sealed_block.sealed_header(), "sealed built block");

    let mut payload = EthBuiltPayload::new(attributes.id, sealed_block, total_fees, requests);

    // extend the payload with the blob sidecars from the executed txs
    payload.extend_sidecars(blob_sidecars.into_iter().map(Arc::unwrap_or_clone));

    Ok(BuildOutcome::Better {
        payload,
        cached_reads,
    })
}
