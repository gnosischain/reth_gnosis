use alloy_consensus::Header;
use reth::{
    consensus_common::validation::{
        validate_against_parent_4844, validate_against_parent_eip1559_base_fee,
        validate_against_parent_hash_number, validate_body_against_header, validate_cancun_gas,
        validate_header_base_fee, validate_header_gas, validate_shanghai_withdrawals,
    },
    primitives::{BlockBody, BlockWithSenders, SealedBlock, SealedHeader},
};
use reth_chainspec::{ChainSpec, EthereumHardforks};
use reth_consensus::{Consensus, ConsensusError, PostExecutionInput};
use reth_primitives::GotExpected;
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
    fn validate_header(&self, header: &SealedHeader) -> Result<(), ConsensusError> {
        validate_header_gas(header)?;
        validate_header_base_fee(header, &self.chain_spec)
    }

    fn validate_header_against_parent(
        &self,
        header: &SealedHeader,
        parent: &SealedHeader,
    ) -> Result<(), ConsensusError> {
        validate_against_parent_hash_number(header, parent)?;
        validate_against_parent_eip1559_base_fee(header, parent, &self.chain_spec)?;

        // ensure that the blob gas fields for this block
        if self
            .chain_spec
            .is_cancun_active_at_timestamp(header.timestamp)
        {
            validate_against_parent_4844(header, parent)?;
        }

        Ok(())
    }

    fn validate_header_with_total_difficulty(
        &self,
        _header: &Header,
        _total_difficulty: U256,
    ) -> Result<(), ConsensusError> {
        // TODO
        Ok(())
    }

    fn validate_body_against_header(
        &self,
        body: &BlockBody,
        header: &SealedHeader,
    ) -> Result<(), ConsensusError> {
        validate_body_against_header(body, header)
    }

    // fn validate_block_pre_execution(&self, _block: &SealedBlock) -> Result<(), ConsensusError> {
    //     // TODO
    //     Ok(())
    // }

    fn validate_block_pre_execution(&self, block: &SealedBlock) -> Result<(), ConsensusError> {
        // Check ommers hash
        let ommers_hash = reth_primitives::proofs::calculate_ommers_root(&block.body.ommers);
        if block.header.ommers_hash != ommers_hash {
            return Err(ConsensusError::BodyOmmersHashDiff(
                GotExpected {
                    got: ommers_hash,
                    expected: block.header.ommers_hash,
                }
                .into(),
            ));
        }

        // Check transaction root
        if let Err(error) = block.ensure_transaction_root_valid() {
            return Err(ConsensusError::BodyTransactionRootDiff(error.into()));
        }

        // EIP-4895: Beacon chain push withdrawals as operations
        if self
            .chain_spec
            .is_shanghai_active_at_timestamp(block.timestamp)
        {
            validate_shanghai_withdrawals(block)?;
        }

        if self
            .chain_spec
            .is_cancun_active_at_timestamp(block.timestamp)
        {
            validate_cancun_gas(block)?;
        }

        Ok(())
    }

    fn validate_block_post_execution(
        &self,
        _block: &BlockWithSenders,
        _input: PostExecutionInput<'_>,
    ) -> Result<(), ConsensusError> {
        // TODO
        Ok(())
    }
}
