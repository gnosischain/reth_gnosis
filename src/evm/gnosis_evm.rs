use revm::{
    context::{
        result::{EVMError, ExecutionResult, HaltReason, InvalidTransaction, ResultAndState},
        Block, Cfg, ContextSetters, ContextTr, Evm, JournalOutput, JournalTr, Transaction,
        TransactionType,
    },
    handler::{
        instructions::{EthInstructions, InstructionProvider},
        post_execution,
        pre_execution::validate_account_nonce_and_code,
        EthFrame, EvmTr, EvmTrError, Frame, FrameResult, Handler, PrecompileProvider,
    },
    inspector::{InspectorEvmTr, InspectorFrame, InspectorHandler, JournalExt},
    interpreter::{
        interpreter::EthInterpreter, FrameInput, Interpreter, InterpreterAction, InterpreterResult,
        InterpreterTypes,
    },
    Database, DatabaseCommit, ExecuteCommitEvm, ExecuteEvm, InspectEvm, Inspector,
};
use revm_primitives::{hardfork::SpecId, Address, U256};

// REF 1: https://github.com/bluealloy/revm/blob/24162b7ddbf467f4541f49c3e93bcff6e704b198/book/src/framework.md
// REF 2: https://github.com/bluealloy/revm/blob/dff454328b2932937803f98adb546aa7e6f8bec2/examples/erc20_gas/src/handler.rs#L148
/// Custom EVM Handler needed due of custom `reward_beneficiary` in [`crate::evm::gnosis_evm::GnosisEvmHandler`]
/// Other traits necessary due to traitbounds.
pub struct GnosisEvmHandler<EVM, ERROR, FRAME> {
    fee_collector: Address,
    _phantom: core::marker::PhantomData<(EVM, ERROR, FRAME)>,
}

impl<CTX, ERROR, FRAME> GnosisEvmHandler<CTX, ERROR, FRAME> {
    pub fn new(fee_collector: Address) -> Self {
        Self {
            fee_collector,
            _phantom: core::marker::PhantomData,
        }
    }
}

