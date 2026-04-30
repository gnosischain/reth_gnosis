pub mod config;
pub mod finality;
pub mod seal;
pub mod validators;

use std::sync::Arc;

use alloy_consensus::{constants::EMPTY_OMMER_ROOT_HASH, BlockHeader};
use gnosis_primitives::header::GnosisHeader;
use reth_chainspec::EthereumHardforks;
use reth_consensus::{Consensus, ConsensusError, FullConsensus, HeaderValidator, ReceiptRootBloom};
use reth_consensus_common::validation::{
    validate_against_parent_eip1559_base_fee, validate_against_parent_hash_number,
    validate_against_parent_timestamp, validate_block_pre_execution, validate_body_against_header,
    validate_header_extra_data, validate_header_gas,
};
use reth_ethereum_consensus::{validate_block_post_execution, EthBeaconConsensus};
use reth_execution_types::BlockExecutionResult;
use reth_primitives_traits::{Block, NodePrimitives, RecoveredBlock, SealedBlock, SealedHeader};

use crate::primitives::GnosisNodePrimitives;
use crate::spec::gnosis_spec::GnosisChainSpec;

use self::config::AuraConfig;
use self::seal::{calculate_aura_difficulty, recover_seal_author};
use self::validators::ValidatorSet;

/// Gnosis consensus implementation that handles both AuRa (pre-merge) and
/// Beacon (post-merge) consensus.
#[derive(Debug, Clone)]
pub struct GnosisConsensus {
    /// The inner EthBeaconConsensus for post-merge validation.
    inner: EthBeaconConsensus<GnosisChainSpec>,
    /// The AuRa configuration (if available in the chain spec).
    aura_config: Option<AuraConfig>,
    /// Chain spec reference.
    chain_spec: Arc<GnosisChainSpec>,
}

impl GnosisConsensus {
    /// Create a new `GnosisConsensus` from a chain spec.
    pub fn new(chain_spec: Arc<GnosisChainSpec>) -> Self {
        let aura_config = chain_spec.aura_config.clone();
        Self {
            inner: EthBeaconConsensus::new(chain_spec.clone()),
            aura_config,
            chain_spec,
        }
    }

    /// Returns true if the header is a pre-merge AuRa header.
    fn is_aura_header(header: &GnosisHeader) -> bool {
        header.is_pre_merge()
    }
}

impl HeaderValidator<GnosisHeader> for GnosisConsensus {
    fn validate_header(&self, header: &SealedHeader<GnosisHeader>) -> Result<(), ConsensusError> {
        if Self::is_aura_header(header.header()) {
            validate_aura_header(header.header(), &self.chain_spec)
        } else {
            self.inner.validate_header(header)
        }
    }

    fn validate_header_against_parent(
        &self,
        header: &SealedHeader<GnosisHeader>,
        parent: &SealedHeader<GnosisHeader>,
    ) -> Result<(), ConsensusError> {
        if Self::is_aura_header(header.header()) {
            validate_aura_header_against_parent(
                header,
                parent,
                &self.chain_spec,
                self.aura_config.as_ref(),
            )
        } else {
            self.inner.validate_header_against_parent(header, parent)
        }
    }
}

impl<B> Consensus<B> for GnosisConsensus
where
    B: Block<Header = GnosisHeader>,
{
    fn validate_body_against_header(
        &self,
        body: &B::Body,
        header: &SealedHeader<B::Header>,
    ) -> Result<(), ConsensusError> {
        validate_body_against_header(body, header.header())
    }

    fn validate_block_pre_execution(&self, block: &SealedBlock<B>) -> Result<(), ConsensusError> {
        if Self::is_aura_header(block.header()) {
            // For pre-merge AuRa blocks, validate ommers are empty and tx root matches.
            // Skip hardfork-specific post-merge checks.
            validate_body_against_header(block.body(), block.header())?;

            // Ommers must be empty in AuRa
            if block.header().ommers_hash() != EMPTY_OMMER_ROOT_HASH {
                return Err(ConsensusError::TheMergeOmmerRootIsNotEmpty);
            }

            Ok(())
        } else {
            validate_block_pre_execution(block, &*self.chain_spec)
        }
    }
}

impl FullConsensus<GnosisNodePrimitives> for GnosisConsensus {
    fn validate_block_post_execution(
        &self,
        block: &RecoveredBlock<<GnosisNodePrimitives as NodePrimitives>::Block>,
        result: &BlockExecutionResult<<GnosisNodePrimitives as NodePrimitives>::Receipt>,
        receipt_root_bloom: Option<ReceiptRootBloom>,
    ) -> Result<(), ConsensusError> {
        validate_block_post_execution(
            block,
            &*self.chain_spec,
            &result.receipts,
            &result.requests,
            receipt_root_bloom,
        )
    }
}

