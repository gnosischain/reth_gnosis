//! Integration tests for bytecode rewrite via full block execution.
//!
//! These tests verify that the Balancer hardfork bytecode rewrites are correctly
//! applied during block execution, specifically in `apply_pre_execution_changes`.
//!
//! This tests the actual production code path rather than calling `rewrite_bytecodes` directly.

use alloy_primitives::{Address, Bytes, B256, U256};
use reth_evm::block::BlockExecutor;
use reth_evm::env::EvmEnv;
use reth_evm::{Evm, EvmFactory};
use reth_evm_ethereum::RethReceiptBuilder;
use reth_gnosis::block::{GnosisBlockExecutionCtx, GnosisBlockExecutor};
use reth_gnosis::evm::factory::GnosisEvmFactory;
use reth_gnosis::spec::gnosis_spec::{BalancerHardforkConfig, GnosisChainSpec, GnosisHardForks};
use revm::context::{BlockEnv, CfgEnv};
use revm::database::{CacheDB, EmptyDB};
use revm::Database;
use revm_database::State;
use revm_state::{AccountInfo, Bytecode};
use serde_json::json;

const TEST_BYTECODE: &str = "6080604052348015600e575f5ffd5b50603e80601a5f395ff3fe60806040525f5ffdfea2646970667358221220f7f53e1645a9cd5b79da6920c67891306d178dcff5e5683946cc1dae3c65aed664736f6c634300081e0033";
const HARDFORK_ACTIVATION_TIME: u64 = 1000;

/// Creates a test chain spec with Balancer hardfork configured at HARDFORK_ACTIVATION_TIME.
fn create_test_chain_spec_with_balancer_hardfork() -> GnosisChainSpec {
    // Create genesis with extra_fields for BalancerFork
    // The GnosisChainSpec::from(Genesis) reads these to configure the hardfork
    let mut genesis = alloy_genesis::Genesis::default();
    genesis.config.extra_fields.insert(
        "balancerHardforkTime".to_string(),
        json!(HARDFORK_ACTIVATION_TIME),
    );
    genesis.config.extra_fields.insert(
        "balancerHardforkBytecodes".to_string(),
        json!({
            "0x1111111111111111111111111111111111111111": "",
            "0x2222222222222222222222222222222222222222": TEST_BYTECODE
        }),
    );

    // This will properly set up the hardfork in the chain's hardfork list
    GnosisChainSpec::from(genesis)
}

/// Creates EVM environment for a block at the given timestamp.
fn create_block_env(timestamp: u64) -> EvmEnv {
    EvmEnv {
        cfg_env: CfgEnv::default().with_chain_id(100), // Gnosis chain ID
        block_env: BlockEnv {
            timestamp: U256::from(timestamp),
            number: U256::from(1),
            ..Default::default()
        },
    }
}

/// Creates a GnosisBlockExecutionCtx with the given parent timestamp.
fn create_execution_ctx(parent_timestamp: u64) -> GnosisBlockExecutionCtx<'static> {
    GnosisBlockExecutionCtx {
        parent_hash: B256::ZERO,
        parent_beacon_block_root: None,
        withdrawals: None,
        parent_timestamp,
    }
}

/// Sets up a database with "wrong" bytecode at the target addresses.
fn setup_db_with_wrong_bytecode(config: &BalancerHardforkConfig) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    let wrong_code = Bytecode::new_legacy(Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]));

    for (addr, _, _) in &config.config {
        db.insert_account_info(
            *addr,
            AccountInfo {
                balance: U256::from(1000),
                nonce: 5,
                code_hash: wrong_code.hash_slow(),
                code: Some(wrong_code.clone()),
                account_id: None,
            },
        );
    }
    db
}

/// Sets up a database with correct bytecode (already rewritten).
fn setup_db_with_correct_bytecode(config: &BalancerHardforkConfig) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());

    for (addr, expected_code, expected_hash) in &config.config {
        db.insert_account_info(
            *addr,
            AccountInfo {
                balance: U256::from(1000),
                nonce: 5,
                code_hash: *expected_hash,
                code: expected_code.clone(),
                account_id: None,
            },
        );
    }
    db
}

