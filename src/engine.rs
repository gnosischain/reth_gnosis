// NOTE: Copied from https://github.com/paradigmxyz/reth/blob/2ebb519287ffc4dcfa75743337b10cd1d68aac2d/crates/ethereum/engine-primitives/src/lib.rs
// Relevant changes made for Gnosis types
// Needed for AddOns, debug capabilities and custom primitives

use crate::{
    consts::{
        is_blacklisted_setcode, is_sender_blacklisted, is_to_address_blacklisted, GnosisError,
        DEFAULT_7702_PATCH_TIME, DEFAULT_EL_PATCH_TIME,
    },
    payload::GnosisBuiltPayload,
    primitives::block::{GnosisBlock, IntoGnosisBlock, TransactionSigned},
    spec::gnosis_spec::{GnosisChainSpec, GnosisHardForks},
};
use alloy_consensus::Transaction;
use reth::rpc::types::engine::{ExecutionData, ExecutionPayload, ExecutionPayloadEnvelopeV5};
use reth_ethereum_engine_primitives::{
    EthPayloadAttributes, ExecutionPayloadEnvelopeV2, ExecutionPayloadEnvelopeV3,
    ExecutionPayloadEnvelopeV4, ExecutionPayloadV1,
};
use reth_ethereum_payload_builder::EthereumExecutionPayloadValidator;
use reth_node_builder::{
    validate_execution_requests, validate_version_specific_fields, BuiltPayload,
    EngineApiMessageVersion, EngineApiValidator, EngineObjectValidationError, EngineTypes,
    NewPayloadError, PayloadOrAttributes, PayloadTypes, PayloadValidator,
};
use reth_payload_builder::EthPayloadBuilderAttributes;
use reth_primitives::{NodePrimitives, RecoveredBlock};
use reth_primitives_traits::SealedBlock;
use serde::{Deserialize, Serialize};
use std::{env, sync::Arc};

/// Custom engine types - uses a custom payload attributes RPC type, but uses the default
/// payload builder attributes type.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct GnosisEngineTypes;

impl PayloadTypes for GnosisEngineTypes {
    type ExecutionData = ExecutionData;
    type BuiltPayload = GnosisBuiltPayload;
    type PayloadAttributes = EthPayloadAttributes;
    type PayloadBuilderAttributes = EthPayloadBuilderAttributes;

    fn block_to_payload(
        block: SealedBlock<
            <<Self::BuiltPayload as BuiltPayload>::Primitives as NodePrimitives>::Block,
        >,
    ) -> ExecutionData {
        let (payload, sidecar) =
            ExecutionPayload::from_block_unchecked(block.hash(), &block.into_block());
        ExecutionData { payload, sidecar }
    }
}

impl EngineTypes for GnosisEngineTypes {
    type ExecutionPayloadEnvelopeV1 = ExecutionPayloadV1;
    type ExecutionPayloadEnvelopeV2 = ExecutionPayloadEnvelopeV2;
    type ExecutionPayloadEnvelopeV3 = ExecutionPayloadEnvelopeV3;
    type ExecutionPayloadEnvelopeV4 = ExecutionPayloadEnvelopeV4;
    type ExecutionPayloadEnvelopeV5 = ExecutionPayloadEnvelopeV5;
}

/// Custom engine validator
#[derive(Debug, Clone)]
pub struct GnosisEngineValidator {
    inner: EthereumExecutionPayloadValidator<GnosisChainSpec>,
}

impl GnosisEngineValidator {
    /// Creates a new Gnosis engine validator.
    pub const fn new(chain_spec: Arc<GnosisChainSpec>) -> Self {
        Self {
            inner: EthereumExecutionPayloadValidator::new(chain_spec),
        }
    }

    /// Returns the chain spec used by the validator.
    #[inline]
    fn chain_spec(&self) -> &GnosisChainSpec {
        self.inner.chain_spec()
    }
}

impl PayloadValidator<GnosisEngineTypes> for GnosisEngineValidator {
    type Block = GnosisBlock;

    fn ensure_well_formed_payload(
        &self,
        payload: ExecutionData,
    ) -> Result<RecoveredBlock<GnosisBlock>, NewPayloadError> {
        // Convert payload to sealed GnosisBlock
        let sealed_block = self.convert_payload_to_block(payload)?;

        // Recover transaction senders
        let block = sealed_block
            .try_recover()
            .map_err(|e| NewPayloadError::Other(e.into()))?;

        if !self
            .chain_spec()
            .is_balancer_hardfork_active_at_timestamp(block.timestamp)
            && block.timestamp
                > env::var("GNOSIS_EL_PATCH_TIME")
                    .unwrap_or(DEFAULT_EL_PATCH_TIME.to_string())
                    .parse::<u64>()
                    .unwrap_or_default()
        {
            let is_patch2_enabled: bool = block.timestamp
                > env::var("GNOSIS_EL_7702_PATCH_TIME")
                    .unwrap_or(DEFAULT_7702_PATCH_TIME.to_string())
                    .parse::<u64>()
                    .unwrap_or_default();

            for (sender, tx) in block.transactions_with_sender() {
                if is_sender_blacklisted(sender)
                    || is_to_address_blacklisted(&tx.to().unwrap_or_default())
                    || (is_patch2_enabled && is_blacklisted_setcode(tx))
                {
                    return Err(NewPayloadError::other(GnosisError::custom(format!(
                            "Unable to proceed (ensure_well_formed_payload) - signer: {}, to: {:?}, block: {}, {}",
                            &sender,
                            &tx.to().unwrap_or_default(),
                            &block.number,
                            &block.hash()
                        ))));
                }
            }
        }

        Ok(block)
    }

    fn convert_payload_to_block(
        &self,
        payload: ExecutionData,
    ) -> Result<SealedBlock<Self::Block>, NewPayloadError> {
        // Use the inner validator to ensure the payload is well-formed
        let sealed_block = self
            .inner
            .ensure_well_formed_payload::<TransactionSigned>(payload)?;

        // Extract hash and convert to GnosisBlock
        let hash = sealed_block.hash();
        let gnosis_block = sealed_block.into_block().into_gnosis_block();

        // Create the sealed GnosisBlock with the same hash
        Ok(SealedBlock::new_unchecked(gnosis_block, hash))
    }
}

impl EngineApiValidator<GnosisEngineTypes> for GnosisEngineValidator {
    fn validate_version_specific_fields(
        &self,
        version: EngineApiMessageVersion,
        payload_or_attrs: PayloadOrAttributes<'_, ExecutionData, EthPayloadAttributes>,
    ) -> Result<(), EngineObjectValidationError> {
        payload_or_attrs
            .execution_requests()
            .map(|requests| validate_execution_requests(requests))
            .transpose()?;

        validate_version_specific_fields(self.chain_spec(), version, payload_or_attrs)
    }

    fn ensure_well_formed_attributes(
        &self,
        version: EngineApiMessageVersion,
        attributes: &EthPayloadAttributes,
    ) -> Result<(), EngineObjectValidationError> {
        validate_version_specific_fields(
            self.chain_spec(),
            version,
            PayloadOrAttributes::<ExecutionData, EthPayloadAttributes>::PayloadAttributes(
                attributes,
            ),
        )
    }
}
