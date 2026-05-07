use alloy_primitives::{Address, Bytes};
use serde::Deserialize;
use std::collections::BTreeMap;

use super::validators::{ValidatorSet, ValidatorSetKind};

/// AuRa consensus configuration parsed from the chain spec genesis JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraConfig {
    /// Duration of each step in seconds.
    pub step_duration: u64,
    /// Validator set configuration.
    pub validators: ValidatorSet,
    /// Block reward contract transitions: block_number -> contract_address.
    pub block_reward_contract_transitions: BTreeMap<u64, Address>,
    /// POSDAO activation block. Required in genesis for AuRa chains —
    /// both we ship (Gnosis, Chiado) supply this, so an absent
    /// `posdaoTransition` is treated as a chain-spec misconfig and the
    /// parser errors out.
    pub posdao_transition: u64,
    /// Pre-merge bytecode rewrites: block_number -> { contract_address -> new_bytecode }.
    /// Used by AuRa chains to upgrade contract bytecode at hardfork blocks
    /// (e.g., Gnosis token contract rewrite at block 21,735,000).
    pub rewrite_bytecode: BTreeMap<u64, BTreeMap<Address, Bytes>>,
}

/// Raw JSON structure for the "aura" section in chain spec.
///
/// Nethermind's `AuRaChainSpecEngineParameters` defines a superset of fields.
/// This struct covers everything the Gnosis-supported AuRa chains (Gnosis,
/// Chiado) actually use. Fields below are the ones we **don't** parse, with
/// the rationale per field — Gnosis is fully post-merge and will never run
/// AuRa again, so historical-sync is the only thing reth_gnosis cares about
/// and these fields are not load-bearing for replaying historical blocks:
///
/// - `blockReward` — fixed-amount reward transitions. Gnosis uses contract-
///   based rewards (`blockRewardContractTransitions`), not flat amounts.
/// - `maximumUncleCount` / `maximumUncleCountTransition` — AuRa disallows
///   uncles, so the count is always 0.
/// - `validateScoreTransition` / `validateStepTransition` — gating block
///   numbers below which difficulty / step monotonicity isn't checked. We
///   always validate; the reference chains don't gate.
/// - `randomnessContractAddress` — POSDAO randomness for block production.
///   Sync-only clients don't produce blocks.
/// - `twoThirdsMajorityTransition` — switches finality threshold from 1/2
///   to 2/3 at this block. Not active on Gnosis or Chiado in the synced
///   range; would matter only on a chain that activated it.
/// - `rewriteBytecodeTimestamp` — bytecode rewrite indexed by timestamp
///   instead of block number. Only `rewriteBytecode` (block-number-keyed)
///   is in use on Gnosis (for AuRa).
/// - `withdrawalContractAddress` — post-Shanghai withdrawal contract. We
///   resolve the equivalent address through the chain spec elsewhere.
///
/// Serde silently drops unknown fields, so adding any of these to a genesis
/// JSON simply means we ignore them. If a future chain spec relies on them,
/// the appropriate fix is to extend this struct rather than work around its
/// absence at call sites.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAuraConfig {
    step_duration: u64,
    validators: RawValidators,
    #[serde(default)]
    block_reward_contract_address: Option<Address>,
    #[serde(default)]
    block_reward_contract_transition: Option<u64>,
    #[serde(default)]
    block_reward_contract_transitions: Option<BTreeMap<StringNum, Address>>,
    #[serde(default)]
    posdao_transition: Option<u64>,
    #[serde(default)]
    rewrite_bytecode: Option<BTreeMap<StringNum, BTreeMap<Address, Bytes>>>,
}

/// Wrapper for block numbers that may appear as strings in JSON.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StringNum(u64);

impl<'de> Deserialize<'de> for StringNum {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<u64>()
            .map(StringNum)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Deserialize)]
