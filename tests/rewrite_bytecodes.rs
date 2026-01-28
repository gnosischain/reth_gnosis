use alloy_consensus::constants::KECCAK_EMPTY;
use alloy_primitives::{keccak256, Address, Bytes, U256};
use reth_evm::{env::EvmEnv, Evm, EvmFactory};
use reth_gnosis::consts::parse_balancer_hardfork_config;
use reth_gnosis::evm::factory::GnosisEvmFactory;
use reth_gnosis::gnosis::rewrite_bytecodes;
use reth_gnosis::spec::gnosis_spec::BalancerHardforkConfig;
use revm::context::{BlockEnv, CfgEnv};
use revm::database::{CacheDB, EmptyDB};
use revm::Database;
use revm_state::{AccountInfo, Bytecode};
use serde_json::json;

const TEST_BYTECODE: &str = "6080604052348015600e575f5ffd5b50603e80601a5f395ff3fe60806040525f5ffdfea2646970667358221220f7f53e1645a9cd5b79da6920c67891306d178dcff5e5683946cc1dae3c65aed664736f6c634300081e0033";

fn get_test_hardfork_config() -> BalancerHardforkConfig {
    let time_value = json!(1000);
    let config_value = json!({
        "0x1111111111111111111111111111111111111111": "",
        "0x2222222222222222222222222222222222222222": TEST_BYTECODE
    });
    parse_balancer_hardfork_config(Some(&time_value), Some(&config_value))
        .expect("Test config should parse successfully")
}

/// Creates a minimal EvmEnv for testing purposes
fn create_test_evm_env() -> EvmEnv {
    EvmEnv {
        cfg_env: CfgEnv::default().with_chain_id(100), // Gnosis chain ID
        block_env: BlockEnv::default(),
    }
}

/// Creates a GnosisEvm instance with a CacheDB for testing
fn create_test_evm(db: CacheDB<EmptyDB>) -> impl Evm<DB = CacheDB<EmptyDB>> {
    let factory = GnosisEvmFactory {
        fee_collector_address: Address::ZERO,
    };
    factory.create_evm(db, create_test_evm_env())
}

#[test]
fn test_rewrite_bytecodes_rewrites_when_code_differs() {
    // Get the rewrite config to know what addresses and expected codes we're testing
    let config = get_test_hardfork_config();
    assert!(
        !config.config.is_empty(),
        "Should have at least one rewrite configured"
    );

    let (addr, _expected_bytecode, expected_code_hash) = config.config[0].clone();

    // Setup: Create account with DIFFERENT code than what rewrite expects
    let mut db = CacheDB::new(EmptyDB::default());
    let original_code = Bytecode::new_legacy(Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]));
    let original_code_hash = original_code.hash_slow();

    db.insert_account_info(
        addr,
        AccountInfo {
            balance: U256::from(1000),
            nonce: 5,
            code_hash: original_code_hash,
            code: Some(original_code),
            account_id: None,
        },
    );

    // Sanity check: original code hash differs from expected
    assert_ne!(original_code_hash, expected_code_hash);

    // Action: Create EVM and call rewrite_bytecodes
    let mut evm = create_test_evm(db);
    rewrite_bytecodes(&mut evm, &config);

    // Assert: Code should be rewritten
    let account = evm.db_mut().basic(addr).unwrap().unwrap();
    assert_eq!(
        account.code_hash, expected_code_hash,
        "Code hash should be updated to expected value"
    );

    // Assert: Balance and nonce should be PRESERVED
    assert_eq!(
        account.balance,
        U256::from(1000),
        "Balance should be preserved"
    );
    assert_eq!(account.nonce, 5, "Nonce should be preserved");
}

#[test]
fn test_rewrite_bytecodes_skips_when_code_matches_expected() {
    let config = get_test_hardfork_config();
    let (addr, expected_bytecode, expected_code_hash) = config.config[0].clone();

    // Setup: Account already has the expected code hash
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        addr,
        AccountInfo {
            balance: U256::from(500),
            nonce: 10,
            code_hash: expected_code_hash, // Already matches expected!
            code: expected_bytecode.clone(),
            account_id: None,
        },
    );

    // Action: Create EVM and call rewrite_bytecodes
    let mut evm = create_test_evm(db);
    rewrite_bytecodes(&mut evm, &config);

    // Assert: Account should remain unchanged (skip path taken)
    let account = evm.db_mut().basic(addr).unwrap().unwrap();
    assert_eq!(account.code_hash, expected_code_hash);
    assert_eq!(account.balance, U256::from(500));
    assert_eq!(account.nonce, 10);
}

#[test]
fn test_rewrite_bytecodes_handles_nonexistent_account() {
    let config = get_test_hardfork_config();

    // Find an address that should get actual bytecode (not None)
    // When bytecode is None and account doesn't exist, skip is expected
    // (KECCAK_EMPTY == KECCAK_EMPTY)
    let code_entry = config
        .config
        .iter()
        .find(|(_, bytecode, _)| bytecode.is_some())
        .expect("Should have at least one address with bytecode");

    let (addr, _expected_bytecode, expected_code_hash) = code_entry.clone();

    // Setup: Empty database - account doesn't exist
    let db = CacheDB::new(EmptyDB::default());

    // Action: Create EVM and call rewrite_bytecodes
    let mut evm = create_test_evm(db);
    rewrite_bytecodes(&mut evm, &config);

    // Assert: Account should now exist with the expected code
    // Note: After commit, we need to check the cache directly
    let account = evm.db_mut().basic(addr).unwrap().unwrap();
    assert_eq!(
        account.code_hash, expected_code_hash,
        "Non-existent account should get the expected code"
    );
    // Default account has zero balance and nonce
    assert_eq!(account.balance, U256::ZERO);
    assert_eq!(account.nonce, 0);
}

