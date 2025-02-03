use alloy_consensus::{BlockHeader, Header};
use alloy_primitives::{Address, U256};
use core::fmt::Debug;
use reth::revm::{inspector_handle_register, Database, GetInspector};
use reth_chainspec::EthereumHardforks;
use reth_evm::{ConfigureEvm, ConfigureEvmEnv, EvmEnv, NextBlockEnvAttributes};
use reth_evm_ethereum::{revm_spec, revm_spec_by_timestamp_and_block_number, EthEvm};
use reth_primitives::{transaction::FillTxEnv, TransactionSigned};
use revm::EvmBuilder;
use revm::{
    handler::mainnet::reward_beneficiary as reward_beneficiary_mainnet, interpreter::Gas, Context,
};
use revm_primitives::{
    spec_to_generic, AnalysisKind, BlockEnv, CfgEnv, CfgEnvWithHandlerCfg, EVMError, HandlerCfg,
    Spec, SpecId, TxEnv,
};
use std::{convert::Infallible, sync::Arc};

use crate::blobs::{next_blob_gas_and_price, CANCUN_BLOB_PARAMS, PRAGUE_BLOB_PARAMS};
use crate::spec::GnosisChainSpec;

/// Reward beneficiary with gas fee.
#[inline]
pub fn reward_beneficiary<SPEC: Spec, EXT, DB: Database>(
    context: &mut Context<EXT, DB>,
    gas: &Gas,
    collector_address: Address,
) -> Result<(), EVMError<DB::Error>> {
    reward_beneficiary_mainnet::<SPEC, EXT, DB>(context, gas)?;
    if SPEC::enabled(SpecId::LONDON) {
        mint_basefee_to_collector_address::<EXT, DB>(context, gas, collector_address)?;
    }
    Ok(())
}

/// Mint basefee to eip1559 collector
#[inline]
pub fn mint_basefee_to_collector_address<EXT, DB: Database>(
    context: &mut Context<EXT, DB>,
    gas: &Gas,
    collector_address: Address,
) -> Result<(), EVMError<DB::Error>> {
    // TODO: Define a per-network collector address configurable via genesis file
    let base_fee = context.evm.env.block.basefee;
    let gas_used = U256::from(gas.spent() - gas.refunded() as u64);

    let collector_account = context
        .evm
        .inner
        .journaled_state
        .load_account(collector_address, &mut context.evm.inner.db)?
        .data;

    collector_account.mark_touch();
    collector_account.info.balance = collector_account
        .info
        .balance
        .saturating_add(base_fee * gas_used);

    Ok(())
}

/// Returns a configuration environment for the EVM based on the given chain specification and timestamp.
pub fn get_cfg_env(chain_spec: &GnosisChainSpec, timestamp: u64) -> CfgEnv {
    let mut cfg = CfgEnv::default().with_chain_id(chain_spec.chain().id());
    if !chain_spec.is_shanghai_active_at_timestamp(timestamp) {
        // EIP-170 is enabled at the Shanghai Fork on Gnosis Chain
        cfg.limit_contract_code_size = Some(usize::MAX);
    }
    cfg
}

/// Custom EVM configuration
#[derive(Debug, Clone)]
pub struct GnosisEvmConfig {
    pub collector_address: Address,
    chain_spec: Arc<GnosisChainSpec>,
}

impl GnosisEvmConfig {
    /// Creates a new [`GnosisEvmConfig`] with the given chain spec.
    pub const fn new(collector_address: Address, chain_spec: Arc<GnosisChainSpec>) -> Self {
        Self {
            collector_address,
            chain_spec,
        }
    }

    /// Returns the chain spec associated with this configuration.
    pub fn chain_spec(&self) -> &GnosisChainSpec {
        &self.chain_spec
    }
}

impl ConfigureEvm for GnosisEvmConfig {
    type Evm<'a, DB: Database + 'a, I: 'a> = EthEvm<'a, I, DB>;

    fn evm_with_env<DB: Database>(
        &self,
        db: DB,
        evm_env: reth_evm::EvmEnv<Self::Spec>,
    ) -> Self::Evm<'_, DB, ()> {
        let collector_address = self.collector_address;
        let cfg_env_with_handler_cfg = CfgEnvWithHandlerCfg {
            cfg_env: evm_env.cfg_env,
            handler_cfg: HandlerCfg::new(evm_env.spec),
        };

