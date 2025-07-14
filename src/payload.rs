use std::sync::Arc;

use alloy_consensus::Transaction;
use alloy_eips::{eip7685::Requests, Typed2718};
use reth::rpc::types::engine::{BlobsBundleV1, BlobsBundleV2, ExecutionPayloadEnvelopeV5, ExecutionPayloadFieldV2, ExecutionPayloadV3};
use alloy_eips::eip7685::Requests;
use reth::rpc::types::engine::{
    BlobsBundleV1, BlobsBundleV2, ExecutionPayloadEnvelopeV5, ExecutionPayloadFieldV2,
    ExecutionPayloadV3,
};
use reth_basic_payload_builder::{
    is_better_payload, BuildArguments, BuildOutcome, PayloadBuilder, PayloadConfig,
};
use reth_chainspec::EthereumHardforks;
use reth_errors::{BlockExecutionError, BlockValidationError};
use reth_ethereum_engine_primitives::{BlobSidecars, BuiltPayloadConversionError};
// use reth_ethereum_engine_primitives::{BlobSidecars, BuiltPayloadConversionError};
use reth_ethereum_engine_primitives::{
    BuiltPayloadConversionError, ExecutionPayloadEnvelopeV2, ExecutionPayloadEnvelopeV3,
    ExecutionPayloadEnvelopeV4, ExecutionPayloadV1,
};
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_ethereum_primitives::TransactionSigned;
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome},
    ConfigureEvm, Evm, NextBlockEnvAttributes,
};
use reth_node_builder::{PayloadBuilderAttributes, PayloadBuilderError};
use reth_payload_builder::{BlobSidecars, EthBuiltPayload, EthPayloadBuilderAttributes};
use reth_primitives_traits::transaction::error::InvalidTransactionError;
use reth_ethereum_engine_primitives::{EthPayloadAttributes, ExecutionPayloadEnvelopeV2, ExecutionPayloadEnvelopeV3, ExecutionPayloadEnvelopeV4, ExecutionPayloadV1};
use reth_node_builder::{BuiltPayload, PayloadBuilderAttributes, PayloadBuilderError};
use reth_payload_builder::{EthPayloadBuilderAttributes, PayloadId};
use reth_primitives_traits::{transaction::error::InvalidTransactionError, SealedBlock};

use crate::primitives::{block::Block as GnosisBlock, header::GnosisHeader, GnosisNodePrimitives};
use reth_provider::{ChainSpecProvider, StateProviderFactory};
use reth_revm::{database::StateProviderDatabase, db::State};
use reth_transaction_pool::{
    error::{Eip4844PoolTransactionError, InvalidPoolTransactionError},
    BestTransactions, BestTransactionsAttributes, PoolTransaction, TransactionPool,
    ValidPoolTransaction,
};
use revm::context::Block;
use revm_primitives::U256;
use tracing::{debug, trace, warn};

use crate::{blobs::get_blob_params, spec::gnosis_spec::GnosisChainSpec};

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
    /// Payload builder configuration.
    builder_config: EthereumBuilderConfig,
}

impl<Pool, Client, EvmConfig> GnosisPayloadBuilder<Pool, Client, EvmConfig> {
    pub const fn new(
        client: Client,
        pool: Pool,
        evm_config: EvmConfig,
        builder_config: EthereumBuilderConfig,
    ) -> Self {
        Self {
            client,
            pool,
            evm_config,
            builder_config,
        }
    }
}

// Default implementation of [PayloadBuilder] for unit type
impl<Pool, Client, EvmConfig> PayloadBuilder for GnosisPayloadBuilder<Pool, Client, EvmConfig>
where
    EvmConfig:
        ConfigureEvm<Primitives = GnosisNodePrimitives, NextBlockEnvCtx = NextBlockEnvAttributes>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = GnosisChainSpec> + Clone,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
{
    type Attributes = EthPayloadBuilderAttributes;
    type BuiltPayload = GnosisBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<EthPayloadBuilderAttributes, GnosisBuiltPayload>,
    ) -> Result<BuildOutcome<GnosisBuiltPayload>, PayloadBuilderError> {
        default_ethereum_payload(
            self.evm_config.clone(),
            self.client.clone(),
            self.pool.clone(),
            self.builder_config.clone(),
            args,
            // evm_env,
            |attributes| self.pool.best_transactions_with_attributes(attributes),
        )
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, GnosisHeader>,
    ) -> Result<GnosisBuiltPayload, PayloadBuilderError> {
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
            |attributes| self.pool.best_transactions_with_attributes(attributes),
        )?
        .into_payload()
        .ok_or_else(|| PayloadBuilderError::MissingPayload)
    }
}

