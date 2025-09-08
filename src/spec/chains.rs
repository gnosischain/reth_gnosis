use alloy_genesis::Genesis;
use std::sync::LazyLock;

// const CHIADO_GENESIS: Genesis = Genesis {
//     config: ChainConfig {
//         chain_id: 10200,
//         terminal_total_difficulty: Some(U256::from_str_radix("231707791542740786049188744689299064356246512", 10).unwrap()),
//         terminal_total_difficulty_passed: true,
//         blob_schedule: CHIADO_BLOB_SCHEDULE.clone(),
//         extra_fields: OtherFields::new(BTreeMap::from([
//                 (String::from("eip1559collector"), serde_json::to_value(address!("0x1559000000000000000000000000000000000000")).unwrap()),
//                 (String::from("blockRewardsContract"), serde_json::to_value(address!("0x2000000000000000000000000000000000000001")).unwrap()),
//         ])),
//         ..Default::default()
//     },
//     gas_limit: 17_000_000,
//     difficulty: U256::from_str_radix("0x20000", 16).unwrap(),
//     // alloc:
//     ..Default::default()
// };

pub static CHIADO_GENESIS: LazyLock<Genesis> = LazyLock::new(|| {
    serde_json::from_str(include_str!("chainspecs/chiado.json"))
        .expect("Can't deserialize Mainnet genesis json")
});

pub static GNOSIS_GENESIS: LazyLock<Genesis> = LazyLock::new(|| {
    serde_json::from_str(include_str!("chainspecs/gnosis.json"))
        .expect("Can't deserialize Mainnet genesis json")
});

pub static DEVNET_GENESIS: LazyLock<Genesis> = LazyLock::new(|| {
    serde_json::from_str(include_str!("chainspecs/devnet.json"))
        .expect("Can't deserialize Devnet genesis json")
});
