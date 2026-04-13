use alloy_evm::precompiles::PrecompilesMap;
use alloy_evm::{Database, Evm};
use core::ops::{Deref, DerefMut};
use reth::revm::precompile::{PrecompileSpecId, Precompiles};
use reth_evm::{eth::EthEvmContext, EvmEnv, EvmFactory};
use revm::{
    context::{
        result::{EVMError, HaltReason, ResultAndState},
        BlockEnv, TxEnv,
    },
    handler::{instructions::EthInstructions, PrecompileProvider},
    inspector::NoOpInspector,
    interpreter::{interpreter::EthInterpreter, InterpreterResult},
    Context, ExecuteEvm, InspectEvm, Inspector, MainBuilder, MainContext,
};
use revm_primitives::{hardfork::SpecId, Address, Bytes};
use revm_primitives::{TxKind, U256};
use revm_state::{Account, AccountInfo, AccountStatus};

// https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md
const TX_GAS_LIMIT: u64 = 30_000_000;

#[allow(missing_debug_implementations)] // missing revm::Context Debug impl
pub struct GnosisEvm<DB: Database, I, PRECOMPILE = PrecompilesMap> {
    inner: crate::evm::gnosis_evm::GnosisEvm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
    >,
    inspect: bool,
}

impl<DB: Database, I, PRECOMPILE> GnosisEvm<DB, I, PRECOMPILE> {
    /// Creates a new Gnosis EVM instance.
    ///
    /// The `inspect` argument determines whether the configured [`Inspector`] of the given
    /// [`GnosisEvm`] should be invoked on [`Evm::transact`].
    pub const fn new(
        evm: crate::evm::gnosis_evm::GnosisEvm<
            EthEvmContext<DB>,
            I,
            EthInstructions<EthInterpreter, EthEvmContext<DB>>,
            PRECOMPILE,
        >,
        inspect: bool,
    ) -> Self {
        Self {
            inner: evm,
            inspect,
        }
    }

    /// Consumes self and return the inner EVM instance.
    pub fn into_inner(
        self,
    ) -> super::gnosis_evm::GnosisEvm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
    > {
        self.inner
    }

    /// Provides a reference to the EVM context.
    pub const fn ctx(&self) -> &EthEvmContext<DB> {
        &self.inner.0.ctx
    }

    /// Provides a mutable reference to the EVM context.
    pub fn ctx_mut(&mut self) -> &mut EthEvmContext<DB> {
        &mut self.inner.0.ctx
    }

    /// Provides a mutable reference to the EVM inspector.
    pub fn inspector_mut(&mut self) -> &mut I {
        &mut self.inner.0.inspector
    }
}

impl<DB: Database, I, PRECOMPILE> Deref for GnosisEvm<DB, I, PRECOMPILE> {
    type Target = EthEvmContext<DB>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.ctx()
    }
}

impl<DB: Database, I, PRECOMPILE> DerefMut for GnosisEvm<DB, I, PRECOMPILE> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx_mut()
    }
}

