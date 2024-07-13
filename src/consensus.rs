use reth::primitives::{BlockWithSenders, Header, SealedBlock, SealedHeader};
use reth_chainspec::ChainSpec;
use reth_consensus::{Consensus, ConsensusError, PostExecutionInput};
use revm_primitives::U256;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GnosisBeaconConsensus {
    /// Configuration
    chain_spec: Arc<ChainSpec>,
}

impl GnosisBeaconConsensus {
    pub fn new(chain_spec: Arc<ChainSpec>) -> Self {
        Self { chain_spec }
    }
}

// `validate_header`, `validate_header_against_parent`, `validate_header_with_total_difficulty`, `validate_block_pre_execution`, `validate_block_post_execution`
impl Consensus for GnosisBeaconConsensus {
    fn validate_header(&self, _header: &SealedHeader) -> Result<(), ConsensusError> {
        todo!();
    }

    fn validate_header_against_parent(
        &self,
        _header: &SealedHeader,
        _parent: &SealedHeader,
    ) -> Result<(), ConsensusError> {
        todo!();
    }

    fn validate_header_with_total_difficulty(
        &self,
        _header: &Header,
        _total_difficulty: U256,
    ) -> Result<(), ConsensusError> {
        todo!();
    }

    fn validate_block_pre_execution(&self, _block: &SealedBlock) -> Result<(), ConsensusError> {
        todo!();
    }

    fn validate_block_post_execution(
        &self,
        _block: &BlockWithSenders,
        _input: PostExecutionInput<'_>,
    ) -> Result<(), ConsensusError> {
        todo!();
    }
}