#[test]
fn test_bytecode_rewrite_at_hardfork_activation_block() {
    // Setup: Chain spec with Balancer hardfork at timestamp 1000
    let spec = create_test_chain_spec_with_balancer_hardfork();
    let config = spec.balancer_hardfork_config.as_ref().unwrap();

    // Parent block at timestamp 999 (BEFORE hardfork)
    // Current block at timestamp 1000 (AT hardfork activation)
    let parent_timestamp = HARDFORK_ACTIVATION_TIME - 1; // 999
    let current_timestamp = HARDFORK_ACTIVATION_TIME; // 1000

    // Verify hardfork activation logic
    assert!(
        !spec.is_balancer_hardfork_active_at_timestamp(parent_timestamp),
        "Hardfork should NOT be active at parent timestamp {parent_timestamp}"
    );
    assert!(
        spec.is_balancer_hardfork_active_at_timestamp(current_timestamp),
        "Hardfork SHOULD be active at current timestamp {current_timestamp}"
    );

    // Setup database with WRONG bytecode
    let db = setup_db_with_wrong_bytecode(config);
    let mut state = State::builder().with_database(db).build();

    // Create EVM and executor
    let factory = GnosisEvmFactory {
        fee_collector_address: Address::ZERO,
    };
    let evm_env = create_block_env(current_timestamp);
    let evm = factory.create_evm(&mut state, evm_env);

    let ctx = create_execution_ctx(parent_timestamp);

    let receipt_builder = RethReceiptBuilder::default();
    let mut executor = GnosisBlockExecutor::new(
        evm,
        ctx,
        &spec,
        &receipt_builder,
        Address::ZERO, // block_rewards_address
    );

    // Execute pre-block changes - this should trigger bytecode rewrite
    // Note: This may fail due to missing blockhashes/beacon root contracts in empty state
    // The important thing is that the bytecode rewrite happens BEFORE those calls
    let _ = executor.apply_pre_execution_changes();

    // Verify bytecode was rewritten for ALL configured addresses
    for (addr, _expected_code, expected_hash) in &config.config {
        let account = executor.evm_mut().db_mut().basic(*addr).unwrap().unwrap();
        assert_eq!(
            account.code_hash, *expected_hash,
            "Address {addr:?} should have bytecode rewritten at activation block"
        );
        // Verify balance/nonce preserved
        assert_eq!(
            account.balance,
            U256::from(1000),
            "Balance should be preserved"
        );
        assert_eq!(account.nonce, 5, "Nonce should be preserved");
    }
}

#[test]
fn test_bytecode_not_rewritten_after_hardfork_activation() {
    // Setup: Chain spec with Balancer hardfork at timestamp 1000
    let spec = create_test_chain_spec_with_balancer_hardfork();
    let config = spec.balancer_hardfork_config.as_ref().unwrap();

    // Parent block at timestamp 1000 (AFTER hardfork - already active)
    // Current block at timestamp 1001 (still active)
    let parent_timestamp = HARDFORK_ACTIVATION_TIME; // 1000
    let current_timestamp = HARDFORK_ACTIVATION_TIME + 1; // 1001

    // Both should be active - no rewrite should occur
    assert!(
        spec.is_balancer_hardfork_active_at_timestamp(parent_timestamp),
        "Hardfork SHOULD be active at parent timestamp"
    );
    assert!(
        spec.is_balancer_hardfork_active_at_timestamp(current_timestamp),
        "Hardfork SHOULD be active at current timestamp"
    );

    // Setup database with WRONG bytecode
    // If rewrite doesn't happen, bytecode should remain wrong
    let db = setup_db_with_wrong_bytecode(config);
    let wrong_hash = db
        .cache
        .accounts
        .get(&config.config[0].0)
        .unwrap()
        .info
        .code_hash;
    let mut state = State::builder().with_database(db).build();

    // Create EVM and executor
    let factory = GnosisEvmFactory {
        fee_collector_address: Address::ZERO,
    };
    let evm_env = create_block_env(current_timestamp);
    let evm = factory.create_evm(&mut state, evm_env);

    let ctx = create_execution_ctx(parent_timestamp);

    let receipt_builder = RethReceiptBuilder::default();
    let mut executor = GnosisBlockExecutor::new(evm, ctx, &spec, &receipt_builder, Address::ZERO);

    // Execute pre-block changes - should NOT trigger bytecode rewrite
    let _ = executor.apply_pre_execution_changes();

    // Verify bytecode was NOT rewritten (still has wrong code)
    let (addr, _, expected_hash) = &config.config[0];
    let account = executor.evm_mut().db_mut().basic(*addr).unwrap().unwrap();
    assert_ne!(
        account.code_hash, *expected_hash,
        "Bytecode should NOT be rewritten after activation block"
    );
    assert_eq!(
        account.code_hash, wrong_hash,
        "Bytecode should remain unchanged"
    );
}

