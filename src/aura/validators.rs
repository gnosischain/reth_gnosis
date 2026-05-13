use alloy_primitives::Address;
use std::collections::BTreeMap;

/// A single validator set type.
///
/// Nethermind distinguishes two contract-based kinds: `safeContract`
/// (`getValidators()` only) and `contract` (also calls `reportMalicious()`/
/// `reportBenign()` on observed misbehavior). Reporting is a *block-producer*
/// concern — a sync-only client doesn't originate reports; historical reports
/// are baked into the chain as ordinary transactions and replay automatically.
/// reth_gnosis is sync-only for AuRa (the chains are post-merge and will never
/// produce new AuRa blocks), so we collapse both into one variant. The JSON
/// parser still accepts both `safeContract` and `contract` field names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidatorSetKind {
    /// Static list of validators.
    List(Vec<Address>),
    /// Validators resolved from a contract via `getValidators()` syscall.
    /// Covers both Nethermind's `safeContract` and `contract` variants.
    Contract { address: Address },
}

/// Multi-validator set: transitions between different validator sets at specific block numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatorSet {
    /// Sorted by block number. Each entry: (activation_block, validator_set_kind)
    sets: BTreeMap<u64, ValidatorSetKind>,
}

/// Errors returned by [`ValidatorSet`] operations.
///
/// AuRa is a pre-merge–only mechanism — no new AuRa blocks will ever be
/// produced — so consensus code must not panic on adversarial or malformed
/// input. Callers propagate these errors as `ConsensusError`s instead.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidatorSetError {
    /// `ValidatorSet::new` was called with an empty map.
    #[error("validator set must not be empty")]
    Empty,
}

impl ValidatorSet {
    /// Create a new multi-validator set. Returns [`ValidatorSetError::Empty`]
    /// if `sets` is empty (a chain spec missing all validator transitions
    /// would be malformed; we surface it as an error rather than panicking).
    pub fn new(sets: BTreeMap<u64, ValidatorSetKind>) -> Result<Self, ValidatorSetError> {
        if sets.is_empty() {
            return Err(ValidatorSetError::Empty);
        }
        Ok(Self { sets })
    }

    /// Returns the validator set kind active at `block_number` (highest
    /// activation block ≤ N), or `None` if no entry covers it. With a
    /// well-formed chain spec there is always an entry at block 0, so this
    /// only returns `None` for malformed input.
    pub fn kind_at(&self, block_number: u64) -> Option<&ValidatorSetKind> {
        self.sets
            .range(..=block_number)
            .next_back()
            .map(|(_, kind)| kind)
    }

    /// Static-list validators active at `block_number`, if the active set
    /// is list-typed. `None` if the set is contract-typed (caller must
    /// resolve via EVM state) or if no set covers this block.
    pub fn try_get_list_validators(&self, block_number: u64) -> Option<&[Address]> {
        match self.kind_at(block_number)? {
            ValidatorSetKind::List(addrs) => Some(addrs),
            ValidatorSetKind::Contract { .. } => None,
        }
    }

    /// Contract address for contract-based validator sets. `None` if the
    /// active set is list-typed or if no set covers this block.
    pub fn contract_address_at(&self, block_number: u64) -> Option<Address> {
        match self.kind_at(block_number)? {
            ValidatorSetKind::List(_) => None,
            ValidatorSetKind::Contract { address } => Some(*address),
        }
    }

    /// Round-robin proposer for the given step. Returns `None` if the
    /// validator list is empty (which would be a malformed chain spec).
    ///
    /// The modulo runs in `u64` then narrows to `usize`, so the result is
    /// independent of host bit-width — `step as usize` would truncate to
    /// 32 bits on 32-bit targets and produce a wrong index.
    pub fn expected_proposer(step: u64, validators: &[Address]) -> Option<Address> {
        let len = validators.len();
        if len == 0 {
            return None;
        }
        let idx = (step % len as u64) as usize;
        Some(validators[idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

    #[test]
    fn new_errors_on_empty_set() {
        assert_eq!(
            ValidatorSet::new(BTreeMap::new()),
            Err(ValidatorSetError::Empty)
        );
    }

    /// End-to-end: build a multi-transition spec (List → Contract A → Contract B)
    /// and check `kind_at`, `try_get_list_validators`, `contract_address_at` all
    /// dispatch correctly across the boundaries — including the inclusive-edge
    /// behavior of `..=block_number` (transition block belongs to the new set).
    #[test]
    fn lookups_across_transitions() {
        let listed = vec![addr(1), addr(2), addr(3)];
        let contract_a = addr(0xaa);
        let contract_b = addr(0xbb);

        let mut sets = BTreeMap::new();
        sets.insert(0, ValidatorSetKind::List(listed.clone()));
        sets.insert(
            100,
            ValidatorSetKind::Contract {
                address: contract_a,
            },
        );
        sets.insert(
            1000,
            ValidatorSetKind::Contract {
                address: contract_b,
            },
        );
        let vs = ValidatorSet::new(sets).unwrap();

        // List region [0, 100): kind_at = List, try_get_list = Some, contract = None.
        for &b in &[0u64, 1, 99] {
            assert!(matches!(vs.kind_at(b), Some(ValidatorSetKind::List(_))));
            assert_eq!(vs.try_get_list_validators(b), Some(listed.as_slice()));
            assert_eq!(vs.contract_address_at(b), None);
        }

        // Contract A region [100, 1000): boundary inclusive, contract = A.
        for &b in &[100u64, 999] {
            assert!(vs.try_get_list_validators(b).is_none());
            assert_eq!(vs.contract_address_at(b), Some(contract_a));
        }

        // Contract B region [1000, ∞).
        for &b in &[1000u64, u64::MAX] {
            assert_eq!(vs.contract_address_at(b), Some(contract_b));
        }
    }

    #[test]
    fn kind_at_returns_none_below_first_entry() {
        // Defensive: with no block-0 entry, lookups below the first activation
        // must return None (not panic). Real chain specs always have a block-0
        // entry, but ValidatorSet doesn't enforce that — pin the behavior.
        let mut sets = BTreeMap::new();
        sets.insert(100, ValidatorSetKind::List(vec![addr(0xa)]));
        let vs = ValidatorSet::new(sets).unwrap();
        assert!(vs.kind_at(0).is_none());
        assert!(vs.kind_at(99).is_none());
        assert!(vs.kind_at(100).is_some());
    }

    #[test]
    fn expected_proposer_round_robin() {
        let v = vec![addr(0xa), addr(0xb), addr(0xc)];
        assert_eq!(ValidatorSet::expected_proposer(0, &v), Some(v[0]));
        assert_eq!(ValidatorSet::expected_proposer(2, &v), Some(v[2]));
        assert_eq!(ValidatorSet::expected_proposer(3, &v), Some(v[0])); // wrap
        assert_eq!(ValidatorSet::expected_proposer(7, &v), Some(v[1]));
        // Empty list returns None — must not panic on indexing.
        assert_eq!(ValidatorSet::expected_proposer(0, &[]), None);
    }
}
