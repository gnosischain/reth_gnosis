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
    /// Block gas limit contract transitions: block_number -> contract_address.
    pub block_gas_limit_contract_transitions: BTreeMap<u64, Address>,
    /// POSDAO activation block.
    pub posdao_transition: Option<u64>,
    /// Pre-merge bytecode rewrites: block_number -> { contract_address -> new_bytecode }.
    /// Used by AuRa chains to upgrade contract bytecode at hardfork blocks
    /// (e.g., Gnosis token contract rewrite at block 21,735,000).
    pub rewrite_bytecode: BTreeMap<u64, BTreeMap<Address, Bytes>>,
}

/// Raw JSON structure for the "aura" section in chain spec.
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
    block_gas_limit_contract_transitions: Option<BTreeMap<StringNum, Address>>,
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
                RawValidatorSetKind::SafeContract { safe_contract } => {
                    ValidatorSetKind::SafeContract {
                        address: safe_contract,
                    }
                }
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

        // Build block gas limit contract transitions
        let mut block_gas_limit_contract_transitions = BTreeMap::new();
        if let Some(transitions) = raw.block_gas_limit_contract_transitions {
            for (block_num, addr) in transitions {
                block_gas_limit_contract_transitions.insert(block_num.0, addr);
            }
        }

        // Parse rewrite_bytecode
        let mut rewrite_bytecode = BTreeMap::new();
        if let Some(rewrites) = raw.rewrite_bytecode {
            for (block_num, contracts) in rewrites {
                rewrite_bytecode.insert(block_num.0, contracts);
            }
        }

        Ok(AuraConfig {
            step_duration: raw.step_duration,
            validators: ValidatorSet::new(sets),
            block_reward_contract_transitions,
            block_gas_limit_contract_transitions,
            posdao_transition: raw.posdao_transition,
            rewrite_bytecode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_minimal_with_list_validators() {
        let v = json!({
            "stepDuration": 5,
            "validators": {
                "multi": {
                    "0": {
                        "list": [
                            "0x0000000000000000000000000000000000000001",
                            "0x0000000000000000000000000000000000000002"
                        ]
                    }
                }
            }
        });
        let cfg = AuraConfig::from_json_value(&v).expect("parse must succeed");
        assert_eq!(cfg.step_duration, 5);
        assert!(cfg.posdao_transition.is_none());
        assert!(cfg.block_reward_contract_transitions.is_empty());
        assert!(cfg.block_gas_limit_contract_transitions.is_empty());
        assert!(cfg.rewrite_bytecode.is_empty());
        let list = cfg.validators.try_get_list_validators(0).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn parse_safe_contract_validator() {
        let v = json!({
            "stepDuration": 5,
            "validators": {
                "multi": {
                    "100": {
                        "safeContract": "0x00000000000000000000000000000000000000aa"
                    }
                }
            }
        });
        let cfg = AuraConfig::from_json_value(&v).unwrap();
        assert!(cfg.validators.try_get_list_validators(100).is_none());
        assert!(cfg.validators.contract_address_at(100).is_some());
    }

    #[test]
    fn parse_contract_validator() {
        let v = json!({
            "stepDuration": 5,
            "validators": {
                "multi": {
                    "200": {
                        "contract": "0x00000000000000000000000000000000000000bb"
                    }
                }
            }
        });
        let cfg = AuraConfig::from_json_value(&v).unwrap();
        assert!(cfg.validators.contract_address_at(200).is_some());
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
        assert_eq!(cfg.posdao_transition, Some(9186425));
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
    fn parse_block_reward_modern_transitions_map() {
        let v = json!({
            "stepDuration": 5,
            "validators": { "multi": { "0": { "list": ["0x0000000000000000000000000000000000000001"] } } },
            "blockRewardContractTransitions": {
                "1": "0x0000000000000000000000000000000000000010",
                "100": "0x0000000000000000000000000000000000000020",
                "9186425": "0x0000000000000000000000000000000000000030"
            }
        });
        let cfg = AuraConfig::from_json_value(&v).unwrap();
        assert_eq!(cfg.block_reward_contract_transitions.len(), 3);
        assert!(cfg.block_reward_contract_transitions.contains_key(&1));
        assert!(cfg.block_reward_contract_transitions.contains_key(&9186425));
    }

    #[test]
    fn parse_block_gas_limit_contract_transitions() {
        let v = json!({
            "stepDuration": 5,
            "validators": { "multi": { "0": { "list": ["0x0000000000000000000000000000000000000001"] } } },
            "blockGasLimitContractTransitions": {
                "1300": "0x0000000000000000000000000000000000000040"
            }
        });
        let cfg = AuraConfig::from_json_value(&v).unwrap();
        assert_eq!(cfg.block_gas_limit_contract_transitions.len(), 1);
    }

    #[test]
    fn parse_rewrite_bytecode() {
        // Real Gnosis: token contract upgrade at block 21,735,000.
        let v = json!({
            "stepDuration": 5,
            "validators": { "multi": { "0": { "list": ["0x0000000000000000000000000000000000000001"] } } },
            "rewriteBytecode": {
                "21735000": {
                    "0x6f1cef828f1bd5b6acff5d76d4a86d50f9a86998": "0xdeadbeef"
                }
            }
        });
        let cfg = AuraConfig::from_json_value(&v).unwrap();
        assert_eq!(cfg.rewrite_bytecode.len(), 1);
        let inner = cfg.rewrite_bytecode.get(&21735000).unwrap();
        assert_eq!(inner.len(), 1);
        let (_, code) = inner.iter().next().unwrap();
        assert_eq!(code.as_ref(), &[0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn parse_string_num_rejects_non_numeric() {
        // Block-number keys must be numeric strings; otherwise parsing must fail.
        let v = json!({
            "stepDuration": 5,
            "validators": { "multi": { "abc": { "list": ["0x0000000000000000000000000000000000000001"] } } }
        });
        assert!(AuraConfig::from_json_value(&v).is_err());
    }

    #[test]
    fn parse_unknown_validator_kind_fails() {
        let v = json!({
            "stepDuration": 5,
            "validators": { "multi": { "0": { "weirdKind": "0xff" } } }
        });
        assert!(AuraConfig::from_json_value(&v).is_err());
    }
}
