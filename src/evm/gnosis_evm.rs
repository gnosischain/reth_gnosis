use revm::{context::{result::{EVMError, ExecutionResult, HaltReason, InvalidTransaction, ResultAndState}, Block, Cfg, ContextSetters, ContextTr, Evm, JournalOutput, JournalTr}, handler::{instructions::{EthInstructions, InstructionProvider}, post_execution, EthFrame, EvmTr, EvmTrError, Frame, FrameResult, Handler, PrecompileProvider}, inspector::{InspectorEvmTr, InspectorFrame, InspectorHandler, JournalExt}, interpreter::{interpreter::EthInterpreter, FrameInput, Interpreter, InterpreterAction, InterpreterResult, InterpreterTypes}, Database, DatabaseCommit, ExecuteCommitEvm, ExecuteEvm, InspectEvm, Inspector};
use revm_primitives::{address, hardfork::SpecId, Address, U256};

const FEE_COLLECTOR: Address = address!("1559000000000000000000000000000000000000");

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

    fn reward_beneficiary(
            &self,
            evm: &mut Self::Evm,
            exec_result: &mut <Self::Frame as Frame>::FrameResult,
        ) -> Result<(), Self::Error> {
            //disab dbg!("debjit debug > sending funds back to beneficiary");
            post_execution::reward_beneficiary(evm.ctx(), exec_result.gas_mut())?;
            let spec = evm.ctx().cfg().spec().into();
            if spec.is_enabled_in(SpecId::LONDON) {
                // mint basefee to collector address
                let basefee = evm.ctx().block().basefee() as u128;
                let gas_used = (exec_result.gas().spent() - exec_result.gas().refunded() as u64) as u128;

                let mut collector_account = evm.ctx().journal().load_account(self.fee_collector)?;
                collector_account.mark_touch();
                collector_account.data.info.balance = collector_account.data.info.balance.saturating_add(U256::from(basefee * gas_used));
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

pub struct GnosisEvm<CTX, INSP, I, P>(
    pub Evm<CTX, INSP, I, P>,
);

impl<CTX, INSP, I, P> EvmTr for GnosisEvm<CTX, INSP, I, P>
where
    CTX: ContextTr,
    I: InstructionProvider<
        Context = CTX,
        InterpreterTypes: InterpreterTypes<Output = InterpreterAction>,
    >,
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
        let context = &mut self.0.data.ctx;
        let instructions = &mut self.0.instruction;
        interpreter.run_plain(instructions.instruction_table(), context)
    }
    #[inline]
    fn ctx(&mut self) -> &mut Self::Context {
        &mut self.0.data.ctx
    }

    #[inline]
    fn ctx_ref(&self) -> &Self::Context {
        &self.0.data.ctx
    }

    #[inline]
    fn ctx_instructions(&mut self) -> (&mut Self::Context, &mut Self::Instructions) {
        (&mut self.0.data.ctx, &mut self.0.instruction)
    }

    #[inline]
    fn ctx_precompiles(&mut self) -> (&mut Self::Context, &mut Self::Precompiles) {
        (&mut self.0.data.ctx, &mut self.0.precompiles)
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

    fn replay(&mut self) -> Self::Output {
        //disab dbg!("debjit debug > inside replay");
        let mut t = GnosisEvmHandler::<_, _, EthFrame<_, _, _>>::new(FEE_COLLECTOR);
        t.run(self)
    }

    fn set_tx(&mut self, tx: Self::Tx) {
        self.0.data.ctx.set_tx(tx);
    }

    fn set_block(&mut self, block: Self::Block) {
        self.0.data.ctx.set_block(block);
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
{
    type Inspector = INSP;

    fn inspector(&mut self) -> &mut Self::Inspector {
        &mut self.0.data.inspector
    }

    fn ctx_inspector(&mut self) -> (&mut Self::Context, &mut Self::Inspector) {
        (&mut self.0.data.ctx, &mut self.0.data.inspector)
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
        self.0.data.inspector = inspector;
    }

    fn inspect_replay(&mut self) -> Self::Output {
        let mut h = GnosisEvmHandler::<_, _, EthFrame<_, _, _>>::new(FEE_COLLECTOR);
        h.inspect_run(self)
    }
}