/// Validate a pre-merge AuRa header in isolation.
fn validate_aura_header(
    header: &GnosisHeader,
    chain_spec: &GnosisChainSpec,
) -> Result<(), ConsensusError> {
    // AuRa step must be present
    if header.aura_step.is_none() {
        tracing::warn!(
            target: "reth::gnosis",
            block = header.number,
            "Validation FAILED: missing AuRa step"
        );
        return Err(ConsensusError::Other(
            "missing AuRa step in pre-merge header".into(),
        ));
    }

    // AuRa seal must be present
    if header.aura_seal.is_none() {
        tracing::warn!(
            target: "reth::gnosis",
            block = header.number,
            "Validation FAILED: missing AuRa seal"
        );
        return Err(ConsensusError::Other(
            "missing AuRa seal in pre-merge header".into(),
        ));
    }

    // Ommers must be empty in AuRa
    if header.ommers_hash != EMPTY_OMMER_ROOT_HASH {
        return Err(ConsensusError::TheMergeOmmerRootIsNotEmpty);
    }

    // Common checks
    validate_header_extra_data(header, 32)?;
    validate_header_gas(header)?;

    // Base fee validation (London+)
    // Note: skip base fee standalone validation for now — it's checked against parent

    // Withdrawals root must not be present pre-Shanghai
    if !chain_spec.is_shanghai_active_at_timestamp(header.timestamp) {
        if header.withdrawals_root.is_some() {
            return Err(ConsensusError::WithdrawalsRootUnexpected);
        }
    }

    // Blob fields must not be present pre-Cancun
    if !chain_spec.is_cancun_active_at_timestamp(header.timestamp) {
        if header.blob_gas_used.is_some()
            || header.excess_blob_gas.is_some()
            || header.parent_beacon_block_root.is_some()
        {
            return Err(ConsensusError::BlobGasUsedUnexpected);
        }
    }

    Ok(())
}

/// Validate a pre-merge AuRa header against its parent.
fn validate_aura_header_against_parent(
    header: &SealedHeader<GnosisHeader>,
    parent: &SealedHeader<GnosisHeader>,
    chain_spec: &GnosisChainSpec,
    aura_config: Option<&AuraConfig>,
) -> Result<(), ConsensusError> {
    // Standard parent validations
    validate_against_parent_hash_number(header.header(), parent)?;
    validate_against_parent_timestamp(header.header(), parent.header())?;

    // Base fee validation (if London is active)
    validate_against_parent_eip1559_base_fee(header.header(), parent.header(), chain_spec)?;

    // Skip gas limit ramp check — AuRa networks may use gas limit contracts

    // AuRa step must be monotonically increasing
    let current_step = header
        .header()
        .aura_step
        .ok_or_else(|| ConsensusError::Other("missing AuRa step".into()))?;

    if parent.header().is_pre_merge() {
        let parent_step = parent
            .header()
            .aura_step
            .ok_or_else(|| ConsensusError::Other("missing parent AuRa step".into()))?;

        if current_step <= parent_step {
            return Err(ConsensusError::Other(
                format!(
                    "AuRa step must be monotonically increasing: current={}, parent={}",
                    current_step, parent_step
                )
                .into(),
            ));
        }

        // Verify AuRa difficulty
        let expected_difficulty =
            calculate_aura_difficulty(parent_step.to::<u64>(), current_step.to::<u64>());
        if header.header().difficulty != expected_difficulty {
            return Err(ConsensusError::Other(
                format!(
                    "AuRa difficulty mismatch: expected={}, got={}",
                    expected_difficulty,
                    header.header().difficulty
                )
                .into(),
            ));
        }
    }

    // Verify seal signature if we have AuRa config
    if let Some(config) = aura_config {
        let block_number = header.header().number;

        // Recover signer from seal
        let signer = recover_seal_author(header.header()).map_err(|e| {
            ConsensusError::Other(format!("AuRa seal verification failed: {}", e).into())
        })?;

        // Get expected validators. The proposer for block N is determined by
        // the validator set active at block N-1 (the parent), because the new
        // set at a multi-transition block only applies to blocks AFTER it.
        let proposer_lookup_block = block_number.saturating_sub(1);
        if let Some(validators) = config
            .validators
            .try_get_list_validators(proposer_lookup_block)
        {
            let step = current_step.to::<u64>();
            let expected_proposer = ValidatorSet::expected_proposer(step, validators);

            if signer != expected_proposer {
                tracing::warn!(
                    target: "reth::gnosis",
                    block = block_number,
                    expected = %expected_proposer,
                    got = %signer,
                    step = step,
                    num_validators = validators.len(),
                    "Validation FAILED: AuRa proposer mismatch"
                );
                return Err(ConsensusError::Other(
                    format!(
                        "AuRa proposer mismatch: expected={}, got={} (step={}, block={})",
                        expected_proposer, signer, step, block_number
                    )
                    .into(),
                ));
            }
        }
        // For contract-based validators, we skip proposer verification in consensus
        // (it would require EVM state access which isn't available here)
    }

    Ok(())
}
