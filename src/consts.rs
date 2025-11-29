use std::{any::Any, collections::HashMap};

use alloy_consensus::{EthereumTxEnvelope, TxEip4844, TxEip7702};
use alloy_primitives::{address, Address, Bytes};
use reth_transaction_pool::error::PoolTransactionError;
use revm_primitives::{hex::FromHex, KECCAK_EMPTY};
use revm_state::Bytecode;
use serde_json::{self, Value};

use crate::spec::gnosis_spec::BalancerHardforkConfig;

const BLACKLIST_SENDERS_COUNT: usize = 2;
pub const BLACKLIST_SENDERS: [Address; BLACKLIST_SENDERS_COUNT] = [
    // Example blacklisted address
    address!("0x506d1f9efe24f0d47853adca907eb8d89ae03207"),
    address!("0x491837cc85bbeab5f9b3110ad61f39d87f8ec618"),
    // address!("0x41FAb0BB658EF4c3f76AbD8Ee5bca4611f7478d0"),
];

const BLACKLIST_CONTRACT_ADDRESSES_COUNT: usize = 5;
pub const BLACKLIST_CONTRACT_ADDRESSES: [Address; BLACKLIST_CONTRACT_ADDRESSES_COUNT] = [
    address!("0x5e7FA86cfdD10de6129e53377335b78BB34eaBD3"),
    address!("0x234490fA3Cd6C899681C8E93Ba88e97183a71FE4"),
    address!("0x49b5CE67B22b1D596842ca071ac3dA93eE593E11"),
    address!("0x7b23c07A0BbBe652Bf7069c9c4143a2C85132166"),
    address!("0x1Bdc1FebebF92BfFab3a2E49C5cF3B7e35a9E81E"),
    // address!("0x0eA9cACa364E352360EA241136c88867D63b93cB"),
    // address!("0x413cFF89C3f59F900BD9e36336543F4AEFfc2e54"),
];

pub fn is_sender_blacklisted(sender: &Address) -> bool {
    BLACKLIST_SENDERS.contains(sender)
}

pub fn is_to_address_blacklisted(address: &Address) -> bool {
    BLACKLIST_CONTRACT_ADDRESSES.contains(address)
}

/// Gnosis-specific transaction pool validation errors
#[derive(Debug, thiserror::Error)]
pub enum GnosisError {
    /// Custom error message for Gnosis-specific validation
    #[error("{message}")]
    CustomValidation { message: String },
}

impl PoolTransactionError for GnosisError {
    fn is_bad_transaction(&self) -> bool {
        match self {
            Self::CustomValidation { .. } => false, // Could be environmental
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// Helper function to create a pool error with Gnosis-specific validation error
impl GnosisError {
    /// Creates a new custom validation error
    pub fn custom(message: impl Into<String>) -> Self {
        Self::CustomValidation {
            message: message.into(),
        }
    }
}

// filter to use:
// is_sender_blacklisted(&sender)
//   || is_to_address_blacklisted(&to)
//   || is_blacklisted_setcode(&pool_tx.transaction.clone().into_consensus())
pub fn is_blacklisted_setcode(tx: &EthereumTxEnvelope<TxEip4844>) -> bool {
    match tx {
        EthereumTxEnvelope::Eip7702(signed_tx) => {
            let TxEip7702 {
                authorization_list, ..
            } = signed_tx.tx();
            for auth in authorization_list {
                if is_sender_blacklisted(&auth.recover_authority().unwrap_or_default()) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

pub const DEFAULT_EL_PATCH_TIME: &str = "1762349400";
pub const DEFAULT_7702_PATCH_TIME: &str = "1762522200";

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
            .expect("Error parsing balancerHardforkConfig");

    let config = parsed_mapping
        .into_iter()
        .map(|(addr, bytecode_str)| {
            let bytecode = if bytecode_str.len() > 0 {
                Some(Bytecode::new_legacy(
                    Bytes::from_hex(bytecode_str)
                        .expect(&format!("Unable to parse bytecode hex for {addr}")),
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
