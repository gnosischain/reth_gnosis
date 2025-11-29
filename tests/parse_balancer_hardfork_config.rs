use alloy_consensus::constants::KECCAK_EMPTY;
use reth_gnosis::consts::parse_balancer_hardfork_config;
use serde_json::json;

#[test]
fn test_returns_none_when_both_inputs_missing() {
    assert!(parse_balancer_hardfork_config(None, None).is_none());
}

#[test]
fn test_returns_none_when_time_missing() {
    let config = json!({
        "0x1111111111111111111111111111111111111111": ""
    });
    assert!(parse_balancer_hardfork_config(None, Some(&config)).is_none());
}

#[test]
fn test_returns_none_when_config_missing() {
    let time = json!(1000);
    assert!(parse_balancer_hardfork_config(Some(&time), None).is_none());
}

#[test]
fn test_parses_valid_config() {
    let time = json!(12345);
    let config = json!({
        "0x1111111111111111111111111111111111111111": "",
        "0x2222222222222222222222222222222222222222": "6080"
    });

    let result = parse_balancer_hardfork_config(Some(&time), Some(&config));
    assert!(result.is_some());

    let hardfork_config = result.unwrap();
    assert_eq!(hardfork_config.activation_time, 12345);
    assert_eq!(hardfork_config.config.len(), 2);
}

#[test]
fn test_empty_bytecode_string_results_in_none_with_keccak_empty() {
    let time = json!(1000);
    let config = json!({
        "0x1111111111111111111111111111111111111111": ""
    });

    let result = parse_balancer_hardfork_config(Some(&time), Some(&config))
        .expect("Should parse successfully");

    let (_, bytecode, code_hash) = &result.config[0];
    assert!(
        bytecode.is_none(),
        "Empty string should result in None bytecode"
    );
    assert_eq!(
        *code_hash, KECCAK_EMPTY,
        "Empty bytecode should have KECCAK_EMPTY hash"
    );
}

#[test]
fn test_handles_bytecode_with_0x_prefix() {
    let time = json!(1000);
    let config = json!({
        "0x1111111111111111111111111111111111111111": "0x6080"
    });

    let result = parse_balancer_hardfork_config(Some(&time), Some(&config))
        .expect("Should parse successfully");

    let (_, bytecode, _) = &result.config[0];
    assert!(bytecode.is_some(), "Should parse bytecode with 0x prefix");
}

#[test]
fn test_handles_bytecode_without_0x_prefix() {
    let time = json!(1000);
    let config = json!({
        "0x1111111111111111111111111111111111111111": "6080"
    });

    let result = parse_balancer_hardfork_config(Some(&time), Some(&config))
        .expect("Should parse successfully");

    let (_, bytecode, _) = &result.config[0];
    assert!(
        bytecode.is_some(),
        "Should parse bytecode without 0x prefix"
    );
}

#[test]
fn test_handles_multiple_addresses() {
    let time = json!(1000);
    let config = json!({
        "0x1111111111111111111111111111111111111111": "",
        "0x2222222222222222222222222222222222222222": "6080",
        "0x3333333333333333333333333333333333333333": "60806040"
    });

    let result = parse_balancer_hardfork_config(Some(&time), Some(&config))
        .expect("Should parse successfully");

    assert_eq!(result.config.len(), 3, "Should have 3 addresses");
}

#[test]
fn test_code_hash_matches_bytecode() {
    let time = json!(1000);
    let bytecode_hex = "6080604052348015600e575f5ffd5b50603e80601a5f395ff3fe60806040525f5ffdfea2646970667358221220f7f53e1645a9cd5b79da6920c67891306d178dcff5e5683946cc1dae3c65aed664736f6c634300081e0033";
    let config = json!({
        "0x1111111111111111111111111111111111111111": bytecode_hex
    });

    let result = parse_balancer_hardfork_config(Some(&time), Some(&config))
        .expect("Should parse successfully");

    let (_, bytecode, code_hash) = &result.config[0];
    let bytecode = bytecode.as_ref().expect("Should have bytecode");

    assert_eq!(
        bytecode.hash_slow(),
        *code_hash,
        "Code hash should match the bytecode's hash"
    );
}
