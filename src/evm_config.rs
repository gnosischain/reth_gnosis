use alloy_consensus::{BlockHeader, Header};
use alloy_primitives::{Address, U256};
use reth_ethereum_primitives::Block;
use reth_evm::eth::{EthBlockExecutionCtx, EthBlockExecutorFactory};
use reth_evm::{EvmFactory, FromRecoveredTx, TransactionEnv};
use reth_primitives::EthPrimitives;
use reth_primitives_traits::{SealedBlock, SealedHeader};

use revm::context::result::EVMError;
use revm::context::{BlockEnv, Cfg, CfgEnv};
use revm_primitives::hardfork::SpecId;
use revm_primitives::Bytes;
use core::fmt::Debug;
use std::borrow::Cow;
use reth_chainspec::{ChainSpec, EthereumHardforks};
use reth_evm::{env::EvmEnv, ConfigureEvm, Database, NextBlockEnvAttributes};
use reth_evm_ethereum::{revm_spec, revm_spec_by_timestamp_and_block_number, EthBlockAssembler, RethReceiptBuilder};
use reth_primitives::{transaction::FillTxEnv, TransactionSigned};
use std::{convert::Infallible, sync::Arc};

use crate::blobs::{evm_env_blob_schedule, get_blob_params, next_blob_gas_and_price};
use crate::block::GnosisBlockExecutorFactory;
use crate::build::GnosisBlockAssembler;
use crate::spec::GnosisChainSpec;
use crate::evm::evm::GnosisEvmFactory;

/// Returns a configuration environment for the EVM based on the given chain specification and timestamp.
pub fn get_cfg_env(chain_spec: &GnosisChainSpec, spec: SpecId, timestamp: u64) -> CfgEnv {
    let mut cfg = CfgEnv::new().with_chain_id(chain_spec.chain().id()).with_spec(spec);
    cfg.set_blob_max_and_target_count(evm_env_blob_schedule());
    // let mut cfg = cfg.;
    if !chain_spec.is_shanghai_active_at_timestamp(timestamp) {
        // EIP-170 is enabled at the Shanghai Fork on Gnosis Chain
        cfg.limit_contract_code_size = Some(usize::MAX);
    }
    cfg
}

/// Custom EVM configuration
#[derive(Debug, Clone)]
pub struct GnosisEvmConfig {
    /// Inner [`GnosisBlockExecutorFactory`].
    pub executor_factory: GnosisBlockExecutorFactory<RethReceiptBuilder, Arc<GnosisChainSpec>, GnosisEvmFactory>,
    /// Ethereum block assembler.
    pub block_assembler: GnosisBlockAssembler<GnosisChainSpec>,

    pub collector_address: Address,
    chain_spec: Arc<GnosisChainSpec>,
}

impl GnosisEvmConfig {
    /// Creates a new [`GnosisEvmConfig`] with the given chain spec.
    pub fn new(chain_spec: Arc<GnosisChainSpec>) -> Self {
        let collector_address = chain_spec
            .genesis()
            .config
            .extra_fields
            .get("eip1559collector")
            .expect("no eip1559collector field");
        let collector_address: Address = serde_json::from_value(collector_address.clone())
            .expect("failed to parse eip1559collector field");
        
        Self {
            block_assembler: GnosisBlockAssembler::new(chain_spec.clone()),
            executor_factory: GnosisBlockExecutorFactory::new(
                RethReceiptBuilder::default(),
                chain_spec.clone(),
                GnosisEvmFactory::default(),
            ),
            collector_address,
            chain_spec,
        }
    }

    /// Returns the chain spec associated with this configuration.
    pub fn chain_spec(&self) -> &GnosisChainSpec {
        &self.chain_spec
    }

    /// Sets the extra data for the block assembler.
    pub fn with_extra_data(mut self, extra_data: Bytes) -> Self {
        self.block_assembler.extra_data = extra_data;
        self
    }
}