// REF: https://github.com/paradigmxyz/reth/blob/aac3a6eb47dc8601f718f4b7e0d82037921f6769/crates/ethereum/payload/src/lib.rs#L125
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
    args: BuildArguments<EthPayloadBuilderAttributes, GnosisBuiltPayload>,
    best_txs: F,
) -> Result<BuildOutcome<GnosisBuiltPayload>, PayloadBuilderError>
where
    EvmConfig:
        ConfigureEvm<Primitives = GnosisNodePrimitives, NextBlockEnvCtx = NextBlockEnvAttributes>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = GnosisChainSpec>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
    F: FnOnce(BestTransactionsAttributes) -> BestTransactionsIter<Pool>,
{
    let BuildArguments {
        mut cached_reads,
        config,
        cancel,
        best_payload,
    } = args;
    let PayloadConfig {
        parent_header,
        attributes,
    } = config;

    let state_provider = client.state_by_block_hash(parent_header.hash())?;
    let state = StateProviderDatabase::new(&state_provider);
    let mut db = State::builder()
        .with_database(cached_reads.as_db_mut(state))
        .with_bundle_update()
        .build();

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

    let mut best_txs = best_txs(BestTransactionsAttributes::new(
        base_fee,
        builder
            .evm_mut()
            .block()
            .blob_gasprice()
            .map(|gasprice| gasprice as u64),
    ));
    let mut total_fees = U256::ZERO;

    builder.apply_pre_execution_changes().map_err(|err| {
        warn!(target: "payload_builder", %err, "failed to apply pre-execution changes");
        PayloadBuilderError::Internal(err.into())
    })?;

    // initialize empty blob sidecars at first. If cancun is active then this will be populated by
    // blob sidecars if any.
    let mut blob_sidecars = BlobSidecars::Empty;

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
        let mut blob_tx_sidecar = None;
        if let Some(blob_tx) = tx.as_eip4844() {
            let tx_blob_count = blob_tx.tx().blob_versioned_hashes.len() as u64;

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
                continue;
            }

            let blob_sidecar_result = 'sidecar: {
                let Some(sidecar) = pool
                    .get_blob(*tx.hash())
                    .map_err(PayloadBuilderError::other)?
                else {
                    break 'sidecar Err(Eip4844PoolTransactionError::MissingEip4844BlobSidecar);
                };

                if chain_spec.is_osaka_active_at_timestamp(attributes.timestamp) {
                    if sidecar.is_eip7594() {
                        Ok(sidecar)
                    } else {
                        Err(Eip4844PoolTransactionError::UnexpectedEip4844SidecarAfterOsaka)
                    }
                } else if sidecar.is_eip4844() {
                    Ok(sidecar)
                } else {
                    Err(Eip4844PoolTransactionError::UnexpectedEip7594SidecarBeforeOsaka)
                }
            };

            blob_tx_sidecar = match blob_sidecar_result {
                Ok(sidecar) => Some(sidecar),
                Err(error) => {
                    best_txs.mark_invalid(&pool_tx, InvalidPoolTransactionError::Eip4844(error));
                    continue;
                }
            };
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
                continue;
            }
            // this is an error that we should treat as fatal for this attempt
            Err(err) => return Err(PayloadBuilderError::evm(err)),
        };

        // add to the total blob gas used if the transaction successfully executed
        if let Some(blob_tx) = tx.as_eip4844() {
            block_blob_count += blob_tx.tx().blob_versioned_hashes.len() as u64;

            // if we've reached the max blob count, we can skip blob txs entirely
            if block_blob_count == max_blob_count {
                best_txs.skip_blobs();
            }
        }

        // update add to total fees
        let miner_fee = tx
            .effective_tip_per_gas(base_fee)
            .expect("fee is always valid; execution succeeded");
        total_fees += U256::from(miner_fee) * U256::from(gas_used);
        cumulative_gas_used += gas_used;

        // Add blob tx sidecar to the payload.
        if let Some(sidecar) = blob_tx_sidecar {
            blob_sidecars.push_sidecar_variant(sidecar.as_ref().clone());
        }
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

    let BlockBuilderOutcome {
        execution_result,
        block,
        ..
    } = builder.finish(&state_provider)?;

    let requests = chain_spec
        .is_prague_active_at_timestamp(attributes.timestamp)
        .then_some(execution_result.requests);

    let sealed_block = Arc::new(block.sealed_block().clone());
    debug!(target: "payload_builder", id=%attributes.id, sealed_block_header = ?sealed_block.sealed_header(), "sealed built block");

    let payload = GnosisBuiltPayload::new(attributes.id, sealed_block, total_fees, requests)
        // extend the payload with the blob sidecars from the executed txs
        .with_sidecars(
            blob_sidecars
                .into_iter()
                .map(Arc::unwrap_or_clone)
                .collect::<Vec<_>>(),
        );

    Ok(BuildOutcome::Better {
        payload,
        cached_reads,
    })
}

