use alloy_genesis::Genesis;
use std::sync::LazyLock;

pub static CHIADO_GENESIS: LazyLock<Genesis> = LazyLock::new(|| {
    serde_json::from_str(include_str!("chainspecs/chiado.json"))
        .expect("Can't deserialize Mainnet genesis json")
});

pub static GNOSIS_GENESIS: LazyLock<Genesis> = LazyLock::new(|| {
    serde_json::from_str(include_str!("chainspecs/gnosis.json"))
        .expect("Can't deserialize Mainnet genesis json")
});