struct RawValidators {
    multi: BTreeMap<StringNum, RawValidatorSetKind>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawValidatorSetKind {
    List {
        list: Vec<Address>,
    },
    SafeContract {
        #[serde(rename = "safeContract")]
        safe_contract: Address,
    },
    Contract {
        contract: Address,
    },
}

impl AuraConfig {
    /// Parse AuRa config from the genesis JSON "aura" field.
    pub fn from_json_value(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        let raw: RawAuraConfig = serde_json::from_value(value.clone())?;

        // Parse validator sets
        let mut sets = BTreeMap::new();
        for (block_num, kind) in raw.validators.multi {
            let validator_kind = match kind {
                RawValidatorSetKind::List { list } => ValidatorSetKind::List(list),
                // Both JSON forms collapse to one Rust variant — see the
                // doc on `ValidatorSetKind` for why we don't track Nethermind's
                // SafeContract/Contract distinction.
                RawValidatorSetKind::SafeContract { safe_contract } => ValidatorSetKind::Contract {
                    address: safe_contract,
                },
                RawValidatorSetKind::Contract { contract } => {
                    ValidatorSetKind::Contract { address: contract }
                }
            };
            sets.insert(block_num.0, validator_kind);
        }

        // Build block reward contract transitions
        let mut block_reward_contract_transitions = BTreeMap::new();
        if let Some(addr) = raw.block_reward_contract_address {
            let transition = raw.block_reward_contract_transition.unwrap_or(0);
            block_reward_contract_transitions.insert(transition, addr);
        }
        if let Some(transitions) = raw.block_reward_contract_transitions {
            for (block_num, addr) in transitions {
                block_reward_contract_transitions.insert(block_num.0, addr);
            }
        }

        // Parse rewrite_bytecode
        let mut rewrite_bytecode = BTreeMap::new();
        if let Some(rewrites) = raw.rewrite_bytecode {
            for (block_num, contracts) in rewrites {
                rewrite_bytecode.insert(block_num.0, contracts);
            }
        }

        let validators = ValidatorSet::new(sets).map_err(serde::de::Error::custom)?;

        let posdao_transition = raw.posdao_transition.ok_or_else(|| {
            serde::de::Error::custom(
                "AuRa chain spec must include `posdaoTransition` (use a far-future block \
                 number if POSDAO never activates on this chain)",
            )
        })?;

        Ok(AuraConfig {
            step_duration: raw.step_duration,
            validators,
            block_reward_contract_transitions,
            posdao_transition,
            rewrite_bytecode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_contract_validator_field_name_aliases() {
        // Both `safeContract` and `contract` JSON field names map to the single
        // `Contract` variant — the parser must accept both spellings.
        for field in ["safeContract", "contract"] {
            let v = json!({
                "stepDuration": 5,
                "posdaoTransition": 0,
                "validators": {
                    "multi": {
                        "100": { field: "0x00000000000000000000000000000000000000aa" }
                    }
                }
            });
            let cfg = AuraConfig::from_json_value(&v)
                .unwrap_or_else(|e| panic!("field `{field}` must parse: {e}"));
            assert!(cfg.validators.try_get_list_validators(100).is_none());
            assert!(cfg.validators.contract_address_at(100).is_some());
        }
    }

    #[test]
    fn parse_multi_transitions_in_order() {
        // List at 0, SafeContract at 1300, Contract at 9186425 (POSDAO-style).
        let v = json!({
            "stepDuration": 5,
            "validators": {
                "multi": {
                    "0": { "list": ["0x0000000000000000000000000000000000000001"] },
                    "1300": { "safeContract": "0x00000000000000000000000000000000000000aa" },
                    "9186425": { "contract": "0x00000000000000000000000000000000000000bb" }
                }
            },
            "posdaoTransition": 9186425
        });
        let cfg = AuraConfig::from_json_value(&v).unwrap();
        assert_eq!(cfg.posdao_transition, 9186425);
        assert!(cfg.validators.try_get_list_validators(1).is_some());
        assert!(cfg.validators.try_get_list_validators(1299).is_some());
        assert!(cfg.validators.try_get_list_validators(1300).is_none());
        assert!(cfg.validators.contract_address_at(1300).is_some());
        // Different addresses for SafeContract (1300+) and Contract (9186425+).
        assert_ne!(
            cfg.validators.contract_address_at(1300),
            cfg.validators.contract_address_at(9186425)
        );
    }

    #[test]
    fn parse_block_reward_legacy_single_address() {
        // Old format: blockRewardContractAddress + blockRewardContractTransition.
        let v = json!({
            "stepDuration": 5,
            "posdaoTransition": 0,
            "validators": { "multi": { "0": { "list": ["0x0000000000000000000000000000000000000001"] } } },
            "blockRewardContractAddress": "0x000000000000000000000000000000000000beef",
            "blockRewardContractTransition": 100
        });
        let cfg = AuraConfig::from_json_value(&v).unwrap();
        assert_eq!(cfg.block_reward_contract_transitions.len(), 1);
        let (block, _addr) = cfg.block_reward_contract_transitions.iter().next().unwrap();
        assert_eq!(*block, 100);
    }

    #[test]
    fn parse_block_reward_legacy_address_no_transition_defaults_zero() {
        let v = json!({
            "stepDuration": 5,
            "posdaoTransition": 0,
            "validators": { "multi": { "0": { "list": ["0x0000000000000000000000000000000000000001"] } } },
            "blockRewardContractAddress": "0x000000000000000000000000000000000000beef"
        });
        let cfg = AuraConfig::from_json_value(&v).unwrap();
        assert_eq!(
            cfg.block_reward_contract_transitions
                .get(&0)
                .copied()
                .is_some(),
            true
        );
    }

    #[test]
    fn parse_missing_posdao_transition_errors() {
        // Both AuRa chains we ship include `posdaoTransition`; absence is a
        // chain-spec misconfig and the parser must surface it loudly.
        let v = json!({
            "stepDuration": 5,
            "validators": { "multi": { "0": { "list": ["0x0000000000000000000000000000000000000001"] } } }
        });
        let err = AuraConfig::from_json_value(&v).unwrap_err();
        assert!(err.to_string().contains("posdaoTransition"));
    }
}