#[test]
fn test_bytecode_not_rewritten_before_hardfork() {
    // Setup: Chain spec with Balancer hardfork at timestamp 1000
    let spec = create_test_chain_spec_with_balancer_hardfork();
    let config = spec.balancer_hardfork_config.as_ref().unwrap();

    // Parent block at timestamp 998 (before hardfork)
    // Current block at timestamp 999 (still before hardfork)
    let parent_timestamp = HARDFORK_ACTIVATION_TIME - 2; // 998
    let current_timestamp = HARDFORK_ACTIVATION_TIME - 1; // 999

    // Neither should be active
    assert!(
        !spec.is_balancer_hardfork_active_at_timestamp(parent_timestamp),
        "Hardfork should NOT be active at parent timestamp"
    );
    assert!(
        !spec.is_balancer_hardfork_active_at_timestamp(current_timestamp),
        "Hardfork should NOT be active at current timestamp"
    );

    // Setup database with WRONG bytecode
    let db = setup_db_with_wrong_bytecode(config);
    let wrong_hash = db
        .cache
        .accounts
        .get(&config.config[0].0)
        .unwrap()
        .info
        .code_hash;
    let mut state = State::builder().with_database(db).build();

    // Create EVM and executor
    let factory = GnosisEvmFactory {
        fee_collector_address: Address::ZERO,
    };
    let evm_env = create_block_env(current_timestamp);
    let evm = factory.create_evm(&mut state, evm_env);

    let ctx = create_execution_ctx(parent_timestamp);

    let receipt_builder = RethReceiptBuilder::default();
    let mut executor = GnosisBlockExecutor::new(evm, ctx, &spec, &receipt_builder, Address::ZERO);

    // Execute pre-block changes - should NOT trigger bytecode rewrite
    let _ = executor.apply_pre_execution_changes();

    // Verify bytecode was NOT rewritten
    let (addr, _, expected_hash) = &config.config[0];
    let account = executor.evm_mut().db_mut().basic(*addr).unwrap().unwrap();
    assert_ne!(
        account.code_hash, *expected_hash,
        "Bytecode should NOT be rewritten before hardfork"
    );
    assert_eq!(
        account.code_hash, wrong_hash,
        "Bytecode should remain unchanged"
    );
}

#[test]
fn test_bytecode_rewrite_idempotent_via_block_execution() {
    // Setup: Chain spec with Balancer hardfork at timestamp 1000
    let spec = create_test_chain_spec_with_balancer_hardfork();
    let config = spec.balancer_hardfork_config.as_ref().unwrap();

    // At activation boundary
    let parent_timestamp = HARDFORK_ACTIVATION_TIME - 1;
    let current_timestamp = HARDFORK_ACTIVATION_TIME;

    // Setup database with CORRECT bytecode (already rewritten)
    let db = setup_db_with_correct_bytecode(config);
    let mut state = State::builder().with_database(db).build();

    // Create EVM and executor
    let factory = GnosisEvmFactory {
        fee_collector_address: Address::ZERO,
    };
    let evm_env = create_block_env(current_timestamp);
    let evm = factory.create_evm(&mut state, evm_env);

    let ctx = create_execution_ctx(parent_timestamp);

    let receipt_builder = RethReceiptBuilder::default();
    let mut executor = GnosisBlockExecutor::new(evm, ctx, &spec, &receipt_builder, Address::ZERO);

    // Execute pre-block changes - rewrite is triggered but should be idempotent
    let _ = executor.apply_pre_execution_changes();

    // Verify bytecode still has correct values (idempotent)
    for (addr, _, expected_hash) in &config.config {
        let account = executor.evm_mut().db_mut().basic(*addr).unwrap().unwrap();
        assert_eq!(
            account.code_hash, *expected_hash,
            "Bytecode should remain correct after idempotent rewrite"
        );
    }
}