        EvmBuilder::default()
            .with_db(db)
            .with_cfg_env_with_handler_cfg(cfg_env_with_handler_cfg)
            .with_block_env(evm_env.block_env)
            .append_handler_register_box(Box::new(move |h| {
                spec_to_generic!(h.spec_id(), {
                    h.post_execution.reward_beneficiary = Arc::new(move |context, gas| {
                        reward_beneficiary::<SPEC, (), DB>(context, gas, collector_address)
                    });
                });
            }))
            .build()
            .into()
    }

    fn evm_with_env_and_inspector<DB, I>(
        &self,
        db: DB,
        evm_env: reth_evm::EvmEnv<Self::Spec>,
        inspector: I,
    ) -> Self::Evm<'_, DB, I>
    where
        DB: Database,
        I: GetInspector<DB>,
    {
        let collector_address = self.collector_address;
        let cfg_env_with_handler_cfg = CfgEnvWithHandlerCfg {
            cfg_env: evm_env.cfg_env,
            handler_cfg: HandlerCfg::new(evm_env.spec),
        };

        EvmBuilder::default()
            .with_db(db)
            .with_external_context(inspector)
            .with_cfg_env_with_handler_cfg(cfg_env_with_handler_cfg)
            .with_block_env(evm_env.block_env)
            .append_handler_register_box(Box::new(move |h| {
                spec_to_generic!(h.spec_id(), {
                    h.post_execution.reward_beneficiary = Arc::new(move |context, gas| {
                        reward_beneficiary::<SPEC, I, DB>(context, gas, collector_address)
                    });
                });
            }))
            .append_handler_register(inspector_handle_register)
            .build()
            .into()
    }
}

impl ConfigureEvmEnv for GnosisEvmConfig {
    type Header = Header;
    type Transaction = TransactionSigned;
    type Error = Infallible;
    type TxEnv = TxEnv;
    type Spec = SpecId;

    fn tx_env(&self, transaction: &TransactionSigned, sender: Address) -> Self::TxEnv {
        let mut tx_env = TxEnv::default();
        transaction.fill_tx_env(&mut tx_env, sender);
        tx_env
    }

    fn evm_env(&self, header: &Self::Header) -> EvmEnv {
        let spec = revm_spec(self.chain_spec(), header);

        let mut cfg_env = get_cfg_env(self.chain_spec(), header.timestamp);
        cfg_env.chain_id = self.chain_spec.chain().id();
        cfg_env.perf_analyse_created_bytecodes = AnalysisKind::default();

        let block_env = BlockEnv {
            number: U256::from(header.number()),
            coinbase: header.beneficiary(),
            timestamp: U256::from(header.timestamp()),
            difficulty: if spec >= SpecId::MERGE {
                U256::ZERO
            } else {
                header.difficulty()
            },
            prevrandao: if spec >= SpecId::MERGE {
                header.mix_hash()
            } else {
                None
            },
            gas_limit: U256::from(header.gas_limit()),
            basefee: U256::from(header.base_fee_per_gas().unwrap_or_default()),
            // EIP-4844 excess blob gas of this block, introduced in Cancun
            blob_excess_gas_and_price: header.excess_blob_gas.map(|excess_blob_gas| {
                next_blob_gas_and_price(excess_blob_gas, spec >= SpecId::PRAGUE)
            }),
        };

        EvmEnv {
            cfg_env,
            spec,
            block_env,
        }
    }

    fn next_evm_env(
        &self,
        parent: &Self::Header,
        attributes: NextBlockEnvAttributes,
    ) -> Result<EvmEnv, Self::Error> {
        // configure evm env based on parent block
        // let cfg = CfgEnv::default().with_chain_id(self.chain_spec.chain().id());
        let cfg = get_cfg_env(&self.chain_spec, attributes.timestamp);

        // ensure we're not missing any timestamp based hardforks
        let spec_id = revm_spec_by_timestamp_and_block_number(
            &self.chain_spec,
            attributes.timestamp,
            parent.number() + 1,
        );

        let blob_params = if spec_id >= SpecId::PRAGUE {
            PRAGUE_BLOB_PARAMS
        } else {
            CANCUN_BLOB_PARAMS
        };

        // if the parent block did not have excess blob gas (i.e. it was pre-cancun), but it is
        // cancun now, we need to set the excess blob gas to the default value(0)
        let blob_excess_gas_and_price = parent
            .next_block_excess_blob_gas(blob_params)
            .or_else(|| (spec_id == SpecId::CANCUN).then_some(0))
            .map(|gas| next_blob_gas_and_price(gas, spec_id >= SpecId::PRAGUE));

        let basefee = parent.next_block_base_fee(
            self.chain_spec
                .base_fee_params_at_timestamp(attributes.timestamp),
        );

        let gas_limit = U256::from(attributes.gas_limit);

        let block_env = BlockEnv {
            number: U256::from(parent.number + 1),
            coinbase: attributes.suggested_fee_recipient,
            timestamp: U256::from(attributes.timestamp),
            difficulty: U256::ZERO,
            prevrandao: Some(attributes.prev_randao),
            gas_limit,
            // calculate basefee based on parent block's gas usage
            basefee: basefee.map(U256::from).unwrap_or_default(),
            // calculate excess gas based on parent block's blob gas usage
            blob_excess_gas_and_price,
        };

        Ok((
            CfgEnvWithHandlerCfg::new_with_spec_id(cfg, spec_id),
            block_env,
        )
            .into())
    }
}
