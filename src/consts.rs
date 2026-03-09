use std::collections::HashMap;

use alloy_primitives::{Address, Bytes};
use revm_primitives::{hex::FromHex, KECCAK_EMPTY};
use revm_state::Bytecode;
use serde_json::{self, Value};

use crate::spec::gnosis_spec::BalancerHardforkConfig;

pub fn parse_balancer_hardfork_config(
    time_value: Option<&Value>,
    config_value: Option<&Value>,
) -> Option<BalancerHardforkConfig> {
    if time_value.is_none() || config_value.is_none() {
        return None;
    };

    let activation_time = serde_json::from_value::<u64>(time_value.unwrap().clone())
        .expect("Error parsing balancerHardforkTime");

    let parsed_mapping =
        serde_json::from_value::<HashMap<Address, String>>(config_value.unwrap().clone())
            .expect("Error parsing balancerHardforkBytecodes");

    let config = parsed_mapping
        .into_iter()
        .map(|(addr, bytecode_str)| {
            let bytecode = if !bytecode_str.is_empty() {
                Some(Bytecode::new_legacy(
                    Bytes::from_hex(bytecode_str)
                        .unwrap_or_else(|_| panic!("Unable to parse bytecode hex for {addr}")),
                ))
            } else {
                None
            };
            let codehash = match &bytecode {
                Some(code) => code.hash_slow(),
                None => KECCAK_EMPTY,
            };
            (addr, bytecode, codehash)
        })
        .collect();

    Some(BalancerHardforkConfig {
        activation_time,
        config,
    })
}
