//! Receipt-replay reconstruction of [`RollingFinality`] state at startup.
//!
//! Inspired by Nethermind's `ContractBasedValidator.TryGetInitChangeFromPastBlocks`:
//! walk back `reconstruction_lookback()` blocks from `head`, rebuild
//! `pending_transitions` / `finalize_change_at` by replaying the rolling-finality
//! state machine over historical headers + receipts.

use std::collections::BTreeMap;

use alloy_eips::BlockHashOrNumber;
use alloy_primitives::Address;
use gnosis_primitives::header::GnosisHeader;

use crate::aura::finality::RollingFinality;

/// keccak256("InitiateChange(bytes32,address[])")
pub const INITIATE_CHANGE_TOPIC: alloy_primitives::B256 =
    alloy_primitives::b256!("0x55252fa6eee4741b4e24a74a70e9c11fd2c2281df8d6ea13126ff845f7825c89");

/// How many blocks of receipt history to scan when reconstructing
/// [`RollingFinality`] state at startup.
///
/// Tuned against the full Gnosis pre-merge POSDAO replay (144 events, max
/// observed `k = 9`). `32` keeps ~3.5× margin and fits well inside reth's
/// `--minimal` receipt retention (`Distance(64)`).
pub const fn reconstruction_lookback() -> u64 {
    32
}

/// Read-only chain access for [`reconstruct_finality_state`].
pub trait ChainScanner: Send + Sync + std::fmt::Debug {
    fn header_by_number(&self, n: u64) -> Option<GnosisHeader>;
    fn receipts_by_block_number(&self, n: u64) -> Option<Vec<reth_ethereum_primitives::Receipt>>;
}

/// Rebuild [`RollingFinality`] state for `head_block` by replaying receipts.
///
/// `validator_contract == None` means the active set is `List`-typed; no
/// `InitiateChange` is possible and an empty tracker is returned. The result's
/// `validators` is intentionally empty — the next live block triggers a
/// `getValidators()` refresh from the authoritative contract.
pub fn reconstruct_finality_state<S: ChainScanner>(
    scanner: &S,
    head_block: u64,
    validator_contract: Option<Address>,
    posdao_transition: u64,
) -> RollingFinality {
    let Some(contract) = validator_contract else {
        return RollingFinality::from_recovered(BTreeMap::new(), None);
    };

    let lookback = reconstruction_lookback();
    let start = head_block.saturating_sub(lookback - 1);

    tracing::info!(
        target: "reth::gnosis",
        head_block,
        lookback,
        validator_contract = %contract,
        posdao_transition,
        "AuRa recovery: replaying receipts to rebuild rolling-finality state"
    );

    // Pass 1: collect unique signers, used as the sealed validator set so the
    // rolling-finality threshold is sized correctly during simulation.
    let mut discovered: Vec<Address> = Vec::new();
    for n in start..=head_block {
        if let Some(header) = scanner.header_by_number(n) {
            if !discovered.contains(&header.beneficiary) {
                discovered.push(header.beneficiary);
            }
        }
    }

    // Pass 2: simulate take→scan→push in the same order as live execution
    // (context_for_block(n).take_finalize_change(n), then finish() pushes the
    // signer). Mismatched ordering would silently shift finalization timing.
    let mut sim = RollingFinality::new(discovered.clone());
    sim.set_validators(discovered);

    for n in start..=head_block {
        let _ = sim.take_finalize_change(n);

        let Some(header) = scanner.header_by_number(n) else {
            continue;
        };
        let receipts = scanner.receipts_by_block_number(n).unwrap_or_default();

        if receipts_contain_initiate_change(&receipts, contract) {
            if n >= posdao_transition {
                sim.add_pending_transition(n, contract);
            } else {
                sim.set_immediate_finalize(n + 1, contract);
            }
        }

        sim.push(n, header.beneficiary);
    }

    RollingFinality::from_recovered(sim.pending_transitions().clone(), sim.finalize_change_at())
}

/// True iff any receipt contains an `InitiateChange` event from `validator_contract`.
pub fn receipts_contain_initiate_change(
    receipts: &[reth_ethereum_primitives::Receipt],
    validator_contract: Address,
) -> bool {
    use alloy_consensus::TxReceipt;
    receipts.iter().any(|receipt| {
        receipt.logs().iter().any(|log| {
            log.address == validator_contract
                && log.topics().first() == Some(&INITIATE_CHANGE_TOPIC)
        })
    })
}

/// Adapter exposing any reth provider with the required read methods as a
/// [`ChainScanner`].
#[derive(Debug, Clone)]
pub struct ProviderChainScanner<P> {
    provider: P,
}

