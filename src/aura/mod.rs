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

    /// Returns `true` if `header` belongs to the pre-merge AuRa phase, `false`
    /// if it belongs to the post-merge Beacon phase. The chain spec is the
    /// authoritative source of phase.
    ///
    /// Also rejects headers whose structure (`aura_step` / `aura_seal` presence)
    /// disagrees with the spec-derived phase. A peer that crafts a header with
    /// AuRa fields at a post-merge block number would otherwise be routed into
    /// AuRa validation while execution treats it as PoS — that mismatch is
    /// caught here.
    fn is_aura_block(&self, header: &GnosisHeader) -> Result<bool, ConsensusError> {
        let pre_merge_per_spec = !self.chain_spec.is_paris_active_at_block(header.number);
        let pre_merge_per_header = header.is_pre_merge();
        if pre_merge_per_spec != pre_merge_per_header {
            return Err(ConsensusError::msg(format!(
                "header phase mismatch at block {}: chain-spec pre_merge={}, header pre_merge={}",
                header.number, pre_merge_per_spec, pre_merge_per_header
            )));
        }
        Ok(pre_merge_per_spec)
    }
}

impl HeaderValidator<GnosisHeader> for GnosisConsensus {
    fn validate_header(&self, header: &SealedHeader<GnosisHeader>) -> Result<(), ConsensusError> {
        if self.is_aura_block(header.header())? {
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
        if self.is_aura_block(header.header())? {
            // We're validating an AuRa block, so aura_config MUST be present in
            // the chain spec. A chain with pre-merge phase but no `aura` section
            // in genesis is a misconfiguration; refuse rather than silently
            // skipping seal + proposer verification.
            let aura_config = self.aura_config.as_ref().ok_or_else(|| {
                ConsensusError::msg(
                    "AuRa config missing for chain with pre-merge phase: \
                     genesis must contain an `aura` section",
                )
            })?;
            validate_aura_header_against_parent(header, parent, &self.chain_spec, aura_config)
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
        if self.is_aura_block(block.header())? {
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
        validate_block_post_execution(block, &*self.chain_spec, result, receipt_root_bloom)
    }
}

/// Validate a pre-merge AuRa header in isolation.
///
/// Caller invariant (`GnosisConsensus::validate_header` via `is_aura_block`):
/// `header.aura_step` and `header.aura_seal` are both `Some` — otherwise
/// `is_aura_block` would have rejected the header for spec/structure
/// mismatch before reaching this function. No need to re-check here.
fn validate_aura_header(
    header: &GnosisHeader,
    chain_spec: &GnosisChainSpec,
) -> Result<(), ConsensusError> {
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
    if !chain_spec.is_shanghai_active_at_timestamp(header.timestamp)
        && header.withdrawals_root.is_some()
    {
        return Err(ConsensusError::WithdrawalsRootUnexpected);
    }

    // Blob fields must not be present pre-Cancun
    if !chain_spec.is_cancun_active_at_timestamp(header.timestamp)
        && (header.blob_gas_used.is_some()
            || header.excess_blob_gas.is_some()
            || header.parent_beacon_block_root.is_some())
    {
        return Err(ConsensusError::BlobGasUsedUnexpected);
    }

    Ok(())
}

/// Validate a pre-merge AuRa header against its parent.
fn validate_aura_header_against_parent(
    header: &SealedHeader<GnosisHeader>,
    parent: &SealedHeader<GnosisHeader>,
    chain_spec: &GnosisChainSpec,
    aura_config: &AuraConfig,
) -> Result<(), ConsensusError> {
    // Standard parent validations
    validate_against_parent_hash_number(header.header(), parent)?;
    validate_against_parent_timestamp(header.header(), parent.header())?;

    // Base fee validation (if London is active)
    validate_against_parent_eip1559_base_fee(header.header(), parent.header(), chain_spec)?;

    // Skip gas limit ramp check — AuRa networks may use gas limit contracts

    // AuRa step must be present and fit in u64. A peer-supplied U256 step that
    // exceeds u64 must NOT panic the node; reject the header instead.
    let current_step = header
        .header()
        .aura_step
        .ok_or_else(|| ConsensusError::msg("missing AuRa step"))?;
    let current_step: u64 = current_step
        .try_into()
        .map_err(|_| ConsensusError::msg(format!("AuRa step exceeds u64: {current_step}")))?;

    // The caller (`GnosisConsensus::validate_header_against_parent`) only
    // dispatches here when `is_aura_block(child)` returns `true`, i.e. when
    // the child is pre-merge per the chain spec. The phase is monotonic by
    // block number, so a valid pre-merge child has a pre-merge parent —
    // structurally this means `parent.aura_step` is `Some`. We unwrap with
    // `ok_or_else` rather than guarding on `parent.is_pre_merge()` so that a
    // malformed parent (peer-crafted with `aura_step = None`) is rejected
    // instead of silently skipping step + difficulty checks.
    let parent_step = parent
        .header()
        .aura_step
        .ok_or_else(|| ConsensusError::msg("missing parent AuRa step"))?;
    let parent_step: u64 = parent_step
        .try_into()
        .map_err(|_| ConsensusError::msg(format!("AuRa parent step exceeds u64: {parent_step}")))?;

    if current_step <= parent_step {
        return Err(ConsensusError::msg(format!(
            "AuRa step must be monotonically increasing: current={current_step}, parent={parent_step}",
        )));
    }

    // Verify AuRa difficulty.
    let expected_difficulty = calculate_aura_difficulty(parent_step, current_step);
    if header.header().difficulty != expected_difficulty {
        return Err(ConsensusError::msg(format!(
            "AuRa difficulty mismatch: expected={}, got={}",
            expected_difficulty,
            header.header().difficulty
        )));
    }

    // Seal signature + proposer check. Always runs in pre-merge AuRa phase —
    // the caller guarantees a valid `aura_config` is present.
    let block_number = header.header().number;

    // Recover signer from seal
    let signer = recover_seal_author(header.header())
        .map_err(|e| ConsensusError::msg(format!("AuRa seal verification failed: {}", e)))?;

    // Get expected validators. The proposer for block N is determined by
    // the validator set active at block N-1 (the parent), because the new
    // set at a multi-transition block only applies to blocks AFTER it.
    let proposer_lookup_block = block_number.saturating_sub(1);
    if let Some(validators) = aura_config
        .validators
        .try_get_list_validators(proposer_lookup_block)
    {
        // current_step is already validated to fit in u64 above.
        let step = current_step;
        let expected_proposer =
            ValidatorSet::expected_proposer(step, validators).ok_or_else(|| {
                ConsensusError::msg("AuRa validator list is empty for proposer lookup")
            })?;

        if signer != expected_proposer {
            return Err(ConsensusError::msg(format!(
                "AuRa proposer mismatch: expected={}, got={} (step={}, block={})",
                expected_proposer, signer, step, block_number
            )));
        }
    } else {
        // Contract-based validator set: proposer check is deferred to execution
        // (resolving the active validator list requires EVM state access, which
        // isn't available at consensus-validation time). Log so a chainspec
        // misconfig that flips a List into a Contract here is at least visible.
        tracing::debug!(
            target: "reth::gnosis",
            block = block_number,
            "AuRa proposer check deferred to execution: contract-based validator set"
        );
    }

    Ok(())
}
