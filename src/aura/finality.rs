use alloy_primitives::Address;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

const FINALITY_STATE_FILE: &str = "aura_finality_state.json";

/// Persisted subset of rolling finality state that must survive restarts.
/// Only the fields needed to correctly schedule finalizeChange() calls.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedFinalityState {
    pending_transitions: BTreeMap<u64, Address>,
    finalize_change_at: Option<(u64, Address)>,
}

/// Rolling finality tracker for AuRa consensus.
///
/// Tracks unique block signers to determine when a block becomes finalized.
/// A block is finalized when more than half of the current validator set has
/// signed blocks after it (geth rule: `sign_count * 2 > validator_count`).
///
/// When a finalized block has a pending InitiateChange event, the
/// finalizeChange() system call should be triggered at the next block.
#[derive(Debug, Clone)]
pub struct RollingFinality {
    /// Current validator set (addresses authorized to sign blocks).
    validators: Vec<Address>,
    /// Whether the validator set has been authoritatively set via `set_validators()`.
    /// When false, new block signers are auto-discovered and added to the set.
    /// When true, only known validators are counted (hasSigner check).
    validators_sealed: bool,
    /// Queue of unfinalized blocks: (block_number, signer_address).
    headers: VecDeque<(u64, Address)>,
    /// Count of blocks signed by each validator in the window.
    sign_count: BTreeMap<Address, u64>,
    /// Pending InitiateChange transitions: block_number -> validator_contract_address.
    /// When the block becomes finalized, finalizeChange should be called.
    pending_transitions: BTreeMap<u64, Address>,
    /// The block number at which finalization was most recently determined,
    /// meaning finalizeChange should be called at finalized_at + 1.
    finalize_change_at: Option<(u64, Address)>,
    /// Path to the datadir for persisting state across restarts.
    datadir: Option<PathBuf>,
}

impl RollingFinality {
    /// Create a new rolling finality tracker with the given validator set.
    pub fn new(validators: Vec<Address>) -> Self {
        Self {
            validators_sealed: false,
            validators,
            headers: VecDeque::new(),
            sign_count: BTreeMap::new(),
            pending_transitions: BTreeMap::new(),
            finalize_change_at: None,
            datadir: None,
        }
    }

    /// Set the datadir for persistence and load any previously saved state.
    pub fn with_datadir(mut self, datadir: impl Into<PathBuf>) -> Self {
        let datadir = datadir.into();
        if let Some(state) = Self::load_state(&datadir) {
            self.pending_transitions = state.pending_transitions;
            self.finalize_change_at = state.finalize_change_at;
            tracing::info!(
                target: "reth::gnosis",
                pending = self.pending_transitions.len(),
                finalize_at = ?self.finalize_change_at,
                "Restored rolling finality state from disk"
            );
        }
        self.datadir = Some(datadir);
        self
    }

