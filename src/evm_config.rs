use reth::{
    primitives::{address, Address, ChainSpec, Header, TransactionSigned, U256},
    revm::{
        inspector_handle_register,
        interpreter::Gas,
        primitives::{spec_to_generic, CfgEnvWithHandlerCfg, EVMError, Spec, SpecId, TxEnv},
        Context, Database, Evm, EvmBuilder, GetInspector,
    },
};
use reth_evm::{ConfigureEvm, ConfigureEvmEnv};
use reth_evm_ethereum::EthEvmConfig;
use revm::handler::mainnet::reward_beneficiary as reward_beneficiary_mainnet;
use std::sync::Arc;

// TODO: Define a per-network collector address configurable via genesis file
const COLLECTOR_ADDRESS: Address = address!("6BBe78ee9e474842Dbd4AB4987b3CeFE88426A92");

/// Reward beneficiary with gas fee.
#[inline]
pub fn reward_beneficiary<SPEC: Spec, EXT, DB: Database>(
    context: &mut Context<EXT, DB>,
    gas: &Gas,
) -> Result<(), EVMError<DB::Error>> {
    reward_beneficiary_mainnet::<SPEC, EXT, DB>(context, gas)?;
    if SPEC::enabled(SpecId::LONDON) {
        mint_basefee_to_collector_address::<EXT, DB>(context, gas)?;
    }
    Ok(())
}

/// Mint basefee to eip1559 collector
#[inline]
pub fn mint_basefee_to_collector_address<EXT, DB: Database>(
    context: &mut Context<EXT, DB>,
    gas: &Gas,
) -> Result<(), EVMError<DB::Error>> {
    // TODO: Define a per-network collector address configurable via genesis file
    let base_fee = context.evm.env.block.basefee;
    let gas_used = U256::from(gas.spent() - gas.refunded() as u64);

    let (collector_account, _) = context
        .evm
        .inner
        .journaled_state
        .load_account(COLLECTOR_ADDRESS, &mut context.evm.inner.db)?;

    collector_account.mark_touch();
    collector_account.info.balance = collector_account
        .info
        .balance
        .saturating_add(base_fee * gas_used);

    Ok(())
}

/// Custom EVM configuration
#[derive(Debug, Clone, Copy, Default)]
pub struct GnosisEvmConfig;

impl ConfigureEvm for GnosisEvmConfig {
    type DefaultExternalContext<'a> = ();

    fn evm<'a, DB: Database + 'a>(&self, db: DB) -> Evm<'a, Self::DefaultExternalContext<'a>, DB> {
        EvmBuilder::default()
            .with_db(db)
            .append_handler_register_box(Box::new(|h| {
                spec_to_generic!(h.spec_id(), {
                    h.post_execution.reward_beneficiary =
                        Arc::new(reward_beneficiary::<SPEC, (), DB>);
                });
            }))
            .build()
    }

    fn evm_with_inspector<'a, DB, I>(&self, db: DB, inspector: I) -> Evm<'a, I, DB>
    where
        DB: Database + 'a,
        I: GetInspector<DB>,
    {
        EvmBuilder::default()
            .with_db(db)
            .with_external_context(inspector)
            .append_handler_register_box(Box::new(|h| {
                spec_to_generic!(h.spec_id(), {
                    h.post_execution.reward_beneficiary =
                        Arc::new(reward_beneficiary::<SPEC, I, DB>);
                });
            }))
            .append_handler_register(inspector_handle_register)
            .build()
    }
}

impl ConfigureEvmEnv for GnosisEvmConfig {
    fn fill_tx_env(tx_env: &mut TxEnv, transaction: &TransactionSigned, sender: Address) {
        EthEvmConfig::fill_tx_env(tx_env, transaction, sender)
    }

    fn fill_cfg_env(
        cfg_env: &mut CfgEnvWithHandlerCfg,
        chain_spec: &ChainSpec,
        header: &Header,
        total_difficulty: U256,
    ) {
        EthEvmConfig::fill_cfg_env(cfg_env, chain_spec, header, total_difficulty);
    }
}