impl<DB, I, PRECOMPILE> Evm for GnosisEvm<DB, I, PRECOMPILE>
where
    DB: Database,
    I: Inspector<EthEvmContext<DB>>,
    PRECOMPILE: PrecompileProvider<EthEvmContext<DB>, Output = InterpreterResult>,
{
    type DB = DB;
    type Tx = TxEnv;
    type Error = EVMError<DB::Error>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PRECOMPILE;
    type Inspector = I;

    fn block(&self) -> &BlockEnv {
        &self.block
    }

    fn chain_id(&self) -> u64 {
        self.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        if self.inspect {
            self.inner.inspect_tx(tx)
        } else {
            self.inner.transact(tx)
        }
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState, Self::Error> {
        // Nethermind uses 30M gas for SystemTransaction.
        // We disable the block gas limit check so we can use this gas limit
        // without modifying block.gas_limit (which affects GASLIMIT opcode).
        let tx_gas = TX_GAS_LIMIT;

        let tx = TxEnv {
            caller,
            kind: TxKind::Call(contract),
            // Explicitly set nonce to 0 so revm does not do any nonce checks
            nonce: 0,
            gas_limit: tx_gas,
            value: U256::ZERO,
            data,
            // Setting the gas price to zero enforces that no value is transferred as part of the
            // call, and that the call will not count against the block's gas limit
            gas_price: 0,
            // The chain ID check is not relevant here and is disabled if set to None
            chain_id: None,
            // Setting the gas priority fee to None ensures the effective gas price is derived from
            // the `gas_price` field, which we need to be zero
            gas_priority_fee: None,
            access_list: Default::default(),
            // blob fields can be None for this tx
            blob_hashes: Vec::new(),
            max_fee_per_blob_gas: 0,
            tx_type: 0,
            authorization_list: Default::default(),
        };

        let mut disable_nonce_check = true;
        let mut disable_block_gas_limit = true;
        let mut disable_base_fee = true;
        let mut tx_gas_limit_cap = Some(tx_gas);

        // disable nonce check, block gas limit check, base fee check, and set tx gas limit cap
        core::mem::swap(&mut self.cfg.disable_nonce_check, &mut disable_nonce_check);
        core::mem::swap(
            &mut self.cfg.disable_block_gas_limit,
            &mut disable_block_gas_limit,
        );
        core::mem::swap(&mut self.cfg.disable_base_fee, &mut disable_base_fee);
        core::mem::swap(&mut self.cfg.tx_gas_limit_cap, &mut tx_gas_limit_cap);

        let mut res = self.transact(tx);

        // swap back
        core::mem::swap(&mut self.cfg.disable_nonce_check, &mut disable_nonce_check);
        core::mem::swap(
            &mut self.cfg.disable_block_gas_limit,
            &mut disable_block_gas_limit,
        );
        core::mem::swap(&mut self.cfg.disable_base_fee, &mut disable_base_fee);
        core::mem::swap(&mut self.cfg.tx_gas_limit_cap, &mut tx_gas_limit_cap);

        // NOTE: We assume that only the contract storage is modified. Revm currently marks the
        // caller and block beneficiary accounts as "touched" when we do the above transact calls,
        // and includes them in the result.
        //
        // We're doing this state cleanup to make sure that changeset only includes the changed
        // contract storage.
        if let Ok(res) = &mut res {
            // Nethermind: system account (SystemUser) is created if not exists and
            // persists in state. EIP-158 is disabled so empty accounts are NOT removed.
            // We ensure the system account is in the state with Created status if it
            // was loaded as not existing.
            let should_create = res
                .state
                .get(&alloy_eips::eip4788::SYSTEM_ADDRESS)
                .is_none_or(|system_account| {
                    system_account.status
                        == (AccountStatus::Touched | AccountStatus::LoadedAsNotExisting)
                });

            if should_create {
                let account = Account {
                    info: AccountInfo::default(),
                    storage: Default::default(),
                    status: AccountStatus::Touched | AccountStatus::Created,
                    original_info: Box::new(AccountInfo::default()),
                    transaction_id: 0,
                };
                res.state
                    .insert(alloy_eips::eip4788::SYSTEM_ADDRESS, account);
            } else if let Some(system_account) =
                res.state.get_mut(&alloy_eips::eip4788::SYSTEM_ADDRESS)
            {
                // System account already exists — undo nonce bump from revm's
                // validate_against_state_and_deduct_caller. Nethermind's
                // SystemTransactionProcessor doesn't increment nonce.
                system_account.info = *system_account.original_info.clone();
            }

            res.state.remove(&self.block.beneficiary);
            // Remove fee collector from system call state — GnosisEvmHandler touches it
            // during reward_beneficiary, but system calls should not collect fees
            let fee_collector = self.inner.1;
            res.state.remove(&fee_collector);

            // Filter out unchanged storage slots and unmodified accounts.
            // revm marks all SLOAD-ed slots and accessed accounts in the state diff
            // even if values didn't change. For system calls committed directly via
            // db.commit(), these "read-only" entries would pollute the state trie.
            for (_addr, account) in res.state.iter_mut() {
                account
                    .storage
                    .retain(|_slot, value| value.present_value != value.original_value);
            }
            // Note: Don't remove read-only accounts from the state diff.
            // The State::commit() handles unchanged accounts correctly by checking
            // the account status flags internally.
        }

        res
    }

    fn db_mut(&mut self) -> &mut Self::DB {
        &mut self.journaled_state.database
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Context {
            block: block_env,
            cfg: cfg_env,
            journaled_state,
            ..
        } = self.inner.0.ctx;

        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn precompiles_mut(&mut self) -> &mut Self::Precompiles {
        &mut self.inner.0.precompiles
    }

    fn inspector_mut(&mut self) -> &mut Self::Inspector {
        &mut self.inner.0.inspector
    }

    fn precompiles(&self) -> &Self::Precompiles {
        &self.inner.0.precompiles
    }

    fn inspector(&self) -> &Self::Inspector {
        &self.inner.0.inspector
    }

    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        (
            &self.inner.0.ctx.journaled_state.database,
            &self.inner.0.inspector,
            &self.inner.0.precompiles,
        )
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        (
            &mut self.inner.0.ctx.journaled_state.database,
            &mut self.inner.0.inspector,
            &mut self.inner.0.precompiles,
        )
    }
}

/// Custom EVM configuration.
#[derive(Debug, Clone, Default)]
pub struct GnosisEvmFactory {
    pub fee_collector_address: Address,
}

impl EvmFactory for GnosisEvmFactory {
    type Evm<DB: Database, I: Inspector<EthEvmContext<DB>>> = GnosisEvm<DB, I>;
    type Context<DB: Database> = EthEvmContext<DB>;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        let spec_id = input.cfg_env.spec;
        let mut evm = Context::mainnet()
            .with_db(db)
            .with_cfg(input.cfg_env)
            .with_block(input.block_env)
            .build_mainnet_with_inspector(NoOpInspector {})
            .with_precompiles(PrecompilesMap::from_static(Precompiles::new(
                PrecompileSpecId::from_spec_id(spec_id),
            )));

        // Gnosis Constantinople: override SSTORE to apply EIP-1283 net gas metering.
        // revm's CONSTANTINOPLE spec uses pre-EIP-1283 SSTORE gas, but Gnosis
        // activates EIP-1283 at Constantinople (block 1604400).
        if spec_id == SpecId::CONSTANTINOPLE {
            use revm::bytecode::opcode::SSTORE;
            use revm::interpreter::Instruction;
            evm.instruction.insert_instruction(
                SSTORE,
                Instruction::new(crate::evm::gnosis_evm::sstore_eip1283, 0),
            );
        }

        GnosisEvm {
            inner: super::gnosis_evm::GnosisEvm(evm, self.fee_collector_address),
            inspect: false,
        }
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>, EthInterpreter>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let spec_id = input.cfg_env.spec;
        let mut evm = Context::mainnet()
            .with_db(db)
            .with_cfg(input.cfg_env)
            .with_block(input.block_env)
            .build_mainnet_with_inspector(inspector)
            .with_precompiles(PrecompilesMap::from_static(Precompiles::new(
                PrecompileSpecId::from_spec_id(spec_id),
            )));

        // Gnosis Constantinople: override SSTORE to apply EIP-1283 net gas metering.
        // revm's CONSTANTINOPLE spec uses pre-EIP-1283 SSTORE gas, but Gnosis
        // activates EIP-1283 at Constantinople (block 1604400).
        if spec_id == SpecId::CONSTANTINOPLE {
            use revm::bytecode::opcode::SSTORE;
            use revm::interpreter::Instruction;
            evm.instruction.insert_instruction(
                SSTORE,
                Instruction::new(crate::evm::gnosis_evm::sstore_eip1283, 0),
            );
        }

        GnosisEvm {
            inner: super::gnosis_evm::GnosisEvm(evm, self.fee_collector_address),
            inspect: true,
        }
    }
}
