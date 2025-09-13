use alloy_consensus::{BlockHeader, Header};
use alloy_primitives::{Address, U256};
use reth::rpc::types::engine::ExecutionData;
use reth_ethereum_primitives::Block;
use reth_evm::eth::EthBlockExecutionCtx;
use reth_evm::{ConfigureEngineEvm, EvmEnvFor, ExecutableTxIterator, ExecutionCtxFor};
use reth_primitives::TxTy;
use reth_primitives_traits::constants::MAX_TX_GAS_LIMIT_OSAKA;
use reth_primitives_traits::{SealedBlock, SealedHeader, SignedTransaction};
use reth_provider::errors::any::AnyError;
use revm::context_interface::block::BlobExcessGasAndPrice;

use alloy_eips::Decodable2718;
use core::fmt::Debug;
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_evm::{env::EvmEnv, ConfigureEvm, NextBlockEnvAttributes};
use reth_evm_ethereum::{revm_spec, revm_spec_by_timestamp_and_block_number, RethReceiptBuilder};
use revm::context::{BlockEnv, CfgEnv};
use revm_primitives::hardfork::SpecId;
use revm_primitives::Bytes;
use std::borrow::Cow;
use std::{convert::Infallible, sync::Arc};

use crate::blobs::CANCUN_BLOB_PARAMS;
use crate::block::GnosisBlockExecutorFactory;
use crate::build::GnosisBlockAssembler;
use crate::evm::factory::GnosisEvmFactory;
use crate::primitives::GnosisNodePrimitives;
use crate::spec::gnosis_spec::GnosisChainSpec;

/// Returns a configuration environment for the EVM based on the given chain specification and timestamp.
pub fn get_cfg_env(chain_spec: &GnosisChainSpec, spec: SpecId, timestamp: u64) -> CfgEnv {
    let mut cfg = CfgEnv::new()
        .with_chain_id(chain_spec.chain().id())
        .with_spec(spec);

    if !chain_spec.is_shanghai_active_at_timestamp(timestamp) {
        // EIP-170 is enabled at the Shanghai Fork on Gnosis Chain
        cfg.limit_contract_code_size = Some(usize::MAX);
    }
    cfg
}

// REF: https://github.com/paradigmxyz/reth/blob/d3b299754fe79b051bec022e67e922f6792f2a17/crates/ethereum/evm/src/lib.rs#L54
/// Custom EVM configuration
#[derive(Debug, Clone)]
pub struct GnosisEvmConfig {
    /// Inner [`GnosisBlockExecutorFactory`].
    pub executor_factory:
        GnosisBlockExecutorFactory<RethReceiptBuilder, Arc<GnosisChainSpec>, GnosisEvmFactory>,
    /// Ethereum block assembler.
    pub block_assembler: GnosisBlockAssembler<GnosisChainSpec>,
    /// Spec.
    chain_spec: Arc<GnosisChainSpec>,
}

