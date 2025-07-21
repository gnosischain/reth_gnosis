use revm::{
    context::{
        result::{EVMError, ExecutionResult, HaltReason, InvalidTransaction, ResultAndState},
        Block, Cfg, ContextSetters, ContextTr, Evm, FrameStack, JournalTr, Transaction,
        TransactionType,
    },
    handler::{
        evm::{ContextDbError, FrameInitResult},
        instructions::InstructionProvider,
        post_execution,
        pre_execution::{self},
        EthFrame, EvmTr, EvmTrError, FrameInitOrResult, FrameResult, FrameTr, Handler,
        PrecompileProvider,
    },
    inspector::{InspectorEvmTr, InspectorHandler, JournalExt},
    interpreter::{interpreter::EthInterpreter, interpreter_action::FrameInit, InterpreterResult},
    state::EvmState,
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
    EVM: EvmTr<Context: ContextTr<Journal: JournalTr<State = EvmState>>, Frame = FRAME>,
    ERROR: EvmTrError<EVM>,
    FRAME: FrameTr<FrameResult = FrameResult, FrameInit = FrameInit>,
{
    type Evm = EVM;
    type Error = ERROR;
    type HaltReason = HaltReason;

    #[inline]
    fn validate_against_state_and_deduct_caller(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<(), Self::Error> {
        pre_execution::validate_against_state_and_deduct_caller::<EVM::Context, ERROR>(evm.ctx())?;

        // GNOSIS-SPECIFIC // START
        let spec: SpecId = evm.ctx().cfg().spec().into();
        let mut blob_gas_cost = U256::ZERO;
        // EIP-4844
        if evm.ctx().tx().tx_type() == TransactionType::Eip4844 {
            let blob_price = evm.ctx().block().blob_gasprice().unwrap_or_default();
            let blob_gas = evm.ctx().tx().total_blob_gas() as u128;
            blob_gas_cost = U256::from(blob_price).saturating_mul(U256::from(blob_gas));
        }

        if spec.is_enabled_in(SpecId::PRAGUE) {
            let fee_collector_account = evm
                .ctx()
                .journal_mut()
                .load_account(self.fee_collector)?
                .data;
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

    fn reward_beneficiary(
        &self,
        evm: &mut Self::Evm,
        exec_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        post_execution::reward_beneficiary(evm.ctx(), exec_result.gas_mut())?;
        let spec: SpecId = evm.ctx().cfg().spec().into();
        if spec.is_enabled_in(SpecId::LONDON) {
            // mint basefee to collector address
            let basefee = evm.ctx().block().basefee() as u128;
            let gas_used =
                (exec_result.gas().spent() - exec_result.gas().refunded() as u64) as u128;

            let mut collector_account = evm.ctx().journal_mut().load_account(self.fee_collector)?;
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

impl<EVM, ERROR> InspectorHandler for GnosisEvmHandler<EVM, ERROR, EthFrame<EthInterpreter>>
where
    EVM: InspectorEvmTr<
        Context: ContextTr<Journal: JournalTr<State = EvmState>>,
        Frame = EthFrame<EthInterpreter>,
        Inspector: Inspector<<<Self as Handler>::Evm as EvmTr>::Context, EthInterpreter>,
    >,
    ERROR: EvmTrError<EVM>,
{
    type IT = EthInterpreter;
}

pub struct GnosisEvm<CTX, INSP, I, P>(
    pub Evm<CTX, INSP, I, P, EthFrame<EthInterpreter>>,
    pub Address,
);

impl<CTX, INSP, I, P> EvmTr for GnosisEvm<CTX, INSP, I, P>
where
    CTX: ContextTr,
    I: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    type Context = CTX;
    type Instructions = I;
    type Precompiles = P;
    type Frame = EthFrame<EthInterpreter>;

    #[inline]
    fn ctx(&mut self) -> &mut Self::Context {
        &mut self.0.ctx
    }

    #[inline]
    fn ctx_ref(&self) -> &Self::Context {
        &self.0.ctx
    }

    #[inline]
    fn frame_stack(&mut self) -> &mut FrameStack<Self::Frame> {
        &mut self.0.frame_stack
    }

    /// Initializes the frame for the given frame input. Frame is pushed to the frame stack.
    #[inline]
    fn frame_init(
        &mut self,
        frame_input: <Self::Frame as FrameTr>::FrameInit,
    ) -> Result<FrameInitResult<'_, Self::Frame>, ContextDbError<CTX>> {
        let is_first_init = self.0.frame_stack.index().is_none();
        let new_frame = if is_first_init {
            self.0.frame_stack.start_init()
        } else {
            self.0.frame_stack.get_next()
        };

        let ctx = &mut self.0.ctx;
        let precompiles = &mut self.0.precompiles;
        let res = Self::Frame::init_with_context(new_frame, ctx, precompiles, frame_input)?;

        Ok(res.map_frame(|token| {
            if is_first_init {
                self.0.frame_stack.end_init(token);
            } else {
                self.0.frame_stack.push(token);
            }
            self.0.frame_stack.get()
        }))
    }

    /// Run the frame from the top of the stack. Returns the frame init or result.
    #[inline]
    fn frame_run(&mut self) -> Result<FrameInitOrResult<Self::Frame>, ContextDbError<CTX>> {
        let frame = self.0.frame_stack.get();
        let context = &mut self.0.ctx;
        let instructions = &mut self.0.instruction;

        let action = frame
            .interpreter
            .run_plain(instructions.instruction_table(), context);

        frame.process_next_action(context, action).inspect(|i| {
            if i.is_result() {
                frame.set_finished(true);
            }
        })
    }

    /// Returns the result of the frame to the caller. Frame is popped from the frame stack.
    #[inline]
    fn frame_return_result(
        &mut self,
        result: <Self::Frame as FrameTr>::FrameResult,
    ) -> Result<Option<<Self::Frame as FrameTr>::FrameResult>, ContextDbError<Self::Context>> {
        if self.0.frame_stack.get().is_finished() {
            self.0.frame_stack.pop();
        }
        if self.0.frame_stack.index().is_none() {
            return Ok(Some(result));
        }
        self.0
            .frame_stack
            .get()
            .return_result::<_, ContextDbError<Self::Context>>(&mut self.0.ctx, result)?;
        Ok(None)
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

impl<CTX, INSP, INST, PRECOMPILES> ExecuteEvm for GnosisEvm<CTX, INSP, INST, PRECOMPILES>
where
    CTX: ContextTr<Journal: JournalTr<State = EvmState>> + ContextSetters,
    INST: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    PRECOMPILES: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    type ExecutionResult = ExecutionResult<HaltReason>;
    type State = EvmState;
    type Error = EVMError<<CTX::Db as Database>::Error, InvalidTransaction>;
    type Tx = <CTX as ContextTr>::Tx;
    type Block = <CTX as ContextTr>::Block;

    fn transact_one(&mut self, tx: Self::Tx) -> Result<Self::ExecutionResult, Self::Error> {
        self.0.ctx.set_tx(tx);
        GnosisEvmHandler::new(self.1).run(self)
    }

    fn finalize(&mut self) -> Self::State {
        self.0.ctx.journal_mut().finalize()
    }

    fn set_block(&mut self, block: Self::Block) {
        self.0.ctx.set_block(block);
    }

    fn replay(&mut self) -> Result<ResultAndState<HaltReason>, Self::Error> {
        let mut t = GnosisEvmHandler::new(self.1);
        t.run(self).map(|result| {
            let state = self.finalize();
            ResultAndState::new(result, state)
        })
    }
}

impl<CTX, INSP, INST, PRECOMPILES> ExecuteCommitEvm for GnosisEvm<CTX, INSP, INST, PRECOMPILES>
where
    CTX: ContextTr<Journal: JournalTr<State = EvmState>, Db: DatabaseCommit> + ContextSetters,
    INST: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    PRECOMPILES: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    #[inline]
    fn commit(&mut self, state: Self::State) {
        self.0.db_mut().commit(state);
    }
}

impl<CTX, INSP, I, P> InspectorEvmTr for GnosisEvm<CTX, INSP, I, P>
where
    CTX: ContextTr<Journal: JournalExt> + ContextSetters,
    I: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
    INSP: Inspector<CTX, I::InterpreterTypes>,
{
    type Inspector = INSP;

    fn inspector(&mut self) -> &mut Self::Inspector {
        &mut self.0.inspector
    }

    fn ctx_inspector(&mut self) -> (&mut Self::Context, &mut Self::Inspector) {
        (&mut self.0.ctx, &mut self.0.inspector)
    }

    fn ctx_inspector_frame(
        &mut self,
    ) -> (&mut Self::Context, &mut Self::Inspector, &mut Self::Frame) {
        (
            &mut self.0.ctx,
            &mut self.0.inspector,
            self.0.frame_stack.get(),
        )
    }

    fn ctx_inspector_frame_instructions(
        &mut self,
    ) -> (
        &mut Self::Context,
        &mut Self::Inspector,
        &mut Self::Frame,
        &mut Self::Instructions,
    ) {
        (
            &mut self.0.ctx,
            &mut self.0.inspector,
            self.0.frame_stack.get(),
            &mut self.0.instruction,
        )
    }
}

impl<CTX, INSP, INST, PRECOMPILES> InspectEvm for GnosisEvm<CTX, INSP, INST, PRECOMPILES>
where
    CTX: ContextSetters + ContextTr<Journal: JournalTr<State = EvmState> + JournalExt>,
    INSP: Inspector<CTX, EthInterpreter>,
    INST: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    PRECOMPILES: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    type Inspector = INSP;

    fn set_inspector(&mut self, inspector: Self::Inspector) {
        self.0.inspector = inspector;
    }

    fn inspect_one_tx(&mut self, tx: Self::Tx) -> Result<Self::ExecutionResult, Self::Error> {
        self.0.set_tx(tx);
        GnosisEvmHandler::new(self.1).inspect_run(self)
    }
}