impl ConfigureEvm for GnosisEvmConfig
{
    type Primitives = EthPrimitives;
    type Error = Infallible;
    type NextBlockEnvCtx = NextBlockEnvAttributes;
    type BlockExecutorFactory = GnosisBlockExecutorFactory<RethReceiptBuilder, Arc<GnosisChainSpec>, GnosisEvmFactory>;
    type BlockAssembler = GnosisBlockAssembler<GnosisChainSpec>;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        &self.executor_factory
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        &self.block_assembler
    }

    fn evm_env(&self, header: &Header) -> EvmEnv {
        let spec = revm_spec(self.chain_spec(), header);
        //disab dbg!("debjit debug > spec in evm_env: {:?}", spec);

        // configure evm env based on parent block
        let cfg_env = get_cfg_env(self.chain_spec(), spec, header.timestamp);

        let block_env = BlockEnv {
            number: header.number(),
            beneficiary: header.beneficiary(),
            timestamp: header.timestamp(),
            difficulty: if spec >= SpecId::MERGE { U256::ZERO } else { header.difficulty() },
            prevrandao: if spec >= SpecId::MERGE { header.mix_hash() } else { None },
            gas_limit: header.gas_limit(),
            basefee: header.base_fee_per_gas().unwrap_or_default(),
            // EIP-4844 excess blob gas of this block, introduced in Cancun
            blob_excess_gas_and_price: header.excess_blob_gas.map(|excess_blob_gas| {
                next_blob_gas_and_price(excess_blob_gas, spec >= SpecId::PRAGUE)
            }),
        };

        EvmEnv { cfg_env, block_env }
    }

    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &NextBlockEnvAttributes,
    ) -> Result<EvmEnv, Self::Error> {
        // ensure we're not missing any timestamp based hardforks
        let spec_id = revm_spec_by_timestamp_and_block_number(
            self.chain_spec(),
            attributes.timestamp,
            parent.number() + 1,
        );
        //disab dbg!("debjit debug > spec in next_evm_env: {:?}", spec_id);
        
        // configure evm env based on parent block
        let cfg = get_cfg_env(&self.chain_spec, spec_id,  attributes.timestamp);
        //disab dbg!("debjit debug > spec in next_evm_env: {:?}", cfg.spec());

        let blob_params = get_blob_params(spec_id >= SpecId::PRAGUE);

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

        let gas_limit = attributes.gas_limit;

        let block_env = BlockEnv {
            number: parent.number + 1,
            beneficiary: attributes.suggested_fee_recipient,
            timestamp: attributes.timestamp,
            difficulty: U256::ZERO,
            prevrandao: Some(attributes.prev_randao),
            gas_limit,
            // calculate basefee based on parent block's gas usage
            basefee: basefee.unwrap_or_default(),
            // calculate excess gas based on parent block's blob gas usage
            blob_excess_gas_and_price,
        };

        Ok((cfg, block_env).into())
    }

    fn context_for_block<'a>(&self, block: &'a SealedBlock<Block>) -> EthBlockExecutionCtx<'a> {
        EthBlockExecutionCtx {
            parent_hash: block.header().parent_hash,
            parent_beacon_block_root: block.header().parent_beacon_block_root,
            ommers: &block.body().ommers,
            withdrawals: block.body().withdrawals.as_ref().map(Cow::Borrowed),
        }
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader,
        attributes: Self::NextBlockEnvCtx,
    ) -> EthBlockExecutionCtx<'_> {
        EthBlockExecutionCtx {
            parent_hash: parent.hash(),
            parent_beacon_block_root: attributes.parent_beacon_block_root,
            ommers: &[],
            withdrawals: attributes.withdrawals.map(Cow::Owned),
        }
    }
    // modifications to EIP-1559 gas accounting handler has been moved to Handler in gnosis_evm.rs
    // ConfigureEvmEnv and BlockExecutionStrategyFactory traits are merged into a single ConfigureEvm trait
}