#[derive(Debug, Clone)]
pub struct GnosisBuiltPayload {
    /// Identifier of the payload
    pub(crate) id: PayloadId,
    /// The built block
    pub(crate) block: Arc<SealedBlock<GnosisBlock>>,
    /// The fees of the block
    pub(crate) fees: U256,
    /// The blobs, proofs, and commitments in the block. If the block is pre-cancun, this will be
    /// empty.
    pub(crate) sidecars: BlobSidecars,
    /// The requests of the payload
    pub(crate) requests: Option<Requests>,
}

// === impl BuiltPayload ===

impl GnosisBuiltPayload {
    /// Initializes the payload with the given initial block
    ///
    /// Caution: This does not set any [`BlobSidecars`].
    pub const fn new(
        id: PayloadId,
        block: Arc<SealedBlock<GnosisBlock>>,
        fees: U256,
        requests: Option<Requests>,
    ) -> Self {
        Self {
            id,
            block,
            fees,
            requests,
            sidecars: BlobSidecars::Empty,
        }
    }

    /// Returns the identifier of the payload.
    pub const fn id(&self) -> PayloadId {
        self.id
    }

    /// Returns the built block(sealed)
    pub fn block(&self) -> &SealedBlock<GnosisBlock> {
        &self.block
    }

    /// Fees of the block
    pub const fn fees(&self) -> U256 {
        self.fees
    }

    /// Returns the blob sidecars.
    pub const fn sidecars(&self) -> &BlobSidecars {
        &self.sidecars
    }

    /// Sets blob transactions sidecars on the payload.
    pub fn with_sidecars(mut self, sidecars: impl Into<BlobSidecars>) -> Self {
        self.sidecars = sidecars.into();
        self
    }

    /// Try converting built payload into [`ExecutionPayloadEnvelopeV3`].
    ///
    /// Returns an error if the payload contains non EIP-4844 sidecar.
    pub fn try_into_v3(self) -> Result<ExecutionPayloadEnvelopeV3, BuiltPayloadConversionError> {
        let Self {
            block,
            fees,
            sidecars,
            ..
        } = self;

        let blobs_bundle = match sidecars {
            BlobSidecars::Empty => BlobsBundleV1::empty(),
            BlobSidecars::Eip4844(sidecars) => BlobsBundleV1::from(sidecars),
            BlobSidecars::Eip7594(_) => {
                return Err(BuiltPayloadConversionError::UnexpectedEip7594Sidecars)
            }
        };

        Ok(ExecutionPayloadEnvelopeV3 {
            execution_payload: ExecutionPayloadV3::from_block_unchecked(
                block.hash(),
                &Arc::unwrap_or_clone(block).into_block(),
            ),
            block_value: fees,
            // From the engine API spec:
            //
            // > Client software **MAY** use any heuristics to decide whether to set
            // `shouldOverrideBuilder` flag or not. If client software does not implement any
            // heuristic this flag **SHOULD** be set to `false`.
            //
            // Spec:
            // <https://github.com/ethereum/execution-apis/blob/fe8e13c288c592ec154ce25c534e26cb7ce0530d/src/engine/cancun.md#specification-2>
            should_override_builder: false,
            blobs_bundle,
        })
    }

