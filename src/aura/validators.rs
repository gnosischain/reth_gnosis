use alloy_primitives::Address;
use std::collections::BTreeMap;

/// A single validator set type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidatorSetKind {
    /// Static list of validators.
    List(Vec<Address>),
    /// Validators from a "safe" contract (getValidators() call, no reporting).
    SafeContract { address: Address },
    /// Validators from a contract (getValidators() call + misbehavior reporting).
    Contract { address: Address },
}

/// Multi-validator set: transitions between different validator sets at specific block numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatorSet {
    /// Sorted by block number. Each entry: (activation_block, validator_set_kind)
    sets: BTreeMap<u64, ValidatorSetKind>,
}

impl ValidatorSet {
    /// Create a new multi-validator set.
    pub fn new(sets: BTreeMap<u64, ValidatorSetKind>) -> Self {
        assert!(!sets.is_empty(), "validator set must not be empty");
        Self { sets }
    }

    /// Get the validator set kind active at the given block number.
    /// Returns the set with the highest activation block <= block_number.
    pub fn kind_at(&self, block_number: u64) -> &ValidatorSetKind {
        self.sets
            .range(..=block_number)
            .next_back()
            .map(|(_, kind)| kind)
            .expect("validator set must have entry at block 0")
    }

    /// For List-type validator sets, return the validators directly.
    /// For contract-based types, this returns None — caller must resolve via EVM.
    pub fn try_get_list_validators(&self, block_number: u64) -> Option<&[Address]> {
        match self.kind_at(block_number) {
            ValidatorSetKind::List(addrs) => Some(addrs),
            ValidatorSetKind::SafeContract { .. } | ValidatorSetKind::Contract { .. } => None,
        }
    }

    /// Get the contract address for contract-based validator sets.
    pub fn contract_address_at(&self, block_number: u64) -> Option<Address> {
        match self.kind_at(block_number) {
            ValidatorSetKind::List(_) => None,
            ValidatorSetKind::SafeContract { address } | ValidatorSetKind::Contract { address } => {
                Some(*address)
            }
        }
    }

    /// Compute the expected proposer for the given step and validator list.
    /// Uses round-robin: validators[step % validators.len()]
    pub fn expected_proposer(step: u64, validators: &[Address]) -> Address {
        assert!(!validators.is_empty(), "validator list must not be empty");
        let idx = (step as usize) % validators.len();
        validators[idx]
    }
}
