use alloy_consensus::constants::KECCAK_EMPTY;
use alloy_primitives::{Address, Bytes, U256};
use evm2::{
    bytecode::Bytecode,
    env::BlockEnv,
    evm::{AccountInfo, InMemoryDB},
    Evm, SpecId,
};
use reth_gnosis::consts::parse_balancer_hardfork_config;
use reth_gnosis::evm::{factory::GnosisEvmEnv, GnosisEvmFactory, GnosisEvmTypes};
use reth_gnosis::gnosis::rewrite_bytecodes;
use reth_gnosis::spec::gnosis_spec::BalancerHardforkConfig;
use serde_json::json;

const TEST_BYTECODE: &str = "6080604052348015600e575f5ffd5b50603e80601a5f395ff3fe60806040525f5ffdfea2646970667358221220f7f53e1645a9cd5b79da6920c67891306d178dcff5e5683946cc1dae3c65aed664736f6c634300081e0033";

/// Gnosis chain ID.
const CHAIN_ID: u64 = 100;

fn get_test_hardfork_config() -> BalancerHardforkConfig {
    let time_value = json!(1000);
    let config_value = json!({
        "0x1111111111111111111111111111111111111111": "",
        "0x2222222222222222222222222222222222222222": TEST_BYTECODE
    });
    parse_balancer_hardfork_config(Some(&time_value), Some(&config_value))
        .expect("Test config should parse successfully")
}

fn account(balance: u64, nonce: u64, code: Option<Bytecode>) -> AccountInfo {
    let info = AccountInfo::empty()
        .with_balance(U256::from(balance))
        .with_nonce(nonce);
    match code {
        Some(code) => info.with_code(code),
        None => info,
    }
}

/// Creates a Gnosis EVM over `db` for testing.
fn create_test_evm(db: InMemoryDB) -> Evm<'static, GnosisEvmTypes> {
    let factory = GnosisEvmFactory {
        fee_collector_address: Address::ZERO,
        eip1283_window: None,
    };
    factory.create_evm(
        db,
        GnosisEvmEnv::new(
            SpecId::OSAKA,
            BlockEnv::<GnosisEvmTypes>::default(),
            CHAIN_ID,
        ),
    )
}

/// Applies the Balancer rewrites to `evm`'s state, as the block executor does.
fn apply_rewrites(evm: &mut Evm<'_, GnosisEvmTypes>, config: &BalancerHardforkConfig) {
    let state = rewrite_bytecodes(evm, config).expect("in-memory reads cannot fail");
    evm.overlay_db_mut().commit_pending(&state);
}

fn read_account(evm: &mut Evm<'_, GnosisEvmTypes>, address: &Address) -> AccountInfo {
    evm.read_account_info(address)
        .expect("in-memory reads cannot fail")
        .expect("account should exist")
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
    let mut db = InMemoryDB::default();
    let original_code = Bytecode::new_legacy(Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]));
    let original_code_hash = original_code.hash_slow();

    db.insert_account_info(&addr, account(1000, 5, Some(original_code)));

    // Sanity check: original code hash differs from expected
    assert_ne!(original_code_hash, expected_code_hash);

    // Action: Create EVM and call rewrite_bytecodes
    let mut evm = create_test_evm(db);
    apply_rewrites(&mut evm, &config);

    // Assert: Code should be rewritten
    let account = read_account(&mut evm, &addr);
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
    let mut db = InMemoryDB::default();
    db.insert_account_info(&addr, account(500, 10, expected_bytecode.clone()));

    // Action: Create EVM and call rewrite_bytecodes
    let mut evm = create_test_evm(db);
    apply_rewrites(&mut evm, &config);

    // Assert: Account should remain unchanged (skip path taken)
    let account = read_account(&mut evm, &addr);
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
    let db = InMemoryDB::default();

    // Action: Create EVM and call rewrite_bytecodes
    let mut evm = create_test_evm(db);
    apply_rewrites(&mut evm, &config);

    // Assert: Account should now exist with the expected code
    // Note: After commit, we need to check the cache directly
    let account = read_account(&mut evm, &addr);
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
    let mut db = InMemoryDB::default();
    let wrong_code = Bytecode::new_legacy(Bytes::from_static(&[0xff, 0xff]));

    for (addr, _, _) in &config.config {
        db.insert_account_info(addr, account(100, 1, Some(wrong_code.clone())));
    }

    // Action: Create EVM and call rewrite_bytecodes
    let mut evm = create_test_evm(db);
    apply_rewrites(&mut evm, &config);

    // Assert: ALL addresses should have been rewritten
    for (addr, _expected_bytecode, expected_code_hash) in &config.config {
        let account = read_account(&mut evm, addr);
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
        let mut db = InMemoryDB::default();
        let existing_code = Bytecode::new_legacy(Bytes::from_static(&[0x60, 0x00, 0x60, 0x00]));
        db.insert_account_info(addr, account(999, 42, Some(existing_code)));

        // Action
        let mut evm = create_test_evm(db);
        apply_rewrites(&mut evm, &config);

        // Assert: Code should be cleared
        let account = read_account(&mut evm, addr);
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
        let mut db = InMemoryDB::default();
        db.insert_account_info(addr, account(123, 7, None));

        // Action
        let mut evm = create_test_evm(db);
        apply_rewrites(&mut evm, &config);

        // Assert: Account should have the specific bytecode
        let account = read_account(&mut evm, addr);
        assert_eq!(account.code_hash, *expected_code_hash);

        // Verify the bytecode content matches
        if let Some(expected) = expected_bytecode {
            let code = evm
                .overlay_db_mut()
                .cache
                .contracts
                .get(expected_code_hash)
                .expect("Should have code");
            assert_eq!(code.hash_slow(), expected.hash_slow());
        }
    }
}

#[test]
fn test_rewrite_bytecodes_idempotent() {
    let config = get_test_hardfork_config();

    // Setup: Account with wrong code
    let mut db = InMemoryDB::default();
    for (addr, _, _) in &config.config {
        db.insert_account_info(
            addr,
            account(
                100,
                1,
                Some(Bytecode::new_legacy(Bytes::from_static(&[0xba, 0xd]))),
            ),
        );
    }

    // Action: Call rewrite_bytecodes TWICE
    let mut evm = create_test_evm(db);
    apply_rewrites(&mut evm, &config);
    apply_rewrites(&mut evm, &config); // Second call should be a no-op

    // Assert: All accounts still have correct code
    for (addr, _, expected_code_hash) in &config.config {
        let account = read_account(&mut evm, addr);
        assert_eq!(account.code_hash, *expected_code_hash);
    }
}
