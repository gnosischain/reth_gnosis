use alloy_evm::{Database, Evm};
use core::ops::{Deref, DerefMut};
use reth_evm::{eth::EthEvmContext, EvmEnv, EvmFactory};
use revm::{context::{result::{EVMError, HaltReason, ResultAndState}, BlockEnv, ContextTr, TxEnv}, handler::{instructions::EthInstructions, EthPrecompiles, EvmTr, PrecompileProvider}, inspector::NoOpInspector, interpreter::{interpreter::EthInterpreter, InterpreterResult}, Context, ExecuteEvm, InspectEvm, Inspector, MainBuilder, MainContext};
use revm_primitives::{hardfork::SpecId, Address, Bytes, TxKind, U256};

#[allow(missing_debug_implementations)] // missing revm::Context Debug impl
pub struct GnosisEvm<DB: Database, I, PRECOMPILE = EthPrecompiles> {
    inner: crate::evm::gnosis_evm::GnosisEvm<EthEvmContext<DB>, I, EthInstructions<EthInterpreter, EthEvmContext<DB>>, PRECOMPILE>,
    inspect: bool,
}

impl<DB: Database, I, PRECOMPILE> GnosisEvm<DB, I, PRECOMPILE> {
    /// Creates a new OP EVM instance.
    ///
    /// The `inspect` argument determines whether the configured [`Inspector`] of the given
    /// [`GnosisEvm`](op_revm::GnosisEvm) should be invoked on [`Evm::transact`].
    pub const fn new(
        evm: crate::evm::gnosis_evm::GnosisEvm<EthEvmContext<DB>, I, EthInstructions<EthInterpreter, EthEvmContext<DB>>, PRECOMPILE>,
        inspect: bool,
    ) -> Self {
        Self { inner: evm, inspect }
    }

    /// Consumes self and return the inner EVM instance.
    pub fn into_inner(
        self,
    ) -> super::gnosis_evm::GnosisEvm<EthEvmContext<DB>, I, EthInstructions<EthInterpreter, EthEvmContext<DB>>, PRECOMPILE>
    {
        self.inner
    }

    /// Provides a reference to the EVM context.
    pub const fn ctx(&self) -> &EthEvmContext<DB> {
        &self.inner.0.data.ctx
    }

    /// Provides a mutable reference to the EVM context.
    pub fn ctx_mut(&mut self) -> &mut EthEvmContext<DB> {
        &mut self.inner.0.data.ctx
    }

    /// Provides a mutable reference to the EVM inspector.
    pub fn inspector_mut(&mut self) -> &mut I {
        &mut self.inner.0.data.inspector
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

    fn block(&self) -> &BlockEnv {
        &self.block
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        //disab dbg!("debjit debug > self.inspect: {:?}", self.inspect);
        if self.inspect {
            self.inner.set_tx(tx);
            self.inner.inspect_replay()
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
        //disab dbg!("debjit debug > doing transact_system_call");
        let tx = TxEnv {
            caller,
            kind: TxKind::Call(contract),
            // Explicitly set nonce to 0 so revm does not do any nonce checks
            nonce: 0,
            gas_limit: 30_000_000,
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

        let mut gas_limit = tx.gas_limit;
        let mut basefee = 0;
        let mut disable_nonce_check = true;

        // ensure the block gas limit is >= the tx
        core::mem::swap(&mut self.block.gas_limit, &mut gas_limit);
        // disable the base fee check for this call by setting the base fee to zero
        core::mem::swap(&mut self.block.basefee, &mut basefee);
        // disable the nonce check
        core::mem::swap(&mut self.cfg.disable_nonce_check, &mut disable_nonce_check);

        let sp = self.cfg().spec;
        //disab dbg!("debjit debug > spec: {:?}", sp, sp.is_enabled_in(SpecId::PRAGUE));
        let cfg = self.cfg();
        //disab dbg!("debjit debug > cfgenv: {:?}", cfg);

        // //disab dbg!("debjit debug >", self.spec.is_cancun_active_at_timestamp(self.evm.block().timestamp), self.spec.is_prague_active_at_timestamp(self.evm.block().timestamp));

        //disab dbg!("debjit debug > before transact_system_call: {:?}", &tx);
        let res = self.transact(tx);
        // //disab dbg!("debjit debug > after transact_system_call: {:?}", &res);

        // swap back to the previous gas limit
        core::mem::swap(&mut self.block.gas_limit, &mut gas_limit);
        // swap back to the previous base fee
        core::mem::swap(&mut self.block.basefee, &mut basefee);
        // swap back to the previous nonce check flag
        core::mem::swap(&mut self.cfg.disable_nonce_check, &mut disable_nonce_check);

        res
    }

    fn db_mut(&mut self) -> &mut Self::DB {
        &mut self.journaled_state.database
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Context { block: block_env, cfg: cfg_env, journaled_state, .. } = self.inner.0.data.ctx;

        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }
}

/// Custom EVM configuration.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GnosisEvmFactory;

impl EvmFactory for GnosisEvmFactory {
    type Evm<DB: Database, I: Inspector<EthEvmContext<DB>>> =
        GnosisEvm<DB, I>;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Context<DB: Database> = EthEvmContext<DB>;
    type Spec = SpecId;

    fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        GnosisEvm {
            inner: super::gnosis_evm::GnosisEvm {
                0: Context::mainnet()
                    .with_db(db)
                    .with_cfg(input.cfg_env)
                    .with_block(input.block_env)
                    .build_mainnet_with_inspector(NoOpInspector {})
            },
            inspect: false,
        }
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>, EthInterpreter>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        // GnosisEvm::new(self.create_evm(db, input).into_inner().with_inspector(inspector), true)
        GnosisEvm {
            inner: super::gnosis_evm::GnosisEvm {
                0: Context::mainnet()
                    .with_db(db)
                    .with_cfg(input.cfg_env)
                    .with_block(input.block_env)
                    .build_mainnet_with_inspector(inspector)
            },
            inspect: true,
        }
    }
}