    /// Persist the state-change-sensitive fields to disk.
    fn persist(&self) {
        if let Some(datadir) = &self.datadir {
            let state = PersistedFinalityState {
                pending_transitions: self.pending_transitions.clone(),
                finalize_change_at: self.finalize_change_at,
            };
            let path = datadir.join(FINALITY_STATE_FILE);
            match serde_json::to_string_pretty(&state) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(&path, json) {
                        tracing::warn!(
                            target: "reth::gnosis",
                            %e,
                            "Failed to persist rolling finality state"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        target: "reth::gnosis",
                        %e,
                        "Failed to serialize rolling finality state"
                    );
                }
            }
        }
    }

    /// Load persisted state from disk.
    fn load_state(datadir: &Path) -> Option<PersistedFinalityState> {
        let path = datadir.join(FINALITY_STATE_FILE);
        let data = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Returns true if any block in the queue is finalized
    /// (more than half of validators have signed).
    fn is_finalized(&self) -> bool {
        self.sign_count.len() * 2 > self.validators.len()
    }

    /// Push a new block with its signer. Returns any blocks that became finalized.
    pub fn push(&mut self, block_number: u64, signer: Address) -> Vec<(u64, Address)> {
        // Only count signers that are in the known validator set.
        // This matches geth's hasSigner() check — unknown signers are ignored
        // for finality purposes. This prevents pending (not-yet-active) validators
        // from inflating the validator count and shifting the finality threshold.
        if !self.validators.contains(&signer) {
            if !self.validators_sealed {
                // Auto-discover: set hasn't been authoritatively set yet (no
                // getValidators() call has completed). Add signers as they appear.
                // This happens during initial sync when execution starts mid-chain.
                self.validators.push(signer);
            } else {
                // Unknown signer — push to queue but don't count for finality
                self.headers.push_back((block_number, signer));
                return Vec::new();
            }
        }

        // Add signer count
        *self.sign_count.entry(signer).or_insert(0) += 1;
        self.headers.push_back((block_number, signer));

        // Pop finalized blocks
        let mut finalized = Vec::new();
        while self.is_finalized() {
            if let Some((num, addr)) = self.headers.pop_front() {
                // Decrease signer count
                if let Some(count) = self.sign_count.get_mut(&addr) {
                    *count -= 1;
                    if *count == 0 {
                        self.sign_count.remove(&addr);
                    }
                }
                finalized.push((num, addr));
            } else {
                break;
            }
        }

        // Check if any finalized block had a pending transition
        for (finalized_num, _) in &finalized {
            if let Some(contract_addr) = self.pending_transitions.remove(finalized_num) {
                // This finalized block had an InitiateChange event.
                // finalizeChange should be called at the NEXT block after the current one.
                tracing::info!(
                    target: "reth::gnosis",
                    pending_block = finalized_num,
                    current_block = block_number,
                    target_block = block_number + 1,
                    validator = %contract_addr,
                    window_size = self.headers.len(),
                    unique_signers = self.sign_count.len(),
                    "Pending transition finalized, scheduling finalizeChange"
                );
                self.finalize_change_at = Some((block_number + 1, contract_addr));
                self.persist();
            }
        }

        // Log pending transitions that haven't been finalized yet
        if !self.pending_transitions.is_empty() && block_number.is_multiple_of(5) {
            for pblock in self.pending_transitions.keys() {
                tracing::trace!(
                    target: "reth::gnosis",
                    pending_block = pblock,
                    current_block = block_number,
                    window_front = self.headers.front().map(|(n,_)| *n).unwrap_or(0),
                    window_back = self.headers.back().map(|(n,_)| *n).unwrap_or(0),
                    window_size = self.headers.len(),
                    unique_signers = self.sign_count.len(),
                    validators = self.validators.len(),
                    "Pending transition NOT YET finalized"
                );
            }
        }

        finalized
    }

    /// Set an immediate finalizeChange for the given block (pre-POSDAO).
    /// Skips the rolling finality check — calls finalizeChange at target_block directly.
    pub fn set_immediate_finalize(&mut self, target_block: u64, contract_address: Address) {
        self.finalize_change_at = Some((target_block, contract_address));
        self.persist();
    }

    /// Record a pending InitiateChange transition at the given block.
    pub fn add_pending_transition(&mut self, block_number: u64, contract_address: Address) {
        self.pending_transitions
            .insert(block_number, contract_address);
        self.persist();
    }

    /// Check if finalizeChange should be called at the given block number.
    /// Clears the finality window. The validator set will be refreshed by
    /// the caller via getValidators() after the finalizeChange system call.
    pub fn take_finalize_change(&mut self, block_number: u64) -> Option<Address> {
        if let Some((target_block, addr)) = self.finalize_change_at {
            if block_number >= target_block {
                self.finalize_change_at = None;
                // Clear the finality window. The caller refreshes the validator
                // set via getValidators() after the finalizeChange system call.
                self.headers.clear();
                self.sign_count.clear();
                self.persist();
                return Some(addr);
            }
        }
        None
    }

    /// Update the validator set (e.g., after getValidators() syscall).
    /// Seals the set — subsequent unknown signers will be rejected instead
    /// of auto-discovered.
    pub fn set_validators(&mut self, validators: Vec<Address>) {
        self.validators = validators;
        self.validators_sealed = true;
        // Clear the finality tracker since the validator set changed
        self.headers.clear();
        self.sign_count.clear();
    }

    /// Get the current validator count.
    pub fn validator_count(&self) -> usize {
        self.validators.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

    /// Build a sealed tracker (no auto-discover) with the given validators.
    fn sealed_tracker(validators: Vec<Address>) -> RollingFinality {
        let mut rf = RollingFinality::new(validators.clone());
        rf.set_validators(validators);
        rf
    }

    #[test]
    fn push_no_finalize_below_majority() {
        let mut rf = sealed_tracker(vec![addr(1), addr(2), addr(3), addr(4)]);
        let f1 = rf.push(1, addr(1));
        let f2 = rf.push(2, addr(2));
        assert!(f1.is_empty() && f2.is_empty(), "no finalization at 2/4");
    }

    #[test]
    fn push_finalizes_when_majority_unique_signers() {
        let mut rf = sealed_tracker(vec![addr(1), addr(2), addr(3), addr(4)]);
        rf.push(1, addr(1));
        rf.push(2, addr(2));
        let finalized = rf.push(3, addr(3));
        assert_eq!(finalized.len(), 1, "block 1 should now finalize");
        assert_eq!(finalized[0], (1, addr(1)));
    }

    #[test]
    fn push_does_not_double_count_same_signer() {
        let mut rf = sealed_tracker(vec![addr(1), addr(2), addr(3), addr(4)]);
        let _ = rf.push(1, addr(1));
        let _ = rf.push(2, addr(1));
        let f = rf.push(3, addr(1));
        assert!(
            f.is_empty(),
            "single signer cannot finalize 4-validator set"
        );
    }

    #[test]
    fn push_unknown_signer_skipped_in_sealed_set() {
        let mut rf = sealed_tracker(vec![addr(1), addr(2), addr(3), addr(4)]);
        rf.push(1, addr(1));
        rf.push(2, addr(2));
        let f = rf.push(3, addr(0xff));
        assert!(f.is_empty(), "unknown signer must not finalize");
        let f = rf.push(4, addr(3));
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn push_auto_discovers_until_sealed() {
        let mut rf = RollingFinality::new(Vec::new());
        rf.push(1, addr(1));
        rf.push(2, addr(2));
        assert_eq!(rf.validator_count(), 2, "should auto-discover");
        rf.set_validators(vec![addr(1), addr(2)]);
        rf.push(3, addr(0xff));
        assert_eq!(rf.validator_count(), 2, "must not grow after seal");
    }

    #[test]
    fn add_pending_transition_then_finalize_schedules_finalize_change() {
        let mut rf = sealed_tracker(vec![addr(1), addr(2), addr(3), addr(4)]);
        let validator_contract = addr(0xaa);
        rf.push(1, addr(1));
        rf.add_pending_transition(1, validator_contract);
        rf.push(2, addr(2));
        let _f = rf.push(3, addr(3));
        // After block 3, block 1 is finalized → finalizeChange scheduled at 3+1.
        assert_eq!(rf.take_finalize_change(4), Some(validator_contract));
        assert_eq!(rf.take_finalize_change(4), None);
    }

    #[test]
    fn take_finalize_change_blocks_before_target() {
        let mut rf = sealed_tracker(vec![addr(1)]);
        rf.set_immediate_finalize(100, addr(0xaa));
        assert_eq!(rf.take_finalize_change(50), None);
        assert_eq!(rf.take_finalize_change(99), None);
        assert_eq!(rf.take_finalize_change(100), Some(addr(0xaa)));
    }

    #[test]
    fn take_finalize_change_clears_window() {
        let mut rf = sealed_tracker(vec![addr(1), addr(2), addr(3), addr(4)]);
        rf.push(1, addr(1));
        rf.push(2, addr(2));
        rf.set_immediate_finalize(3, addr(0xaa));
        let _ = rf.take_finalize_change(3);
        let f = rf.push(3, addr(1));
        assert!(f.is_empty(), "window should have been cleared");
    }

    #[test]
    fn set_immediate_finalize_overrides_pending() {
        let mut rf = sealed_tracker(vec![addr(1)]);
        rf.add_pending_transition(10, addr(0xaa));
        rf.set_immediate_finalize(5, addr(0xbb));
        assert_eq!(rf.take_finalize_change(5), Some(addr(0xbb)));
    }

    #[test]
    fn validators_sealed_once_set_remains_sealed_after_clear() {
        let mut rf = RollingFinality::new(Vec::new());
        rf.push(1, addr(1));
        rf.set_validators(vec![addr(1), addr(2), addr(3)]);
        rf.set_immediate_finalize(2, addr(0xaa));
        let _ = rf.take_finalize_change(2);
        rf.push(3, addr(0xff));
        assert_eq!(rf.validator_count(), 3);
    }

    #[test]
    fn persist_and_load_round_trip() {
        // The disk-persisted state must round-trip exactly: pending_transitions
        // and finalize_change_at are the only fields that affect future
        // finalizeChange scheduling, and they must survive a restart.
        let dir = std::env::temp_dir().join(format!(
            "reth-gnosis-aura-persist-test-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);

        {
            let mut rf = RollingFinality::new(vec![addr(1), addr(2)]).with_datadir(dir.clone());
            rf.add_pending_transition(42, addr(0xaa));
            rf.set_immediate_finalize(50, addr(0xbb));
        }

        let rf2 = RollingFinality::new(vec![addr(1), addr(2)]).with_datadir(dir.clone());
        assert_eq!(rf2.pending_transitions.get(&42), Some(&addr(0xaa)));
        assert_eq!(rf2.finalize_change_at, Some((50, addr(0xbb))));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
