#![allow(missing_docs)]
#![cfg(feature = "testing")]
#![cfg(test)]

macro_rules! general_state_test {
    ($test_name:ident, $fork_or_testname:ident $(, $test:ident, $testname:ident)?) => {
        #[test]
        fn $test_name() {
            // if test and testname is empty, then return BlockchainTests::new(format!("GeneralStateTests/{}", stringify!($dir))).run();
            if stringify!($($test)?).is_empty() && stringify!($($testname)?).is_empty() {
                return BlockchainTests::new(format!("BlockchainTests/GeneralStateTests/{}", stringify!($fork_or_testname))).run();
            }
            $(BlockchainTests::new(format!("blockchain_tests/{}/{}/{}", stringify!($fork_or_testname), stringify!($test), stringify!($testname))).run();)?
        }
    };
}

#[allow(missing_docs)]
mod general_state_tests {
    use crate::testing::{cases::blockchain_test::BlockchainTests, suite::Suite};

    ///////////////////////////// TESTS FROM EXECUTION LAYER SPEC TESTS /////////////////////////////
    general_state_test!(modexp, byzantium, eip198_modexp_precompile, modexp);
    general_state_test!(acl, berlin, eip2930_access_list, acl);
    general_state_test!(dup, frontier, opcodes, dup);
    general_state_test!(
        call_and_callcode_gas_calculation,
        frontier,
        opcodes,
        call_and_callcode_gas_calculation
    );
    general_state_test!(chainid, istanbul, eip1344_chainid, chainid);
    general_state_test!(
        dynamic_create2_selfdestruct_collision,
        cancun,
        eip6780_selfdestruct,
        dynamic_create2_selfdestruct_collision
    );
    general_state_test!(selfdestruct, cancun, eip6780_selfdestruct, selfdestruct);
    general_state_test!(
        reentrancy_selfdestruct_revert,
        cancun,
        eip6780_selfdestruct,
        reentrancy_selfdestruct_revert
    );
    general_state_test!(
        warm_coinbase,
        shanghai,
        eip3651_warm_coinbase,
        warm_coinbase
    );
    general_state_test!(push0, shanghai, eip3855_push0, push0);
    general_state_test!(yul_example, homestead, yul, yul_example);
    general_state_test!(
        selfdestruct_balance_bug,
        paris,
        security,
        selfdestruct_balance_bug
    );

    #[cfg(feature = "failing-tests")]
    mod failing_eels_tests {
        use super::*;
        general_state_test!(withdrawals, shanghai, eip4895_withdrawals, withdrawals);
        general_state_test!(initcode, shanghai, eip3860_initcode, initcode);
    }

    /////////////////////////////////// TESTS FROM ETHEREUM/TESTS ///////////////////////////////////
    general_state_test!(st_args_zero_one_balance, stArgsZeroOneBalance);
    general_state_test!(st_attack, stAttackTest);
    general_state_test!(st_bugs, stBugs);
    general_state_test!(st_call_codes, stCallCodes);
    general_state_test!(st_call_create_call_code, stCallCreateCallCodeTest);
    general_state_test!(
        st_call_delegate_codes_call_code_homestead,
        stCallDelegateCodesCallCodeHomestead
    );
    general_state_test!(
        st_call_delegate_codes_homestead,
        stCallDelegateCodesHomestead
    );
    general_state_test!(st_chain_id, stChainId);
    general_state_test!(st_code_copy_test, stCodeCopyTest);
    general_state_test!(st_code_size_limit, stCodeSizeLimit);
    general_state_test!(st_delegate_call_test_homestead, stDelegatecallTestHomestead);
    general_state_test!(st_eip150_gas_prices, stEIP150singleCodeGasPrices);
    general_state_test!(st_eip150, stEIP150Specific);
    general_state_test!(st_eip158, stEIP158Specific);
    general_state_test!(st_eip2930, stEIP2930);
    general_state_test!(st_homestead, stHomesteadSpecific);
    general_state_test!(st_init_code, stInitCodeTest);
    general_state_test!(st_log, stLogTests);
    general_state_test!(st_mem_expanding_eip150_calls, stMemExpandingEIP150Calls);
    general_state_test!(st_memory_stress, stMemoryStressTest);
    general_state_test!(st_memory, stMemoryTest);
    general_state_test!(st_non_zero_calls, stNonZeroCallsTest);
    general_state_test!(st_precompiles, stPreCompiledContracts);
    general_state_test!(st_precompiles2, stPreCompiledContracts2);
    general_state_test!(st_random, stRandom);
    general_state_test!(st_random2, stRandom2);
    general_state_test!(st_refund, stRefundTest);
    general_state_test!(st_return, stReturnDataTest);
    general_state_test!(st_self_balance, stSelfBalance);
    general_state_test!(st_shift, stShift);
    general_state_test!(st_sload, stSLoadTest);
    general_state_test!(st_solidity, stSolidityTest);
    general_state_test!(st_static_flag, stStaticFlagEnabled);
    general_state_test!(st_system_operations, stSystemOperationsTest);
    general_state_test!(st_time_consuming, stTimeConsuming);
    general_state_test!(st_wallet, stWalletTest);
    general_state_test!(st_zero_calls_revert, stZeroCallsRevert);
    general_state_test!(st_zero_calls, stZeroCallsTest);
    general_state_test!(st_zero_knowledge, stZeroKnowledge);
    general_state_test!(st_zero_knowledge2, stZeroKnowledge2);

    #[cfg(feature = "failing-tests")]
    mod failing_ethereum_tests {
        use super::*;
        general_state_test!(shanghai, Shanghai);
        general_state_test!(st_bad_opcode, stBadOpcode);
        general_state_test!(st_create2, stCreate2);
        general_state_test!(st_create, stCreateTest);
        general_state_test!(st_eip1559, stEIP1559);
        general_state_test!(st_eip3607, stEIP3607);
        general_state_test!(st_example, stExample);
        general_state_test!(st_ext_codehash, stExtCodeHash);
        general_state_test!(st_quadratic_complexity, stQuadraticComplexityTest);
        general_state_test!(st_recursive_create, stRecursiveCreate);
        general_state_test!(st_revert, stRevertTest);
        general_state_test!(st_special, stSpecialTest);
        general_state_test!(st_sstore, stSStoreTest);
        general_state_test!(st_stack, stStackTests);
        general_state_test!(st_static_call, stStaticCall); // passing, but conflicts with rewards contract
        general_state_test!(st_transaction, stTransactionTest);
        general_state_test!(vm_tests, VMTests);
    }
}
