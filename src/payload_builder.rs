use eyre::eyre;
use reth::{
    api::FullNodeTypes,
    builder::{components::PayloadServiceBuilder, BuilderContext, PayloadBuilderConfig},
    payload::{
        error::PayloadBuilderError, EthBuiltPayload, EthPayloadBuilderAttributes,
        PayloadBuilderHandle, PayloadBuilderService,
    },
    primitives::{
        constants::{
            eip4844::MAX_DATA_GAS_PER_BLOCK, BEACON_NONCE, EMPTY_RECEIPTS, EMPTY_TRANSACTIONS,
            EMPTY_WITHDRAWALS,
        },
        eip4844::calculate_excess_blob_gas,
        proofs::{self, calculate_requests_root},
        Block, Header, IntoRecoveredTransaction, Receipt, EMPTY_OMMER_ROOT_HASH,
    },
    revm::{database::StateProviderDatabase, state_change::apply_blockhashes_update},
    transaction_pool::{BestTransactionsAttributes, TransactionPool},
};
use reth_basic_payload_builder::{
    commit_withdrawals, is_better_payload, BasicPayloadJobGenerator,
    BasicPayloadJobGeneratorConfig, BuildArguments, BuildOutcome, PayloadBuilder, PayloadConfig,
    WithdrawalsOutcome,
};
use reth_chainspec::EthereumHardforks;
use reth_errors::RethError;
use reth_evm::{
    system_calls::{
        post_block_withdrawal_requests_contract_call, pre_block_beacon_root_contract_call,
    },
    ConfigureEvm,
};
use reth_evm_ethereum::eip6110::parse_deposits_from_receipts;
use reth_node_ethereum::EthEngineTypes;
use reth_provider::{CanonStateSubscriptions, ExecutionOutcome, StateProviderFactory};
use revm::{db::states::bundle_state::BundleRetention, DatabaseCommit, State};
use revm_primitives::{
    Address, EVMError, EnvWithHandlerCfg, InvalidTransaction, ResultAndState, U256,
};

use crate::{evm_config::GnosisEvmConfig, execute::gnosis_post_block_system_calls};

/// A basic Gnosis payload service builder
#[derive(Debug, Default, Clone)]
pub struct GnosisPayloadServiceBuilder<EVM = GnosisEvmConfig> {
    /// The EVM configuration to use for the payload builder.
    pub evm_config: EVM,
}

impl<EVM> GnosisPayloadServiceBuilder<EVM> {
    /// Create a new instance with the given evm config.
    pub const fn new(evm_config: EVM) -> Self {
        Self { evm_config }
    }
}

