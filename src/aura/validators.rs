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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

    fn list_set(addrs: Vec<Address>) -> ValidatorSetKind {
        ValidatorSetKind::List(addrs)
    }

    fn safe_set(addr: Address) -> ValidatorSetKind {
        ValidatorSetKind::SafeContract { address: addr }
    }

    fn contract_set(addr: Address) -> ValidatorSetKind {
        ValidatorSetKind::Contract { address: addr }
    }

    #[test]
    #[should_panic(expected = "validator set must not be empty")]
    fn new_panics_on_empty_set() {
        ValidatorSet::new(BTreeMap::new());
    }

    #[test]
    fn kind_at_uses_highest_block_le_query() {
        // Three validator-set transitions: at blocks 0, 100, and 1000.
        // kind_at(N) returns the set whose activation block is the largest one <= N.
        let mut sets = BTreeMap::new();
        sets.insert(0, list_set(vec![addr(0xa)]));
        sets.insert(100, safe_set(addr(0xb)));
        sets.insert(1000, contract_set(addr(0xc)));
        let vs = ValidatorSet::new(sets);

        assert!(matches!(vs.kind_at(0), ValidatorSetKind::List(_)));
        assert!(matches!(vs.kind_at(99), ValidatorSetKind::List(_)));
        assert!(matches!(
            vs.kind_at(100),
            ValidatorSetKind::SafeContract { .. }
        ));
        assert!(matches!(
            vs.kind_at(999),
            ValidatorSetKind::SafeContract { .. }
        ));
        assert!(matches!(
            vs.kind_at(1000),
            ValidatorSetKind::Contract { .. }
        ));
        assert!(matches!(
            vs.kind_at(u64::MAX),
            ValidatorSetKind::Contract { .. }
        ));
    }

    #[test]
    fn try_get_list_validators_returns_some_for_list() {
        let mut sets = BTreeMap::new();
        let validators = vec![addr(1), addr(2), addr(3)];
        sets.insert(0, list_set(validators.clone()));
        let vs = ValidatorSet::new(sets);

        let got = vs
            .try_get_list_validators(0)
            .expect("list set must return Some");
        assert_eq!(got, validators.as_slice());
    }

    #[test]
    fn try_get_list_validators_returns_none_for_contract_kinds() {
        let mut sets = BTreeMap::new();
        sets.insert(0, list_set(vec![addr(1)]));
        sets.insert(10, safe_set(addr(0xb)));
        sets.insert(20, contract_set(addr(0xc)));
        let vs = ValidatorSet::new(sets);

        assert!(
            vs.try_get_list_validators(15).is_none(),
            "SafeContract -> None"
        );
        assert!(vs.try_get_list_validators(25).is_none(), "Contract -> None");
    }

    #[test]
    fn contract_address_at_returns_none_for_list_some_for_contract() {
        let mut sets = BTreeMap::new();
        sets.insert(0, list_set(vec![addr(1)]));
        sets.insert(10, safe_set(addr(0xb)));
        sets.insert(20, contract_set(addr(0xc)));
        let vs = ValidatorSet::new(sets);

        assert_eq!(vs.contract_address_at(5), None);
        assert_eq!(vs.contract_address_at(10), Some(addr(0xb)));
        assert_eq!(vs.contract_address_at(15), Some(addr(0xb)));
        assert_eq!(vs.contract_address_at(20), Some(addr(0xc)));
    }

    #[test]
    fn expected_proposer_round_robin() {
        let v = vec![addr(0xa), addr(0xb), addr(0xc)];
        assert_eq!(ValidatorSet::expected_proposer(0, &v), v[0]);
        assert_eq!(ValidatorSet::expected_proposer(1, &v), v[1]);
        assert_eq!(ValidatorSet::expected_proposer(2, &v), v[2]);
        assert_eq!(ValidatorSet::expected_proposer(3, &v), v[0]); // wrap
        assert_eq!(ValidatorSet::expected_proposer(7, &v), v[1]);
    }

    #[test]
    fn expected_proposer_single_validator() {
        let v = vec![addr(0xff)];
        assert_eq!(ValidatorSet::expected_proposer(0, &v), v[0]);
        assert_eq!(ValidatorSet::expected_proposer(u64::MAX, &v), v[0]);
    }

    #[test]
    fn expected_proposer_realistic_chiado_step() {
        // Chiado block 100000 has step 332890827. With a 4-validator set, this
        // selects validators[332890827 % 4] = validators[3].
        let v = vec![addr(0x1), addr(0x2), addr(0x3), addr(0x4)];
        let proposer = ValidatorSet::expected_proposer(332890827, &v);
        assert_eq!(proposer, v[3], "step 332890827 % 4 == 3");
    }

    #[test]
    #[should_panic(expected = "validator list must not be empty")]
    fn expected_proposer_panics_on_empty() {
        let _ = ValidatorSet::expected_proposer(0, &[]);
    }

    #[test]
    fn kind_at_with_address_literal_compiles() {
        // Smoke test that the address! macro path through alloy_primitives works
        // for the test setup pattern used in other tests.
        let mut sets = BTreeMap::new();
        sets.insert(
            0,
            list_set(vec![address!("0x60f1cf46b42df059b98acf67c1dd7771b100e124")]),
        );
        let vs = ValidatorSet::new(sets);
        if let ValidatorSetKind::List(addrs) = vs.kind_at(0) {
            assert_eq!(addrs.len(), 1);
        } else {
            panic!("expected List");
        }
    }
}