impl<P> ProviderChainScanner<P> {
    pub fn new(provider: P) -> Self {
        Self { provider }
    }
}

impl<P> ChainScanner for ProviderChainScanner<P>
where
    P: reth_storage_api::HeaderProvider<Header = GnosisHeader>
        + reth_storage_api::ReceiptProvider<Receipt = reth_ethereum_primitives::Receipt>
        + Send
        + Sync
        + std::fmt::Debug,
{
    fn header_by_number(&self, n: u64) -> Option<GnosisHeader> {
        self.provider.header_by_number(n).ok().flatten()
    }
    fn receipts_by_block_number(&self, n: u64) -> Option<Vec<reth_ethereum_primitives::Receipt>> {
        self.provider
            .receipts_by_block(BlockHashOrNumber::Number(n))
            .ok()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Bytes, LogData};
    use reth_ethereum_primitives::Receipt;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

    fn receipt_with_initiate_change(from: Address) -> Receipt {
        Receipt {
            tx_type: reth_ethereum_primitives::TxType::Legacy,
            success: true,
            cumulative_gas_used: 0,
            logs: vec![alloy_primitives::Log {
                address: from,
                data: LogData::new_unchecked(vec![INITIATE_CHANGE_TOPIC], Bytes::new()),
            }],
        }
    }

    #[derive(Debug, Default)]
    struct MockScanner {
        headers: Mutex<HashMap<u64, GnosisHeader>>,
        receipts: Mutex<HashMap<u64, Vec<Receipt>>>,
    }

    impl MockScanner {
        fn put(&self, n: u64, beneficiary: Address, receipts: Vec<Receipt>) {
            self.headers.lock().unwrap().insert(
                n,
                GnosisHeader {
                    number: n,
                    beneficiary,
                    ..Default::default()
                },
            );
            self.receipts.lock().unwrap().insert(n, receipts);
        }
    }

    impl ChainScanner for MockScanner {
        fn header_by_number(&self, n: u64) -> Option<GnosisHeader> {
            self.headers.lock().unwrap().get(&n).cloned()
        }
        fn receipts_by_block_number(&self, n: u64) -> Option<Vec<Receipt>> {
            self.receipts.lock().unwrap().get(&n).cloned()
        }
    }

    #[test]
    fn returns_empty_state_when_no_validator_contract_active() {
        let scanner = MockScanner::default();
        let mut rf = reconstruct_finality_state(&scanner, 100, None, 0);
        assert!(rf.pending_transitions().is_empty());
        assert_eq!(rf.finalize_change_at(), None);
        assert_eq!(rf.take_finalize_change(101), None);
    }

    #[test]
    fn pre_posdao_event_at_head_schedules_finalize_change() {
        let scanner = MockScanner::default();
        let contract = addr(0xaa);
        for n in 1..=100 {
            let receipts = if n == 100 {
                vec![receipt_with_initiate_change(contract)]
            } else {
                vec![]
            };
            scanner.put(n, addr(1), receipts);
        }
        let mut rf = reconstruct_finality_state(&scanner, 100, Some(contract), 1000);
        assert_eq!(rf.take_finalize_change(101), Some(contract));
    }

    #[test]
    fn unresolved_posdao_event_surfaces_in_recovered_state() {
        let scanner = MockScanner::default();
        let contract = addr(0xaa);
        // Varied signers so the discovered validator set is large enough that
        // a single push at block 100 doesn't trip the finality threshold.
        for n in 1..=100 {
            let signer = addr((n % 7 + 1) as u8);
            let receipts = if n == 100 {
                vec![receipt_with_initiate_change(contract)]
            } else {
                vec![]
            };
            scanner.put(n, signer, receipts);
        }
        let rf = reconstruct_finality_state(&scanner, 100, Some(contract), 0);
        let recovered_state_nonempty =
            !rf.pending_transitions().is_empty() || rf.finalize_change_at().is_some();
        assert!(recovered_state_nonempty);
    }

    #[test]
    fn events_older_than_lookback_are_not_recovered() {
        let scanner = MockScanner::default();
        let contract = addr(0xaa);
        let lookback = reconstruction_lookback();
        let event_block = 5;
        let head = event_block + lookback + 50;
        for n in 1..=head {
            let receipts = if n == event_block {
                vec![receipt_with_initiate_change(contract)]
            } else {
                vec![]
            };
            scanner.put(n, addr(1), receipts);
        }
        let mut rf = reconstruct_finality_state(&scanner, head, Some(contract), 0);
        assert!(rf.pending_transitions().is_empty());
        assert_eq!(rf.take_finalize_change(head + 1), None);
    }
}