#[test]
fn test_rewrite_bytecodes_all_addresses_processed() {
    let config = get_test_hardfork_config();

    // Setup: Create accounts with wrong code for ALL rewrite addresses
    let mut db = CacheDB::new(EmptyDB::default());
    let wrong_code = Bytecode::new_legacy(Bytes::from_static(&[0xff, 0xff]));

    for (addr, _, _) in &config.config {
        db.insert_account_info(
            *addr,
            AccountInfo {
                balance: U256::from(100),
                nonce: 1,
                code_hash: wrong_code.hash_slow(),
                code: Some(wrong_code.clone()),
                account_id: None,
            },
        );
    }

    // Action: Create EVM and call rewrite_bytecodes
    let mut evm = create_test_evm(db);
    rewrite_bytecodes(&mut evm, &config);

    // Assert: ALL addresses should have been rewritten
    for (addr, _expected_bytecode, expected_code_hash) in &config.config {
        let account = evm.db_mut().basic(*addr).unwrap().unwrap();
        assert_eq!(
            account.code_hash, *expected_code_hash,
            "Address {addr:?} should have expected code hash"
        );
    }
}

#[test]
fn test_rewrite_bytecodes_clears_code_when_none() {
    // Find the address that should have its code cleared (bytecode = None)
    let config = get_test_hardfork_config();
    let clear_entry = config
        .config
        .iter()
        .find(|(_, bytecode, _)| bytecode.is_none());

    if let Some((addr, _, expected_code_hash)) = clear_entry {
        assert_eq!(
            *expected_code_hash, KECCAK_EMPTY,
            "None bytecode should have KECCAK_EMPTY hash"
        );

        // Setup: Account with some code
        let mut db = CacheDB::new(EmptyDB::default());
        let existing_code = Bytecode::new_legacy(Bytes::from_static(&[0x60, 0x00, 0x60, 0x00]));
        db.insert_account_info(
            *addr,
            AccountInfo {
                balance: U256::from(999),
                nonce: 42,
                code_hash: existing_code.hash_slow(),
                code: Some(existing_code),
                account_id: None,
            },
        );

        // Action
        let mut evm = create_test_evm(db);
        rewrite_bytecodes(&mut evm, &config);

        // Assert: Code should be cleared
        let account = evm.db_mut().basic(*addr).unwrap().unwrap();
        assert_eq!(account.code_hash, KECCAK_EMPTY, "Code should be cleared");
        assert!(
            account.code.is_none() || account.code.as_ref().map(|c| c.is_empty()).unwrap_or(true),
            "Code should be None or empty"
        );
        // Balance/nonce preserved
        assert_eq!(account.balance, U256::from(999));
        assert_eq!(account.nonce, 42);
    }
}

#[test]
fn test_rewrite_bytecodes_sets_specific_code() {
    // Find an address that should get specific bytecode (not None)
    let config = get_test_hardfork_config();
    let code_entry = config
        .config
        .iter()
        .find(|(_, bytecode, _)| bytecode.is_some());

    if let Some((addr, expected_bytecode, expected_code_hash)) = code_entry {
        // Setup: Account with no code
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            *addr,
            AccountInfo {
                balance: U256::from(123),
                nonce: 7,
                code_hash: KECCAK_EMPTY,
                code: None,
                account_id: None,
            },
        );

        // Action
        let mut evm = create_test_evm(db);
        rewrite_bytecodes(&mut evm, &config);

        // Assert: Account should have the specific bytecode
        let account = evm.db_mut().basic(*addr).unwrap().unwrap();
        assert_eq!(account.code_hash, *expected_code_hash);

        // Verify the bytecode content matches
        if let Some(expected) = expected_bytecode {
            let code = account.code.expect("Should have code");
            assert_eq!(code.hash_slow(), expected.hash_slow());
        }
    }
}

#[test]
fn test_rewrite_bytecodes_idempotent() {
    let config = get_test_hardfork_config();

    // Setup: Account with wrong code
    let mut db = CacheDB::new(EmptyDB::default());
    for (addr, _, _) in &config.config {
        db.insert_account_info(
            *addr,
            AccountInfo {
                balance: U256::from(100),
                nonce: 1,
                code_hash: keccak256([0xba, 0xd]),
                code: Some(Bytecode::new_legacy(Bytes::from_static(&[0xba, 0xd]))),
                account_id: None,
            },
        );
    }

    // Action: Call rewrite_bytecodes TWICE
    let mut evm = create_test_evm(db);
    rewrite_bytecodes(&mut evm, &config);
    rewrite_bytecodes(&mut evm, &config); // Second call should be a no-op

    // Assert: All accounts still have correct code
    for (addr, _, expected_code_hash) in &config.config {
        let account = evm.db_mut().basic(*addr).unwrap().unwrap();
        assert_eq!(account.code_hash, *expected_code_hash);
    }
}
