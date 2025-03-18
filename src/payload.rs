use std::sync::Arc;

use alloy_consensus::Transaction;
use alloy_eips::{eip4844::DATA_GAS_PER_BLOB, Typed2718};
use reth_basic_payload_builder::{
    is_better_payload, BuildArguments, BuildOutcome, PayloadBuilder, PayloadConfig,
};
use reth_chainspec::EthereumHardforks;
use reth_errors::{BlockExecutionError, BlockValidationError};
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_ethereum_primitives::{EthPrimitives, TransactionSigned};
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome},
    ConfigureEvm, Evm, NextBlockEnvAttributes,
};
use reth_node_builder::{PayloadBuilderAttributes, PayloadBuilderError};
use reth_payload_builder::{EthBuiltPayload, EthPayloadBuilderAttributes};
use reth_primitives_traits::{transaction::error::InvalidTransactionError, SignedTransaction};
use reth_provider::{ChainSpecProvider, StateProviderFactory};
use reth_revm::{database::StateProviderDatabase, db::State};
use reth_transaction_pool::{
    error::{Eip4844PoolTransactionError, InvalidPoolTransactionError}, BestTransactions, BestTransactionsAttributes,
    PoolTransaction, TransactionPool, ValidPoolTransaction,
};
use revm::context::Block;
use revm_primitives::{Address, U256};
use tracing::{debug, trace, warn};

use crate::{
    blobs::get_blob_params,
    spec::GnosisChainSpec,
};

type BestTransactionsIter<Pool> = Box<
    dyn BestTransactions<Item = Arc<ValidPoolTransaction<<Pool as TransactionPool>::Transaction>>>,
>;

/// Gnosis payload builder
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GnosisPayloadBuilder<Pool, Client, GnosisEvmConfig> {
    /// Client providing access to node state.
    client: Client,
    /// Transaction pool.
    pool: Pool,
    /// The type responsible for creating the evm.
    evm_config: GnosisEvmConfig,
    /// AuRa BlockRewards contract address for its system call
    block_rewards_contract: Address,
    /// Payload builder configuration.
    builder_config: EthereumBuilderConfig,
    /// EIP-1559 and EIP-4844 collector address
    fee_collector_contract: Address,
}

impl<Pool, Client, EvmConfig> GnosisPayloadBuilder<Pool, Client, EvmConfig> {
    pub const fn new(
        client: Client,
        pool: Pool,
        evm_config: EvmConfig,
        block_rewards_contract: Address,
        fee_collector_contract: Address,
        builder_config: EthereumBuilderConfig,
    ) -> Self {
        Self {
            client,
            pool,
            evm_config,
            block_rewards_contract,
            fee_collector_contract,
            builder_config,
        }
    }
}

// impl<Pool, Client, EvmConfig> GnosisPayloadBuilder<Pool, Client, EvmConfig>
// where
//     EvmConfig: ConfigureEvmEnv<Header = Header>,
// {
//     /// Returns the configured [`EvmEnv`] for the targeted payload
//     /// (that has the `parent` as its parent).
//     fn evm_env(
//         &self,
//         config: &PayloadConfig<EthPayloadBuilderAttributes>,
//         parent: &Header,
//     ) -> Result<EvmEnv<EvmConfig::Spec>, EvmConfig::Error> {
//         let next_attributes = NextBlockEnvAttributes {
//             timestamp: config.attributes.timestamp(),
//             suggested_fee_recipient: config.attributes.suggested_fee_recipient(),
//             prev_randao: config.attributes.prev_randao(),
//             gas_limit: self.builder_config.gas_limit(parent.gas_limit),
//         };
//         self.evm_config.next_evm_env(parent, next_attributes)
//     }
// }