    /// Try converting built payload into [`ExecutionPayloadEnvelopeV4`].
    ///
    /// Returns an error if the payload contains non EIP-4844 sidecar.
    pub fn try_into_v4(self) -> Result<ExecutionPayloadEnvelopeV4, BuiltPayloadConversionError> {
        Ok(ExecutionPayloadEnvelopeV4 {
            execution_requests: self.requests.clone().unwrap_or_default(),
            envelope_inner: self.try_into()?,
        })
    }

    /// Try converting built payload into [`ExecutionPayloadEnvelopeV5`].
    pub fn try_into_v5(self) -> Result<ExecutionPayloadEnvelopeV5, BuiltPayloadConversionError> {
        let Self {
            block,
            fees,
            sidecars,
            requests,
            ..
        } = self;

        let blobs_bundle = match sidecars {
            BlobSidecars::Empty => BlobsBundleV2::empty(),
            BlobSidecars::Eip7594(sidecars) => BlobsBundleV2::from(sidecars),
            BlobSidecars::Eip4844(_) => {
                return Err(BuiltPayloadConversionError::UnexpectedEip4844Sidecars)
            }
        };

        Ok(ExecutionPayloadEnvelopeV5 {
            execution_payload: ExecutionPayloadV3::from_block_unchecked(
                block.hash(),
                &Arc::unwrap_or_clone(block).into_block(),
            ),
            block_value: fees,
            // From the engine API spec:
            //
            // > Client software **MAY** use any heuristics to decide whether to set
            // `shouldOverrideBuilder` flag or not. If client software does not implement any
            // heuristic this flag **SHOULD** be set to `false`.
            //
            // Spec:
            // <https://github.com/ethereum/execution-apis/blob/fe8e13c288c592ec154ce25c534e26cb7ce0530d/src/engine/cancun.md#specification-2>
            should_override_builder: false,
            blobs_bundle,
            execution_requests: requests.unwrap_or_default(),
        })
    }
}

impl BuiltPayload for GnosisBuiltPayload {
    type Primitives = GnosisNodePrimitives;

    fn block(&self) -> &SealedBlock<GnosisBlock> {
        &self.block
    }

    fn fees(&self) -> U256 {
        self.fees
    }

    fn requests(&self) -> Option<Requests> {
        self.requests.clone()
    }
}

// V1 engine_getPayloadV1 response
impl From<GnosisBuiltPayload> for ExecutionPayloadV1 {
    fn from(value: GnosisBuiltPayload) -> Self {
        Self::from_block_unchecked(
            value.block().hash(),
            &Arc::unwrap_or_clone(value.block).into_block(),
        )
    }
}

// V2 engine_getPayloadV2 response
impl From<GnosisBuiltPayload> for ExecutionPayloadEnvelopeV2 {
    fn from(value: GnosisBuiltPayload) -> Self {
        let GnosisBuiltPayload { block, fees, .. } = value;

        Self {
            block_value: fees,
            execution_payload: ExecutionPayloadFieldV2::from_block_unchecked(
                block.hash(),
                &Arc::unwrap_or_clone(block).into_block(),
            ),
        }
    }
}

impl TryFrom<GnosisBuiltPayload> for ExecutionPayloadEnvelopeV3 {
    type Error = BuiltPayloadConversionError;

    fn try_from(value: GnosisBuiltPayload) -> Result<Self, Self::Error> {
        value.try_into_v3()
    }
}

impl TryFrom<GnosisBuiltPayload> for ExecutionPayloadEnvelopeV4 {
    type Error = BuiltPayloadConversionError;

    fn try_from(value: GnosisBuiltPayload) -> Result<Self, Self::Error> {
        value.try_into_v4()
    }
}

impl TryFrom<GnosisBuiltPayload> for ExecutionPayloadEnvelopeV5 {
    type Error = BuiltPayloadConversionError;

    fn try_from(value: GnosisBuiltPayload) -> Result<Self, Self::Error> {
        value.try_into_v5()
    }
}
