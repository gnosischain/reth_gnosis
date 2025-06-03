use std::sync::Arc;

use derive_more::Deref;
use reth::rpc::types::engine::{ExecutionData, ExecutionPayload, ExecutionPayloadEnvelopeV5};
use reth_chainspec::ChainSpec;
use reth_ethereum_engine_primitives::{EthPayloadAttributes, ExecutionPayloadEnvelopeV2, ExecutionPayloadEnvelopeV3, ExecutionPayloadEnvelopeV4, ExecutionPayloadV1};
use reth_ethereum_payload_builder::EthereumExecutionPayloadValidator;
use reth_node_builder::{validate_execution_requests, validate_version_specific_fields, BuiltPayload, EngineApiMessageVersion, EngineObjectValidationError, EngineTypes, EngineValidator, InvalidPayloadAttributesError, NewPayloadError, PayloadOrAttributes, PayloadTypes, PayloadValidator};
use reth_payload_builder::EthPayloadBuilderAttributes;
use reth_primitives::{NodePrimitives, RecoveredBlock};
use reth_primitives_traits::SealedBlock;
use serde::{Deserialize, Serialize};

use crate::{payload::GnosisBuiltPayload, primitives::block::{Block as GnosisBlock, IntoBlock, TransactionSigned}, spec::gnosis_spec::GnosisChainSpec};

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
    inner: EthereumExecutionPayloadValidator<ChainSpec>,
}

impl GnosisEngineValidator {
    /// Creates a new Gnosis engine validator.
    pub const fn new(chain_spec: Arc<ChainSpec>) -> Self {
        Self { inner: EthereumExecutionPayloadValidator::new(chain_spec) }
    }

    /// Returns the chain spec used by the validator.
    #[inline]
    fn chain_spec(&self) -> &ChainSpec {
        self.inner.chain_spec()
    }
}

impl PayloadValidator for GnosisEngineValidator {
    type Block = GnosisBlock;
    type ExecutionData = ExecutionData;

    fn ensure_well_formed_payload(
        &self,
        payload: ExecutionData,
    ) -> Result<RecoveredBlock<GnosisBlock>, NewPayloadError> {
        let sealed_block = self.inner.ensure_well_formed_payload::<TransactionSigned>(payload)?;
        let result = sealed_block.try_recover().map_err(|e| NewPayloadError::Other(e.into()));
        
        let block = result.unwrap();
        let senders = block.senders().to_owned();
        let hash = block.hash();
        let gnosis_block: GnosisBlock = block.into_block().into_block();
        let block = RecoveredBlock::<GnosisBlock>::new(gnosis_block, senders, hash);
        Ok(block)
    }
}

impl<T> EngineValidator<T> for GnosisEngineValidator
where
    T: PayloadTypes<PayloadAttributes = EthPayloadAttributes, ExecutionData = ExecutionData>,
{
    fn validate_version_specific_fields(
        &self,
        version: EngineApiMessageVersion,
        payload_or_attrs: PayloadOrAttributes<'_, Self::ExecutionData, EthPayloadAttributes>,
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
            PayloadOrAttributes::<Self::ExecutionData, EthPayloadAttributes>::PayloadAttributes(
                attributes,
            ),
        )
    }
}