// Default implementation of [PayloadBuilder] for unit type
impl<Pool, Client, EvmConfig> PayloadBuilder for GnosisPayloadBuilder<Pool, Client, EvmConfig>
where
    EvmConfig: ConfigureEvm<Primitives = EthPrimitives, NextBlockEnvCtx = NextBlockEnvAttributes>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = GnosisChainSpec> + Clone,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
{
    type Attributes = EthPayloadBuilderAttributes;
    type BuiltPayload = EthBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<EthPayloadBuilderAttributes, EthBuiltPayload>,
    ) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError> {
        // let evm_env = self
        //     .evm_env(&args.config, &args.config.parent_header)
        //     .map_err(PayloadBuilderError::other)?;

        default_ethereum_payload(
            self.evm_config.clone(),
            self.client.clone(),
            self.pool.clone(),
            self.builder_config.clone(),
            args,
            // evm_env,
            self.block_rewards_contract,
            self.fee_collector_contract,
            |attributes| self.pool.best_transactions_with_attributes(attributes),
        )
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes>,
    ) -> Result<EthBuiltPayload, PayloadBuilderError> {
        let args = BuildArguments::new(Default::default(), config, Default::default(), None);

        // let evm_env = self
        //     .evm_env(&args.config, &args.config.parent_header)
        //     .map_err(PayloadBuilderError::other)?;

        default_ethereum_payload(
            self.evm_config.clone(),
            self.client.clone(),
            self.pool.clone(),
            self.builder_config.clone(),
            args,
            // evm_env,
            self.block_rewards_contract,
            self.fee_collector_contract,
            |attributes| self.pool.best_transactions_with_attributes(attributes),
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
#[allow(clippy::too_many_arguments)]
pub fn default_ethereum_payload<EvmConfig, Pool, Client, F>(
    evm_config: EvmConfig,
    client: Client,
    pool: Pool,
    builder_config: EthereumBuilderConfig,
    args: BuildArguments<EthPayloadBuilderAttributes, EthBuiltPayload>,
    // evm_env: EvmEnv<EvmConfig::Spec>,
    block_rewards_contract: Address,
    fee_collector_contract: Address,
    best_txs: F,
) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError>
where
    EvmConfig: ConfigureEvm<Primitives = EthPrimitives, NextBlockEnvCtx = NextBlockEnvAttributes>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = GnosisChainSpec>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
    F: FnOnce(BestTransactionsAttributes) -> BestTransactionsIter<Pool>,
{
    let BuildArguments { mut cached_reads, config, cancel, best_payload } = args;
    let PayloadConfig { parent_header, attributes } = config;

    let state_provider = client.state_by_block_hash(parent_header.hash())?;
    let state = StateProviderDatabase::new(&state_provider);
    let mut db =
        State::builder().with_database(cached_reads.as_db_mut(state)).with_bundle_update().build();

    let mut builder = evm_config
        .builder_for_next_block(
            &mut db,
            &parent_header,
            NextBlockEnvAttributes {
                timestamp: attributes.timestamp(),
                suggested_fee_recipient: attributes.suggested_fee_recipient(),
                prev_randao: attributes.prev_randao(),
                gas_limit: builder_config.gas_limit(parent_header.gas_limit),
                parent_beacon_block_root: attributes.parent_beacon_block_root(),
                withdrawals: Some(attributes.withdrawals().clone()),
            },
        )
        .map_err(PayloadBuilderError::other)?;

    let chain_spec = client.chain_spec();

    debug!(target: "payload_builder", id=%attributes.id, parent_header = ?parent_header.hash(), parent_number = parent_header.number, "building new payload");
    let mut cumulative_gas_used = 0;
    let block_gas_limit: u64 = builder.evm_mut().block().gas_limit;
    let base_fee = builder.evm_mut().block().basefee;

    // builder.evm_mut() replaces evm_env

    // let mut executed_txs = Vec::new();

    let mut best_txs = best_txs(BestTransactionsAttributes::new(
        base_fee,
        builder.evm_mut().block().blob_gasprice().map(|gasprice| gasprice as u64),
    ));
    let mut total_fees = U256::ZERO;

    // let block_number = evm_env.block_env.number.to::<u64>();
    // let beneficiary = evm_env.block_env.coinbase;

    // let mut system_caller = SystemCaller::new(chain_spec.clone());

    // // apply eip-2935 blockhashes update
    // system_caller.pre_block_blockhashes_contract_call(
    //     parent_header.hash(),
    //     builder.evm_mut(),
    // )
    // .map_err(|err| {
    //     warn!(target: "payload_builder", parent_hash=%parent_header.hash(), %err, "failed to update parent header blockhashes for payload");
    //     PayloadBuilderError::Internal(err.into())
    // })?;

    // // apply eip-4788 pre block contract call
    // system_caller
    //     .pre_block_beacon_root_contract_call(attributes.parent_beacon_block_root, builder.evm_mut())
    //     .map_err(|err| {
    //         warn!(target: "payload_builder",
    //             parent_hash=%parent_header.hash(),
    //             %err,
    //             "failed to apply beacon root contract call for payload"
    //         );
    //         PayloadBuilderError::Internal(err.into())
    //     })?;

    builder.apply_pre_execution_changes().map_err(|err| {
        warn!(target: "payload_builder", %err, "failed to apply pre-execution changes");
        PayloadBuilderError::Internal(err.into())
    })?;

    // let mut evm = evm_config.evm_with_env(&mut db, evm_env);

    // let mut receipts = Vec::new();
    let mut block_blob_count = 0;
    let blob_params = Some(get_blob_params(
        chain_spec.is_prague_active_at_timestamp(attributes.timestamp),
    ));
    let max_blob_count = blob_params
        .as_ref()
        .map(|params| params.max_blob_count)
        .unwrap_or_default();

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
            continue
        }

        // check if the job was cancelled, if so we can exit early
        if cancel.is_cancelled() {
            return Ok(BuildOutcome::Cancelled)
        }

        // convert tx to a signed transaction
        let tx = pool_tx.to_consensus();

        // There's only limited amount of blob space available per block, so we need to check if
        // the EIP-4844 can still fit in the block
        if let Some(blob_tx) = tx.as_eip4844() {
            let tx_blob_count = blob_tx.blob_versioned_hashes.len() as u64;

            if block_blob_count + tx_blob_count > max_blob_count {
                // we can't fit this _blob_ transaction into the block, so we mark it as
                // invalid, which removes its dependent transactions from
                // the iterator. This is similar to the gas limit condition
                // for regular transactions above.
                trace!(target: "payload_builder", tx=?tx.hash(), ?block_blob_count, "skipping blob transaction because it would exceed the max blob count per block");
                best_txs.mark_invalid(
                    &pool_tx,
                    InvalidPoolTransactionError::Eip4844(
                        Eip4844PoolTransactionError::TooManyEip4844Blobs {
                            have: block_blob_count + tx_blob_count,
                            permitted: max_blob_count,
                        },
                    ),
                );
                continue
            }
        }

        let gas_used = match builder.execute_transaction(tx.clone()) {
            Ok(gas_used) => gas_used,
            Err(BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                error, ..
            })) => {
                if error.is_nonce_too_low() {
                    // if the nonce is too low, we can skip this transaction
                    trace!(target: "payload_builder", %error, ?tx, "skipping nonce too low transaction");
                } else {
                    // if the transaction is invalid, we can skip it and all of its
                    // descendants
                    trace!(target: "payload_builder", %error, ?tx, "skipping invalid transaction and its descendants");
                    best_txs.mark_invalid(
                        &pool_tx,
                        InvalidPoolTransactionError::Consensus(
                            InvalidTransactionError::TxTypeNotSupported,
                        ),
                    );
                }
                continue
            }
            // this is an error that we should treat as fatal for this attempt
            Err(err) => return Err(PayloadBuilderError::evm(err)),
        };

        // add to the total blob gas used if the transaction successfully executed
        if let Some(blob_tx) = tx.as_eip4844() {
            block_blob_count += blob_tx.blob_versioned_hashes.len() as u64;

            // if we've reached the max blob count, we can skip blob txs entirely
            if block_blob_count == max_blob_count {
                best_txs.skip_blobs();
            }
        }

        // update add to total fees
        let miner_fee =
            tx.effective_tip_per_gas(base_fee).expect("fee is always valid; execution succeeded");
        total_fees += U256::from(miner_fee) * U256::from(gas_used);
        cumulative_gas_used += gas_used;

        // Push transaction changeset and calculate header bloom filter for receipt.
        // #[allow(clippy::needless_update)] // side-effect of optimism fields
        // receipts.push(Receipt {
        //     tx_type: tx.tx_type(),
        //     success: result.is_success(),
        //     cumulative_gas_used,
        //     logs: result.into_logs().into_iter().collect(),
        //     ..Default::default()
        // });

        // // update add to total fees
        // let miner_fee = tx
        //     .effective_tip_per_gas(base_fee)
        //     .expect("fee is always valid; execution succeeded");
        // total_fees += U256::from(miner_fee) * U256::from(gas_used);

        // // append transaction to the block body
        // executed_txs.push(tx.into_tx());
    }

    // check if we have a better block
    if !is_better_payload(best_payload.as_ref(), total_fees) {
        // Release db
        // drop(evm);
        drop(builder);

        // can skip building the block
        return Ok(BuildOutcome::Aborted {
            fees: total_fees,
            cached_reads,
        });
    }

    // < GNOSIS SPECIFIC
    // let blob_fee_to_collect = if chain_spec.is_prague_active_at_timestamp(attributes.timestamp) {
    //     let blob_gasprice = builder.evm_mut().block().blob_gasprice().unwrap_or(0);
    //     let blob_gas_used = (block_blob_count * DATA_GAS_PER_BLOB) as u128;
    //     blob_gas_used * blob_gasprice
    // } else {
    //     0
    // };

    // let evm = builder.evm_mut();
    // let (mut balance_increments, withdrawal_requests) = apply_post_block_system_calls(
    //     &chain_spec,
    //     // &evm_config,
    //     block_rewards_contract,
    //     attributes.timestamp,
    //     Some(&attributes.withdrawals),
    //     attributes.suggested_fee_recipient,
    //     evm,
    // )
    // .map_err(|err| PayloadBuilderError::Internal(err.into()))?;

    // if chain_spec.is_prague_active_at_timestamp(attributes.timestamp) {
    //     add_blob_fee_collection_to_balance_increments(
    //         &mut balance_increments,
    //         fee_collector_contract,
    //         blob_fee_to_collect,
    //     );
    // }

    // builder.evm_mut().db_mut()
    //     .increment_balances(balance_increments)
    //     .map_err(|err| {
    //         warn!(target: "payload_builder",
    //             parent_hash=%parent_header.hash(),
    //             %err,
    //             "failed to increment balances for payload"
    //         );
    //         PayloadBuilderError::Internal(err.into())
    //     })?;
    // GNOSIS SPECIFIC >

    let BlockBuilderOutcome { execution_result, block, .. } = builder.finish(&state_provider)?;

    // calculate the requests and the requests root
    // let requests = if chain_spec.is_prague_active_at_timestamp(attributes.timestamp) {
    //     let mut requests = Requests::default();

    //     let deposit_requests =
    //         parse_deposits_from_receipts(&chain_spec, &receipts).map_err(|err| {
    //             warn!(target: "payload_builder",
    //                 parent_hash=%parent_header.hash(),
    //                 %err,
    //                 "failed to parse deposits from receipts for payload"
    //             );
    //             PayloadBuilderError::Internal(RethError::Execution(err.into()))
    //         })?;
    //     if !deposit_requests.is_empty() {
    //         requests.push_request_with_type(eip6110::DEPOSIT_REQUEST_TYPE, deposit_requests);
    //     }

    //     if !withdrawal_requests.is_empty() {
    //         requests.push_request_with_type(eip7002::WITHDRAWAL_REQUEST_TYPE, withdrawal_requests);
    //     }

    //     // Collect all EIP-7251 requests
    //     let consolidation_requests = system_caller
    //         .apply_consolidation_requests_contract_call(&mut evm)
    //         .map_err(|err| {
    //             warn!(target: "payload_builder",
    //                 parent_hash=%parent_header.hash(),
    //                 %err,
    //                 "failed to apply consolidation requests contract call for payload"
    //             );
    //             PayloadBuilderError::Internal(err.into())
    //         })?;
    //     if !consolidation_requests.is_empty() {
    //         requests.push_request_with_type(
    //             eip7251::CONSOLIDATION_REQUEST_TYPE,
    //             consolidation_requests,
    //         );
    //     }

    //     Some(requests)
    // } else {
    //     None
    // };

    // Release db
    // drop(evm);

    // let withdrawals_root = Some(calculate_withdrawals_root(&attributes.withdrawals));

    // // merge all transitions into bundle state, this would apply the withdrawal balance changes
    // // and 4788 contract call
    // db.merge_transitions(BundleRetention::Reverts);

    // let requests_hash = requests.as_ref().map(|requests| requests.requests_hash());
    // let execution_outcome = ExecutionOutcome::new(
    //     db.take_bundle(),
    //     vec![receipts],
    //     block_number,
    //     vec![requests.clone().unwrap_or_default()],
    // );
    // let receipts_root = execution_outcome
    //     .ethereum_receipts_root(block_number)
    //     .expect("Number is in range");
    // let logs_bloom = execution_outcome
    //     .block_logs_bloom(block_number)
    //     .expect("Number is in range");

    // // calculate the state root
    // let hashed_state = db.database.db.hashed_post_state(execution_outcome.state());
    // let (state_root, _) = {
    //     db.database
    //         .inner()
    //         .state_root_with_updates(hashed_state)
    //         .inspect_err(|err| {
    //             warn!(target: "payload_builder",
    //                 parent_hash=%parent_header.hash(),
    //                 %err,
    //                 "failed to calculate state root for payload"
    //             );
    //         })?
    // };

    // // create the block header
    // let transactions_root = proofs::calculate_transaction_root(&executed_txs);

    // // initialize empty blob sidecars at first. If cancun is active then this will
    // let mut blob_sidecars = Vec::new();
    // let mut excess_blob_gas = None;
    // let mut blob_gas_used = None;

    // // only determine cancun fields when active
    // if chain_spec.is_cancun_active_at_timestamp(attributes.timestamp) {
    //     // grab the blob sidecars from the executed txs
    //     blob_sidecars = pool
    //         .get_all_blobs_exact(
    //             executed_txs
    //                 .iter()
    //                 .filter(|tx| tx.is_eip4844())
    //                 .map(|tx| *tx.tx_hash())
    //                 .collect(),
    //         )
    //         .map_err(PayloadBuilderError::other)?;

    //     excess_blob_gas = if chain_spec.is_cancun_active_at_timestamp(parent_header.timestamp) {
    //         let blob_params =
    //             get_blob_params(chain_spec.is_prague_active_at_timestamp(attributes.timestamp));
    //         parent_header.next_block_excess_blob_gas(blob_params)
    //     } else {
    //         // for the first post-fork block, both parent.blob_gas_used and
    //         // parent.excess_blob_gas are evaluated as 0
    //         Some(alloy_eips::eip4844::calc_excess_blob_gas(0, 0))
    //     };

    //     blob_gas_used = Some(block_blob_count * DATA_GAS_PER_BLOB);
    // }

    // let header = Header {
    //     parent_hash: parent_header.hash(),
    //     ommers_hash: EMPTY_OMMER_ROOT_HASH,
    //     beneficiary,
    //     state_root,
    //     transactions_root,
    //     receipts_root,
    //     withdrawals_root,
    //     logs_bloom,
    //     timestamp: attributes.timestamp,
    //     mix_hash: attributes.prev_randao,
    //     nonce: BEACON_NONCE.into(),
    //     base_fee_per_gas: Some(base_fee),
    //     number: parent_header.number + 1,
    //     gas_limit: block_gas_limit,
    //     difficulty: U256::ZERO,
    //     gas_used: cumulative_gas_used,
    //     extra_data: builder_config.extra_data,
    //     parent_beacon_block_root: attributes.parent_beacon_block_root,
    //     blob_gas_used,
    //     excess_blob_gas,
    //     requests_hash,
    // };

    // let withdrawals = chain_spec
    //     .is_shanghai_active_at_timestamp(attributes.timestamp)
    //     .then(|| attributes.withdrawals.clone());

    // // seal the block
    // let block = Block {
    //     header,
    //     body: BlockBody {
    //         transactions: executed_txs,
    //         ommers: vec![],
    //         withdrawals,
    //     },
    // };

    let requests = chain_spec
        .is_prague_active_at_timestamp(attributes.timestamp)
        .then_some(execution_result.requests);

    // initialize empty blob sidecars at first. If cancun is active then this will
    let mut blob_sidecars = Vec::new();

    // only determine cancun fields when active
    if chain_spec.is_cancun_active_at_timestamp(attributes.timestamp) {
        // grab the blob sidecars from the executed txs
        blob_sidecars = pool
            .get_all_blobs_exact(
                block
                    .body()
                    .transactions()
                    .filter(|tx| tx.is_eip4844())
                    .map(|tx| *tx.tx_hash())
                    .collect(),
            )
            .map_err(PayloadBuilderError::other)?;
    }

    let sealed_block = Arc::new(block.sealed_block().clone());
    debug!(target: "payload_builder", id=%attributes.id, sealed_block_header = ?sealed_block.sealed_header(), "sealed built block");

    let mut payload = EthBuiltPayload::new(attributes.id, sealed_block, total_fees, requests);

    // extend the payload with the blob sidecars from the executed txs
    payload.extend_sidecars(blob_sidecars.into_iter().map(Arc::unwrap_or_clone));

    Ok(BuildOutcome::Better {
        payload,
        cached_reads,
    })
}