impl<Node, EVM, Pool> PayloadServiceBuilder<Node, Pool> for GnosisPayloadServiceBuilder<EVM>
where
    Node: FullNodeTypes<Engine = EthEngineTypes>,
    Pool: TransactionPool + Unpin + 'static,
    EVM: ConfigureEvm,
{
    async fn spawn_payload_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<PayloadBuilderHandle<Node::Engine>> {
        let chain_spec = ctx.chain_spec();
        let block_rewards_contract = chain_spec
            .genesis()
            .config
            .extra_fields
            .get("blockRewardsContract")
            .ok_or(eyre!("blockRewardsContract not defined"))?;
        let block_rewards_contract: Address =
            serde_json::from_value(block_rewards_contract.clone())?;

        let payload_builder = GnosisPayloadBuilder::new(self.evm_config, block_rewards_contract);
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
            ctx.chain_spec(),
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
pub struct GnosisPayloadBuilder<EvmConfig = GnosisEvmConfig> {
    /// The type responsible for creating the evm.
    evm_config: EvmConfig,
    /// AuRa BlockRewards contract address for its system call
    block_rewards_contract: Address,
}

impl<EvmConfig> GnosisPayloadBuilder<EvmConfig> {
    pub const fn new(evm_config: EvmConfig, block_rewards_contract: Address) -> Self {
        Self {
            evm_config,
            block_rewards_contract,
        }
    }
}

// Default implementation of [PayloadBuilder] for unit type
impl<EvmConfig, Pool, Client> PayloadBuilder<Pool, Client> for GnosisPayloadBuilder<EvmConfig>
where
    EvmConfig: ConfigureEvm,
    Client: StateProviderFactory,
    Pool: TransactionPool,
{
    type Attributes = EthPayloadBuilderAttributes;
    type BuiltPayload = EthBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Pool, Client, EthPayloadBuilderAttributes, EthBuiltPayload>,
    ) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError> {
        default_ethereum_payload_builder(self.evm_config.clone(), args, self.block_rewards_contract)
    }

    fn build_empty_payload(
        &self,
        client: &Client,
        config: PayloadConfig<Self::Attributes>,
    ) -> Result<EthBuiltPayload, PayloadBuilderError> {
        let extra_data = config.extra_data();
        let PayloadConfig {
            initialized_block_env,
            parent_block,
            attributes,
            chain_spec,
            initialized_cfg,
            ..
        } = config;

        let state = client.state_by_block_hash(parent_block.hash())?;
        let mut db = State::builder()
            .with_database(StateProviderDatabase::new(state))
            .with_bundle_update()
            .build();

        let base_fee = initialized_block_env.basefee.to::<u64>();
        let block_number = initialized_block_env.number.to::<u64>();
        let block_gas_limit = initialized_block_env
            .gas_limit
            .try_into()
            .unwrap_or(u64::MAX);

        // apply eip-4788 pre block contract call
        pre_block_beacon_root_contract_call(
            &mut db,
            &self.evm_config,
            &chain_spec,
            &initialized_cfg,
            &initialized_block_env,
            block_number,
            attributes.timestamp,
            attributes.parent_beacon_block_root,
        )
        .map_err(|err| PayloadBuilderError::Internal(err.into()))?;

        // apply eip-2935 blockhashes update
        apply_blockhashes_update(
            &mut db,
            &chain_spec,
            initialized_block_env.timestamp.to::<u64>(),
            block_number,
            parent_block.hash(),
        )
        .map_err(|err| PayloadBuilderError::Internal(err.into()))?;

        let WithdrawalsOutcome {
            withdrawals_root,
            withdrawals,
        } = commit_withdrawals(
            &mut db,
            &chain_spec,
            attributes.timestamp,
            attributes.withdrawals.clone(),
        )?;

        // merge all transitions into bundle state, this would apply the withdrawal balance
        // changes and 4788 contract call
        db.merge_transitions(BundleRetention::PlainState);

        // calculate the state root
        let bundle_state = db.take_bundle();
        let state_root = db.database.state_root(&bundle_state)?;

        let mut excess_blob_gas = None;
        let mut blob_gas_used = None;

        if chain_spec.is_cancun_active_at_timestamp(attributes.timestamp) {
            excess_blob_gas = if chain_spec.is_cancun_active_at_timestamp(parent_block.timestamp) {
                let parent_excess_blob_gas = parent_block.excess_blob_gas.unwrap_or_default();
                let parent_blob_gas_used = parent_block.blob_gas_used.unwrap_or_default();
                Some(calculate_excess_blob_gas(
                    parent_excess_blob_gas,
                    parent_blob_gas_used,
                ))
            } else {
                // for the first post-fork block, both parent.blob_gas_used and
                // parent.excess_blob_gas are evaluated as 0
                Some(calculate_excess_blob_gas(0, 0))
            };

            blob_gas_used = Some(0);
        }

        // Calculate the requests and the requests root.
        let (requests, requests_root) = if chain_spec
            .is_prague_active_at_timestamp(attributes.timestamp)
        {
            // We do not calculate the EIP-6110 deposit requests because there are no
            // transactions in an empty payload.
            let withdrawal_requests = post_block_withdrawal_requests_contract_call::<EvmConfig, _>(
                &self.evm_config,
                &mut db,
                &initialized_cfg,
                &initialized_block_env,
            )
            .map_err(|err| PayloadBuilderError::Internal(err.into()))?;

            let requests = withdrawal_requests;
            let requests_root = calculate_requests_root(&requests);
            (Some(requests.into()), Some(requests_root))
        } else {
            (None, None)
        };

        let header = Header {
            parent_hash: parent_block.hash(),
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            beneficiary: initialized_block_env.coinbase,
            state_root,
            transactions_root: EMPTY_TRANSACTIONS,
            withdrawals_root,
            receipts_root: EMPTY_RECEIPTS,
            logs_bloom: Default::default(),
            timestamp: attributes.timestamp,
            mix_hash: attributes.prev_randao,
            nonce: BEACON_NONCE,
            base_fee_per_gas: Some(base_fee),
            number: parent_block.number + 1,
            gas_limit: block_gas_limit,
            difficulty: U256::ZERO,
            gas_used: 0,
            extra_data,
            blob_gas_used,
            excess_blob_gas,
            parent_beacon_block_root: attributes.parent_beacon_block_root,
            requests_root,
        };

        let block = Block {
            header,
            body: vec![],
            ommers: vec![],
            withdrawals,
            requests,
        };
        let sealed_block = block.seal_slow();

        Ok(EthBuiltPayload::new(
            attributes.payload_id(),
            sealed_block,
            U256::ZERO,
        ))
    }
}

/// Constructs an Ethereum transaction payload using the best transactions from the pool.
///
/// Given build arguments including an Ethereum client, transaction pool,
/// and configuration, this function creates a transaction payload. Returns
/// a result indicating success with the payload or an error in case of failure.
#[inline]
pub fn default_ethereum_payload_builder<EvmConfig, Pool, Client>(
    evm_config: EvmConfig,
    args: BuildArguments<Pool, Client, EthPayloadBuilderAttributes, EthBuiltPayload>,
    block_rewards_contract: Address,
) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError>
where
    EvmConfig: ConfigureEvm,
    Client: StateProviderFactory,
    Pool: TransactionPool,
{
    let BuildArguments {
        client,
        pool,
        mut cached_reads,
        config,
        cancel,
        best_payload,
    } = args;

    let state_provider = client.state_by_block_hash(config.parent_block.hash())?;
    let state = StateProviderDatabase::new(state_provider);
    let mut db = State::builder()
        .with_database_ref(cached_reads.as_db(state))
        .with_bundle_update()
        .build();
    let extra_data = config.extra_data();
    let PayloadConfig {
        initialized_block_env,
        initialized_cfg,
        parent_block,
        attributes,
        chain_spec,
        ..
    } = config;

    let mut cumulative_gas_used = 0;
    let mut sum_blob_gas_used = 0;
    let block_gas_limit: u64 = initialized_block_env
        .gas_limit
        .try_into()
        .unwrap_or(u64::MAX);
    let base_fee = initialized_block_env.basefee.to::<u64>();

    let mut executed_txs = Vec::new();

    let mut best_txs = pool.best_transactions_with_attributes(BestTransactionsAttributes::new(
        base_fee,
        initialized_block_env
            .get_blob_gasprice()
            .map(|gasprice| gasprice as u64),
    ));

    let mut total_fees = U256::ZERO;

    let block_number = initialized_block_env.number.to::<u64>();

    // apply eip-4788 pre block contract call
    pre_block_beacon_root_contract_call(
        &mut db,
        &evm_config,
        &chain_spec,
        &initialized_cfg,
        &initialized_block_env,
        block_number,
        attributes.timestamp,
        attributes.parent_beacon_block_root,
    )
    .map_err(|err| PayloadBuilderError::Internal(err.into()))?;

    // apply eip-2935 blockhashes update
    apply_blockhashes_update(
        &mut db,
        &chain_spec,
        initialized_block_env.timestamp.to::<u64>(),
        block_number,
        parent_block.hash(),
    )
    .map_err(|err| PayloadBuilderError::Internal(err.into()))?;

    let mut receipts = Vec::new();
    while let Some(pool_tx) = best_txs.next() {
        // ensure we still have capacity for this transaction
        if cumulative_gas_used + pool_tx.gas_limit() > block_gas_limit {
            // we can't fit this transaction into the block, so we need to mark it as invalid
            // which also removes all dependent transaction from the iterator before we can
            // continue
            best_txs.mark_invalid(&pool_tx);
            continue;
        }

        // check if the job was cancelled, if so we can exit early
        if cancel.is_cancelled() {
            return Ok(BuildOutcome::Cancelled);
        }

        // convert tx to a signed transaction
        let tx = pool_tx.to_recovered_transaction();

        // There's only limited amount of blob space available per block, so we need to check if
        // the EIP-4844 can still fit in the block
        if let Some(blob_tx) = tx.transaction.as_eip4844() {
            let tx_blob_gas = blob_tx.blob_gas();
            if sum_blob_gas_used + tx_blob_gas > MAX_DATA_GAS_PER_BLOCK {
                // we can't fit this _blob_ transaction into the block, so we mark it as
                // invalid, which removes its dependent transactions from
                // the iterator. This is similar to the gas limit condition
                // for regular transactions above.
                best_txs.mark_invalid(&pool_tx);
                continue;
            }
        }

        let env = EnvWithHandlerCfg::new_with_cfg_env(
            initialized_cfg.clone(),
            initialized_block_env.clone(),
            evm_config.tx_env(&tx),
        );

        // Configure the environment for the block.
        let mut evm = evm_config.evm_with_env(&mut db, env);

        let ResultAndState { result, state } = match evm.transact() {
            Ok(res) => res,
            Err(err) => {
                match err {
                    EVMError::Transaction(err) => {
                        if matches!(err, InvalidTransaction::NonceTooLow { .. }) {
                            // if the nonce is too low, we can skip this transaction
                        } else {
                            // if the transaction is invalid, we can skip it and all of its
                            // descendants
                            best_txs.mark_invalid(&pool_tx);
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
        // drop evm so db is released.
        drop(evm);
        // commit changes
        db.commit(state);

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

        // append transaction to the list of executed transactions
        executed_txs.push(tx.into_signed());
    }

    // check if we have a better block
    if !is_better_payload(best_payload.as_ref(), total_fees) {
        // can skip building the block
        return Ok(BuildOutcome::Aborted {
            fees: total_fees,
            cached_reads,
        });
    }

    // calculate the requests and the requests root
    let (requests, requests_root) = if chain_spec
        .is_prague_active_at_timestamp(attributes.timestamp)
    {
        let deposit_requests = parse_deposits_from_receipts(&chain_spec, receipts.iter().flatten())
            .map_err(|err| PayloadBuilderError::Internal(RethError::Execution(err.into())))?;
        let withdrawal_requests = post_block_withdrawal_requests_contract_call(
            &evm_config,
            &mut db,
            &initialized_cfg,
            &initialized_block_env,
        )
        .map_err(|err| PayloadBuilderError::Internal(err.into()))?;

        let requests = [deposit_requests, withdrawal_requests].concat();
        let requests_root = calculate_requests_root(&requests);
        (Some(requests.into()), Some(requests_root))
    } else {
        (None, None)
    };

    // Compute the withdrawals root independent of how they are applied
    let withdrawals_root = if !chain_spec.is_shanghai_active_at_timestamp(attributes.timestamp) {
        None
    } else if attributes.withdrawals.is_empty() {
        Some(EMPTY_WITHDRAWALS)
    } else {
        Some(proofs::calculate_withdrawals_root(&attributes.withdrawals))
    };

    gnosis_post_block_system_calls(
        &chain_spec,
        &evm_config,
        &mut db,
        &initialized_cfg,
        &initialized_block_env,
        block_rewards_contract,
        attributes.timestamp,
        Some(&attributes.withdrawals),
        attributes.suggested_fee_recipient,
    )
    .map_err(|err| PayloadBuilderError::Internal(err.into()))?;

    let WithdrawalsOutcome { withdrawals, .. } = commit_withdrawals(
        &mut db,
        &chain_spec,
        attributes.timestamp,
        attributes.withdrawals,
    )?;

    // merge all transitions into bundle state, this would apply the withdrawal balance changes
    // and 4788 contract call
    db.merge_transitions(BundleRetention::PlainState);

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
    let state_root = {
        let state_provider = db.database.0.inner.borrow_mut();
        state_provider.db.state_root(execution_outcome.state())?
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
        blob_sidecars = pool.get_all_blobs_exact(
            executed_txs
                .iter()
                .filter(|tx| tx.is_eip4844())
                .map(|tx| tx.hash)
                .collect(),
        )?;

        excess_blob_gas = if chain_spec.is_cancun_active_at_timestamp(parent_block.timestamp) {
            let parent_excess_blob_gas = parent_block.excess_blob_gas.unwrap_or_default();
            let parent_blob_gas_used = parent_block.blob_gas_used.unwrap_or_default();
            Some(calculate_excess_blob_gas(
                parent_excess_blob_gas,
                parent_blob_gas_used,
            ))
        } else {
            // for the first post-fork block, both parent.blob_gas_used and
            // parent.excess_blob_gas are evaluated as 0
            Some(calculate_excess_blob_gas(0, 0))
        };

        blob_gas_used = Some(sum_blob_gas_used);
    }

    let header = Header {
        parent_hash: parent_block.hash(),
        ommers_hash: EMPTY_OMMER_ROOT_HASH,
        beneficiary: initialized_block_env.coinbase,
        state_root,
        transactions_root,
        receipts_root,
        withdrawals_root,
        logs_bloom,
        timestamp: attributes.timestamp,
        mix_hash: attributes.prev_randao,
        nonce: BEACON_NONCE,
        base_fee_per_gas: Some(base_fee),
        number: parent_block.number + 1,
        gas_limit: block_gas_limit,
        difficulty: U256::ZERO,
        gas_used: cumulative_gas_used,
        extra_data,
        parent_beacon_block_root: attributes.parent_beacon_block_root,
        blob_gas_used,
        excess_blob_gas,
        requests_root,
    };

    // seal the block
    let block = Block {
        header,
        body: executed_txs,
        ommers: vec![],
        withdrawals,
        requests,
    };

    let sealed_block = block.seal_slow();

    let mut payload = EthBuiltPayload::new(attributes.id, sealed_block, total_fees);

    // extend the payload with the blob sidecars from the executed txs
    payload.extend_sidecars(blob_sidecars);

    Ok(BuildOutcome::Better {
        payload,
        cached_reads,
    })
}