impl GnosisEvmConfig {
    /// Creates a new [`GnosisEvmConfig`] with the given chain spec.
    pub fn new(chain_spec: Arc<GnosisChainSpec>) -> Self {
        // Parsing fields MANDATORY for GnosisBlockExecutorFactory
        let fee_collector_address = chain_spec
            .genesis()
            .config
            .extra_fields
            .get("eip1559collector")
            .expect("no eip1559collector field");
        let fee_collector_address: Address = serde_json::from_value(fee_collector_address.clone())
            .expect("failed to parse eip1559collector field");

        let block_rewards_address = chain_spec
            .genesis()
            .config
            .extra_fields
            .get("blockRewardsContract")
            .expect("no eip1559collector field");
        let block_rewards_address: Address = serde_json::from_value(block_rewards_address.clone())
            .expect("failed to parse eip1559collector field");

        Self {
            block_assembler: GnosisBlockAssembler::new(chain_spec.clone()),
            executor_factory: GnosisBlockExecutorFactory::new(
                RethReceiptBuilder::default(),
                chain_spec.clone(),
                GnosisEvmFactory {
                    fee_collector_address,
                },
                block_rewards_address,
            ),
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

impl ConfigureEvm for GnosisEvmConfig {
    type Primitives = GnosisNodePrimitives;
    type Error = Infallible;
    type NextBlockEnvCtx = NextBlockEnvAttributes;
    type BlockExecutorFactory =
        GnosisBlockExecutorFactory<RethReceiptBuilder, Arc<GnosisChainSpec>, GnosisEvmFactory>;
    type BlockAssembler = GnosisBlockAssembler<GnosisChainSpec>;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        &self.executor_factory
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        &self.block_assembler
    }

    fn evm_env(&self, header: &Header) -> EvmEnv {
        let blob_params = self.chain_spec().blob_params_at_timestamp(header.timestamp);
        let spec = revm_spec(self.chain_spec(), header);

        // configure evm env based on parent block
        let mut cfg_env = get_cfg_env(self.chain_spec(), spec, header.timestamp);

        if let Some(blob_params) = &blob_params {
            cfg_env.set_max_blobs_per_tx(blob_params.max_blobs_per_tx);
        }

        if self
            .chain_spec()
            .is_osaka_active_at_timestamp(header.timestamp)
        {
            cfg_env.tx_gas_limit_cap = Some(MAX_TX_GAS_LIMIT_OSAKA);
        }

        // derive the EIP-4844 blob fees from the header's `excess_blob_gas` and the current
        // blobparams
        let blob_excess_gas_and_price =
            header
                .excess_blob_gas
                .zip(blob_params)
                .map(|(excess_blob_gas, params)| {
                    let blob_gasprice = params.calc_blob_fee(excess_blob_gas);
                    BlobExcessGasAndPrice {
                        excess_blob_gas,
                        blob_gasprice,
                    }
                });

        let block_env = BlockEnv {
            number: U256::from(header.number()),
            beneficiary: header.beneficiary(),
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
            gas_limit: header.gas_limit(),
            basefee: header.base_fee_per_gas().unwrap_or_default(),
            blob_excess_gas_and_price,
        };

        EvmEnv { cfg_env, block_env }
    }

    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &NextBlockEnvAttributes,
    ) -> Result<EvmEnv, Self::Error> {
        // ensure we're not missing any timestamp based hardforks
        let chain_spec = self.chain_spec();
        let blob_params = chain_spec.blob_params_at_timestamp(attributes.timestamp);
        let spec_id = revm_spec_by_timestamp_and_block_number(
            chain_spec,
            attributes.timestamp,
            parent.number() + 1,
        );

        // configure evm env based on parent block
        let mut cfg = get_cfg_env(&self.chain_spec, spec_id, attributes.timestamp);

        if let Some(blob_params) = &blob_params {
            cfg.set_max_blobs_per_tx(blob_params.max_blobs_per_tx);
        }

        if self
            .chain_spec()
            .is_osaka_active_at_timestamp(attributes.timestamp)
        {
            cfg.tx_gas_limit_cap = Some(MAX_TX_GAS_LIMIT_OSAKA);
        }

        // if the parent block did not have excess blob gas (i.e. it was pre-cancun), but it is
        // cancun now, we need to set the excess blob gas to the default value(0)
        let blob_excess_gas_and_price = parent
            .maybe_next_block_excess_blob_gas(blob_params)
            .or_else(|| (spec_id == SpecId::CANCUN).then_some(0))
            .map(|excess_blob_gas| {
                let blob_gasprice = blob_params
                    .unwrap_or_else(|| CANCUN_BLOB_PARAMS)
                    .calc_blob_fee(excess_blob_gas);
                BlobExcessGasAndPrice {
                    excess_blob_gas,
                    blob_gasprice,
                }
            });

        let basefee = chain_spec.next_block_base_fee(parent, attributes.timestamp);

        let gas_limit = attributes.gas_limit;

        let block_env = BlockEnv {
            number: U256::from(parent.number + 1),
            beneficiary: attributes.suggested_fee_recipient,
            timestamp: U256::from(attributes.timestamp),
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

impl ConfigureEngineEvm<ExecutionData> for GnosisEvmConfig {
    fn evm_env_for_payload(&self, payload: &ExecutionData) -> EvmEnvFor<Self> {
        let timestamp = payload.payload.timestamp();
        let block_number = payload.payload.block_number();

        let blob_params = self.chain_spec().blob_params_at_timestamp(timestamp);
        let spec =
            revm_spec_by_timestamp_and_block_number(self.chain_spec(), timestamp, block_number);

        // configure evm env based on parent block
        let mut cfg_env = get_cfg_env(self.chain_spec(), spec, timestamp);

        if let Some(blob_params) = &blob_params {
            cfg_env.set_max_blobs_per_tx(blob_params.max_blobs_per_tx);
        }

        if self.chain_spec().is_osaka_active_at_timestamp(timestamp) {
            cfg_env.tx_gas_limit_cap = Some(MAX_TX_GAS_LIMIT_OSAKA);
        }

        // derive the EIP-4844 blob fees from the header's `excess_blob_gas` and the current
        // blobparams
        let blob_excess_gas_and_price =
            payload
                .payload
                .excess_blob_gas()
                .zip(blob_params)
                .map(|(excess_blob_gas, params)| {
                    let blob_gasprice = params.calc_blob_fee(excess_blob_gas);
                    BlobExcessGasAndPrice {
                        excess_blob_gas,
                        blob_gasprice,
                    }
                });

        let block_env = BlockEnv {
            number: U256::from(block_number),
            beneficiary: payload.payload.fee_recipient(),
            timestamp: U256::from(timestamp),
            difficulty: if spec >= SpecId::MERGE {
                U256::ZERO
            } else {
                payload.payload.as_v1().prev_randao.into()
            },
            prevrandao: (spec >= SpecId::MERGE).then(|| payload.payload.as_v1().prev_randao),
            gas_limit: payload.payload.gas_limit(),
            basefee: payload.payload.saturated_base_fee_per_gas(),
            blob_excess_gas_and_price,
        };

        EvmEnv { cfg_env, block_env }
    }

    fn context_for_payload<'a>(&self, payload: &'a ExecutionData) -> ExecutionCtxFor<'a, Self> {
        EthBlockExecutionCtx {
            parent_hash: payload.parent_hash(),
            parent_beacon_block_root: payload.sidecar.parent_beacon_block_root(),
            ommers: &[],
            withdrawals: payload
                .payload
                .withdrawals()
                .map(|w| Cow::Owned(w.clone().into())),
        }
    }

    fn tx_iterator_for_payload(&self, payload: &ExecutionData) -> impl ExecutableTxIterator<Self> {
        payload
            .payload
            .transactions()
            .clone()
            .into_iter()
            .map(|tx| {
                let tx = TxTy::<Self::Primitives>::decode_2718_exact(tx.as_ref())
                    .map_err(AnyError::new)?;
                let signer = tx.try_recover().map_err(AnyError::new)?;
                Ok::<_, AnyError>(tx.with_signer(signer))
            })
    }
}