impl<EVM, ERROR, FRAME> Handler for GnosisEvmHandler<EVM, ERROR, FRAME>
where
    EVM: EvmTr<Context: ContextTr<Journal: JournalTr<FinalOutput = JournalOutput>>>,
    FRAME: Frame<Evm = EVM, Error = ERROR, FrameResult = FrameResult, FrameInit = FrameInput>,
    ERROR: EvmTrError<EVM>,
{
    type Evm = EVM;
    type Error = ERROR;
    type Frame = FRAME;
    type HaltReason = HaltReason;

    #[inline]
    fn validate_against_state_and_deduct_caller(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<(), Self::Error> {
        // TODO: is this equivalent to pre_execution::validate_against_state_and_deduct_caller(evm.ctx())
        deduct_caller_gnosis(evm.ctx(), self.fee_collector)
    }

    fn reward_beneficiary(
        &self,
        evm: &mut Self::Evm,
        exec_result: &mut <Self::Frame as Frame>::FrameResult,
    ) -> Result<(), Self::Error> {
        post_execution::reward_beneficiary(evm.ctx(), exec_result.gas_mut())?;
        let spec: SpecId = evm.ctx().cfg().spec().into();
        if spec.is_enabled_in(SpecId::LONDON) {
            // mint basefee to collector address
            let basefee = evm.ctx().block().basefee() as u128;
            let gas_used =
                (exec_result.gas().spent() - exec_result.gas().refunded() as u64) as u128;

            let mut collector_account = evm.ctx().journal().load_account(self.fee_collector)?;
            collector_account.mark_touch();
            collector_account.data.info.balance = collector_account
                .data
                .info
                .balance
                .saturating_add(U256::from(basefee * gas_used));
        }
        Ok(())
    }
}

impl<EVM, ERROR, FRAME> InspectorHandler for GnosisEvmHandler<EVM, ERROR, FRAME>
where
    EVM: InspectorEvmTr<
        Context: ContextTr<Journal: JournalTr<FinalOutput = JournalOutput>>,
        Inspector: Inspector<<<Self as Handler>::Evm as EvmTr>::Context, EthInterpreter>,
    >,
    ERROR: EvmTrError<EVM>,
    FRAME: Frame<Evm = EVM, Error = ERROR, FrameResult = FrameResult, FrameInit = FrameInput>
        + InspectorFrame<IT = EthInterpreter>,
{
    type IT = EthInterpreter;
}

pub struct GnosisEvm<CTX, INSP, I, P>(pub Evm<CTX, INSP, I, P>, pub Address);

impl<CTX, INSP, I, P> EvmTr for GnosisEvm<CTX, INSP, I, P>
where
    CTX: ContextTr,
    I: InstructionProvider<
        Context = CTX,
        InterpreterTypes: InterpreterTypes<Output = InterpreterAction>,
    >,
    P: PrecompileProvider<CTX>,
{
    type Context = CTX;
    type Instructions = I;
    type Precompiles = P;

    #[inline]
    fn run_interpreter(
        &mut self,
        interpreter: &mut Interpreter<
            <Self::Instructions as InstructionProvider>::InterpreterTypes,
        >,
    ) -> <<Self::Instructions as InstructionProvider>::InterpreterTypes as InterpreterTypes>::Output
    {
        let context = &mut self.0.ctx;
        let instructions = &mut self.0.instruction;
        interpreter.run_plain(instructions.instruction_table(), context)
    }
    #[inline]
    fn ctx(&mut self) -> &mut Self::Context {
        &mut self.0.ctx
    }

    #[inline]
    fn ctx_ref(&self) -> &Self::Context {
        &self.0.ctx
    }

    #[inline]
    fn ctx_instructions(&mut self) -> (&mut Self::Context, &mut Self::Instructions) {
        (&mut self.0.ctx, &mut self.0.instruction)
    }

    #[inline]
    fn ctx_precompiles(&mut self) -> (&mut Self::Context, &mut Self::Precompiles) {
        (&mut self.0.ctx, &mut self.0.precompiles)
    }
}

impl<CTX, INSP, PRECOMPILES> ExecuteEvm
    for GnosisEvm<CTX, INSP, EthInstructions<EthInterpreter, CTX>, PRECOMPILES>
where
    CTX: ContextTr<Journal: JournalTr<FinalOutput = JournalOutput>> + ContextSetters,
    PRECOMPILES: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    type Output = Result<
        ResultAndState<HaltReason>,
        EVMError<<CTX::Db as Database>::Error, InvalidTransaction>,
    >;

    type Tx = <CTX as ContextTr>::Tx;

    type Block = <CTX as ContextTr>::Block;

    fn set_tx(&mut self, tx: Self::Tx) {
        self.0.ctx.set_tx(tx);
    }

    fn set_block(&mut self, block: Self::Block) {
        self.0.ctx.set_block(block);
    }

    fn replay(&mut self) -> Self::Output {
        let mut t = GnosisEvmHandler::<_, _, EthFrame<_, _, _>>::new(self.1);
        t.run(self)
    }
}

impl<CTX, INSP, PRECOMPILE> ExecuteCommitEvm
    for GnosisEvm<CTX, INSP, EthInstructions<EthInterpreter, CTX>, PRECOMPILE>
where
    CTX: ContextTr<Journal: JournalTr<FinalOutput = JournalOutput>, Db: DatabaseCommit>
        + ContextSetters,
    PRECOMPILE: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    type CommitOutput = Result<
        ExecutionResult<HaltReason>,
        EVMError<<CTX::Db as Database>::Error, InvalidTransaction>,
    >;

    fn replay_commit(&mut self) -> Self::CommitOutput {
        self.replay().map(|r| {
            self.ctx().db().commit(r.state);
            r.result
        })
    }
}

impl<CTX, INSP, I, P> InspectorEvmTr for GnosisEvm<CTX, INSP, I, P>
where
    CTX: ContextTr<Journal: JournalExt> + ContextSetters,
    I: InstructionProvider<
        Context = CTX,
        InterpreterTypes: InterpreterTypes<Output = InterpreterAction>,
    >,
    INSP: Inspector<CTX, I::InterpreterTypes>,
    P: PrecompileProvider<CTX>,
{
    type Inspector = INSP;

    fn inspector(&mut self) -> &mut Self::Inspector {
        &mut self.0.inspector
    }

    fn ctx_inspector(&mut self) -> (&mut Self::Context, &mut Self::Inspector) {
        (&mut self.0.ctx, &mut self.0.inspector)
    }

    fn run_inspect_interpreter(
        &mut self,
        interpreter: &mut Interpreter<
            <Self::Instructions as InstructionProvider>::InterpreterTypes,
        >,
    ) -> <<Self::Instructions as InstructionProvider>::InterpreterTypes as InterpreterTypes>::Output
    {
        self.0.run_inspect_interpreter(interpreter)
    }
}

