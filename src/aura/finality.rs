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
        if !self.pending_transitions.is_empty() && block_number % 5 == 0 {
            for (pblock, _) in &self.pending_transitions {
                tracing::debug!(
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
