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