impl<CTX, INSP, PRECOMPILE> InspectEvm
    for GnosisEvm<CTX, INSP, EthInstructions<EthInterpreter, CTX>, PRECOMPILE>
where
    CTX: ContextSetters + ContextTr<Journal: JournalTr<FinalOutput = JournalOutput> + JournalExt>,
    INSP: Inspector<CTX, EthInterpreter>,
    PRECOMPILE: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    type Inspector = INSP;

    fn set_inspector(&mut self, inspector: Self::Inspector) {
        self.0.inspector = inspector;
    }

    fn inspect_replay(&mut self) -> Self::Output {
        let mut h = GnosisEvmHandler::<_, _, EthFrame<_, _, _>>::new(self.1);
        h.inspect_run(self)
    }
}

// REF: https://github.com/bluealloy/revm/blob/ce9be1ffa17d394397f58d0c693f8b36016b3fc7/crates/handler/src/pre_execution.rs#L72
// Modification: Collects the blob gas fee (if pectra) after deducting from caller's account.
#[inline]
pub fn deduct_caller_gnosis<
    CTX: ContextTr,
    ERROR: From<InvalidTransaction> + From<<CTX::Db as Database>::Error>,
>(
    context: &mut CTX,
    fee_collector: Address,
) -> Result<(), ERROR> {
    let basefee = context.block().basefee() as u128;
    let blob_price = context.block().blob_gasprice().unwrap_or_default();
    let is_balance_check_disabled = context.cfg().is_balance_check_disabled();
    let is_eip3607_disabled = context.cfg().is_eip3607_disabled();
    let is_nonce_check_disabled = context.cfg().is_nonce_check_disabled();

    let spec: SpecId = context.cfg().spec().into();

    let (tx, journal) = context.tx_journal();

    // GNOSIS-SPECIFIC // START
    let mut blob_gas_cost = U256::ZERO;
    // EIP-4844
    if tx.tx_type() == TransactionType::Eip4844 {
        let blob_gas = tx.total_blob_gas() as u128;
        blob_gas_cost = U256::from(blob_price).saturating_mul(U256::from(blob_gas));
    }
    // GNOSIS-SPECIFIC // END

    // Load caller's account.
    let caller_account = journal.load_account(tx.caller())?.data;

    validate_account_nonce_and_code(
        &mut caller_account.info,
        tx.nonce(),
        tx.kind().is_call(),
        is_eip3607_disabled,
        is_nonce_check_disabled,
    )?;

    let max_balance_spending = tx.max_balance_spending()?;

    // Check if account has enough balance for `gas_limit * max_fee`` and value transfer.
    // Transfer will be done inside `*_inner` functions.
    if is_balance_check_disabled {
        // Make sure the caller's balance is at least the value of the transaction.
        caller_account.info.balance = caller_account.info.balance.max(tx.value());
    } else if max_balance_spending > caller_account.info.balance {
        return Err(InvalidTransaction::LackOfFundForMaxFee {
            fee: Box::new(max_balance_spending),
            balance: Box::new(caller_account.info.balance),
        }
        .into());
    } else {
        let effective_balance_spending = tx
            .effective_balance_spending(basefee, blob_price)
            .expect("effective balance is always smaller than max balance so it can't overflow");

        // subtracting max balance spending with value that is going to be deducted later in the call.
        let gas_balance_spending = effective_balance_spending - tx.value();

        caller_account.info.balance = caller_account
            .info
            .balance
            .saturating_sub(gas_balance_spending);
    }

    // Touch account so we know it is changed.
    caller_account.mark_touch();

    // GNOSIS-SPECIFIC // START
    if spec.is_enabled_in(SpecId::PRAGUE) {
        let fee_collector_account = journal.load_account(fee_collector)?.data;
        // Set new fee collector account balance.
        fee_collector_account.info.balance = fee_collector_account
            .info
            .balance
            .saturating_add(blob_gas_cost);

        // Touch account so we know it is changed.
        fee_collector_account.mark_touch();
    }
    // GNOSIS-SPECIFIC // END

    Ok(())
}
