use alloy_consensus::BlockHeader;
use alloy_primitives::{Address, B256, U256};
use gnosis_primitives::header::GnosisHeader;
use reth::rpc::types::engine::ExecutionData;
use reth_evm::{ConfigureEngineEvm, EvmEnvFor, ExecutableTxIterator, ExecutionCtxFor};
use reth_primitives::TxTy;
use reth_primitives_traits::constants::MAX_TX_GAS_LIMIT_OSAKA;
use reth_primitives_traits::{SealedBlock, SealedHeader, SignedTransaction};
use reth_provider::errors::any::AnyError;
use reth_provider::HeaderProvider;
use revm::context_interface::block::BlobExcessGasAndPrice;

use alloy_eips::Decodable2718;
use core::fmt::Debug;
use reth_chainspec::EthereumHardfork;
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_evm::{env::EvmEnv, ConfigureEvm, NextBlockEnvAttributes};
use reth_evm_ethereum::{revm_spec, revm_spec_by_timestamp_and_block_number, RethReceiptBuilder};
use revm::context::{BlockEnv, CfgEnv};
use revm_primitives::hardfork::SpecId;
use revm_primitives::Bytes;
use std::borrow::Cow;
use std::{convert::Infallible, sync::Arc};

use std::sync::Mutex;

use crate::blobs::CANCUN_BLOB_PARAMS;
use crate::block::{GnosisBlockExecutionCtx, GnosisBlockExecutorFactory};
use crate::build::GnosisBlockAssembler;
use crate::evm::factory::GnosisEvmFactory;
use crate::primitives::block::GnosisBlock;
use crate::primitives::GnosisNodePrimitives;
use crate::spec::gnosis_spec::GnosisChainSpec;

/// Compute the correct revm SpecId for a GnosisHeader.
///
/// For pre-merge (AuRa) headers, the standard `revm_spec()` returns `SpecId::MERGE` because
/// the chain spec's Paris fork condition defaults to `activation_block_number: 0` when
/// `merge_netsplit_block` is absent. This function detects pre-merge headers and returns
/// the correct pre-merge spec instead.
pub fn gnosis_revm_spec(chain_spec: &GnosisChainSpec, header: &GnosisHeader) -> SpecId {
    if header.is_pre_merge() {
        // For pre-merge headers, compute spec without Paris/TTD consideration
        let block_number = header.number;
        if chain_spec.is_london_active_at_block(block_number) {
            SpecId::LONDON
        } else if chain_spec.is_berlin_active_at_block(block_number) {
            SpecId::BERLIN
        } else if chain_spec
            .fork(EthereumHardfork::Istanbul)
            .active_at_block(block_number)
        {
            SpecId::ISTANBUL
        } else if chain_spec
            .fork(EthereumHardfork::Petersburg)
            .active_at_block(block_number)
        {
            SpecId::PETERSBURG
        } else if chain_spec
            .fork(EthereumHardfork::Constantinople)
            .active_at_block(block_number)
        {
            SpecId::CONSTANTINOPLE
        } else if chain_spec
            .fork(EthereumHardfork::Byzantium)
            .active_at_block(block_number)
        {
            SpecId::BYZANTIUM
        } else if chain_spec
            .fork(EthereumHardfork::SpuriousDragon)
            .active_at_block(block_number)
        {
            SpecId::SPURIOUS_DRAGON
        } else if chain_spec
            .fork(EthereumHardfork::Tangerine)
            .active_at_block(block_number)
        {
            SpecId::TANGERINE
        } else if chain_spec
            .fork(EthereumHardfork::Homestead)
            .active_at_block(block_number)
        {
            SpecId::HOMESTEAD
        } else {
            SpecId::FRONTIER
        }
    } else {
        revm_spec(chain_spec, header)
    }
}

/// Returns a configuration environment for the EVM based on the given chain specification and timestamp.
pub fn get_cfg_env(
    chain_spec: &GnosisChainSpec,
    spec: SpecId,
    timestamp: u64,
    is_pre_merge: bool,
) -> CfgEnv {
    let mut cfg = CfgEnv::new()
        .with_chain_id(chain_spec.chain().id())
        .with_spec_and_mainnet_gas_params(spec);

    if !chain_spec.is_shanghai_active_at_timestamp(timestamp) {
        // EIP-170 is enabled at the Shanghai Fork on Gnosis Chain
        cfg.limit_contract_code_size = Some(usize::MAX);
    }

    // Gnosis/Chiado has "service transactions" with zero gas price that bypass the basefee
    // check. Only disable for pre-merge London+ blocks where service txs exist.
    // Post-merge system calls handle basefee via transact_system_call's disable_base_fee.
    if is_pre_merge && spec >= SpecId::LONDON {
        cfg.disable_base_fee = true;
    }

    // For Gnosis Constantinople blocks (EIP-1283 active), override SSTORE gas params
    // to match EIP-1283's base cost of 200 (= SLOAD cost) instead of 5000 (SSTORE_RESET).
    // The custom sstore_eip1283 instruction forces is_istanbul=true for dynamic gas,
    // but the gas params need to match EIP-1283's formula:
    // - static (base) cost: 200 (not 5000)
    // - set cost: 20000 - 200 = 19800
    // - reset cost: 5000 - 200 = 4800
    if spec == SpecId::CONSTANTINOPLE && is_pre_merge {
        use revm::context_interface::cfg::{GasId, GasParams};
        use std::sync::Arc;
        let table = cfg.gas_params.table();
        let mut new_table = *table;
        // EIP-1283: SSTORE base cost = 200 (SLOAD cost), not 5000 (SSTORE_RESET)
        new_table[GasId::sstore_static().as_usize()] = 200;
        new_table[GasId::sstore_set_without_load_cost().as_usize()] = 20000 - 200;
        new_table[GasId::sstore_reset_without_cold_load_cost().as_usize()] = 5000 - 200;
        new_table[GasId::sstore_set_refund().as_usize()] = 20000 - 200;
        new_table[GasId::sstore_reset_refund().as_usize()] = 5000 - 200;
        cfg.set_gas_params(GasParams::new(Arc::new(new_table)));
    }

    cfg
}

/// Minimal trait for looking up block headers by hash.
/// This trait is dyn-compatible (object-safe) unlike the full `HeaderProvider`.
/// We only need the `header` method for looking up parent timestamps.
pub trait HeaderLookup: Debug + Send + Sync {
    /// Get a header by block hash.
    fn header_by_hash(&self, hash: &B256) -> Option<GnosisHeader>;
}

/// Blanket implementation of `HeaderLookup` for any `HeaderProvider`.
impl<T> HeaderLookup for T
where
    T: HeaderProvider<Header = GnosisHeader> + Debug + Send + Sync,
{
    fn header_by_hash(&self, hash: &B256) -> Option<GnosisHeader> {
        self.header(*hash).ok().flatten()
    }
}

// REF: https://github.com/paradigmxyz/reth/blob/d3b299754fe79b051bec022e67e922f6792f2a17/crates/ethereum/evm/src/lib.rs#L54
/// Custom EVM configuration
#[derive(Clone)]
pub struct GnosisEvmConfig {
    /// Inner [`GnosisBlockExecutorFactory`].
    pub executor_factory: GnosisBlockExecutorFactory<RethReceiptBuilder, GnosisEvmFactory>,
    /// Ethereum block assembler.
    pub block_assembler: GnosisBlockAssembler<GnosisChainSpec>,
    /// Spec.
    chain_spec: Arc<GnosisChainSpec>,
    /// Header lookup for getting parent block timestamps.
    header_lookup: Arc<dyn HeaderLookup>,
    /// Rolling finality tracker for AuRa consensus.
    /// Tracks validator signatures to determine when InitiateChange blocks
    /// become finalized (>50% unique validators signed).
    pub rolling_finality: Arc<Mutex<crate::aura::finality::RollingFinality>>,
}

impl Debug for GnosisEvmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GnosisEvmConfig")
            .field("executor_factory", &self.executor_factory)
            .field("block_assembler", &self.block_assembler)
            .field("chain_spec", &self.chain_spec)
            .field("header_lookup", &"<dyn HeaderLookup>")
            .finish()
    }
}

impl GnosisEvmConfig {
    /// Creates a new [`GnosisEvmConfig`] with the given chain spec and header lookup.
    pub fn new(
        chain_spec: Arc<GnosisChainSpec>,
        header_lookup: impl HeaderLookup + 'static,
    ) -> Self {
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
                (*chain_spec).clone(),
                GnosisEvmFactory {
                    fee_collector_address,
                },
                block_rewards_address,
            ),
            chain_spec,
            header_lookup: Arc::new(header_lookup),
            rolling_finality: Arc::new(Mutex::new(crate::aura::finality::RollingFinality::new(
                Vec::new(),
            ))),
        }
    }

    /// Returns the chain spec associated with this configuration.
    pub fn chain_spec(&self) -> &GnosisChainSpec {
        &self.chain_spec
    }

    /// Determine if finalizeChange() needs to be called at this block number.
    /// Returns the validator contract address if the validator set just transitioned
    /// to a contract-based type at this exact block (transition boundary only).
    ///
    /// In Nethermind, finalizeChange() is called at `InitBlockNumber` (the transition
    /// block itself), not at every subsequent block.
    fn compute_finalize_change_address(&self, block_number: u64) -> Option<Address> {
        let aura_config = self.chain_spec.aura_config.as_ref()?;

        if block_number == 0 {
            return aura_config.validators.contract_address_at(0);
        }

        // finalizeChange() is called when transitioning FROM a list-type validator
        // to a contract-type validator (e.g., block 1300→1301 on Gnosis).
        // It's NOT called when transitioning between contract types (e.g., POSDAO at 9186425).
        // For InitiateChange events, the pending_finalize mechanism handles it.
        let current_contract = aura_config.validators.contract_address_at(block_number)?;
        let parent_contract = aura_config.validators.contract_address_at(block_number - 1);

        if parent_contract != Some(current_contract) {
            // Transition block itself — don't call
            None
        } else if block_number >= 2
            && aura_config.validators.contract_address_at(block_number - 2)
                != Some(current_contract)
        {
            // First block AFTER transition. Only call finalizeChange if the PREVIOUS
            // validator was a list type (not a contract). Contract→contract transitions
            // don't need finalizeChange at the Multi boundary.
            let prev_was_list = block_number >= 2
                && aura_config
                    .validators
                    .contract_address_at(block_number - 2)
                    .is_none();
            if prev_was_list {
                Some(current_contract)
            } else {
                None
            }
        } else {
            None
        }
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
    type BlockExecutorFactory = GnosisBlockExecutorFactory<RethReceiptBuilder, GnosisEvmFactory>;
    type BlockAssembler = GnosisBlockAssembler<GnosisChainSpec>;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        &self.executor_factory
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        &self.block_assembler
    }

    fn evm_env(&self, header: &GnosisHeader) -> Result<EvmEnv, Self::Error> {
        let blob_params = self.chain_spec().blob_params_at_timestamp(header.timestamp);
        let spec = gnosis_revm_spec(self.chain_spec(), header);

        // configure evm env based on parent block
        let mut cfg_env = get_cfg_env(
            self.chain_spec(),
            spec,
            header.timestamp,
            header.is_pre_merge(),
        );

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

        Ok(EvmEnv { cfg_env, block_env })
    }

    fn next_evm_env(
        &self,
        parent: &GnosisHeader,
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
        // next_evm_env is for building the next block (post-merge only)
        let mut cfg = get_cfg_env(&self.chain_spec, spec_id, attributes.timestamp, false);

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
                    .unwrap_or(CANCUN_BLOB_PARAMS)
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

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<GnosisBlock>,
    ) -> Result<GnosisBlockExecutionCtx<'a>, Self::Error> {
        // Look up parent header to get its timestamp for hardfork activation checks
        let parent_timestamp = self
            .header_lookup
            .header_by_hash(&block.header().parent_hash)
            .map(|h| h.timestamp)
            .unwrap_or(0);

        // Determine if we need to call finalizeChange() at this block.
        // This happens at the first block of a new validator epoch (list→contract transition).
        let mut finalize_change_address =
            self.compute_finalize_change_address(block.header().number);

        // For POSDAO contract validators: check if a pending InitiateChange
        // has been finalized by the rolling finality tracker.
        if finalize_change_address.is_none() {
            if let Ok(mut rf) = self.rolling_finality.lock() {
                let block_number = block.header().number;
                if let Some(addr) = rf.take_finalize_change(block_number) {
                    tracing::info!(
                        target: "reth::gnosis",
                        block = block_number,
                        validator = %addr,
                        "Rolling finality: finalizeChange triggered"
                    );
                    finalize_change_address = Some(addr);
                }
            }
        }

        // Get the validator contract address for InitiateChange event detection
        let validator_contract = self
            .chain_spec
            .aura_config
            .as_ref()
            .and_then(|c| c.validators.contract_address_at(block.header().number));

        // Get the correct block reward contract for this block from AuRa transitions
        let block_rewards_override = self.chain_spec.aura_config.as_ref().and_then(|c| {
            c.block_reward_contract_transitions
                .range(..=block.header().number)
                .next_back()
                .map(|(_, addr)| *addr)
        });

        // AuRa pre-merge bytecode rewrites at this exact block height
        let aura_bytecode_rewrites = self
            .chain_spec
            .aura_config
            .as_ref()
            .and_then(|c| c.rewrite_bytecode.get(&block.header().number).cloned());

        Ok(GnosisBlockExecutionCtx {
            parent_hash: block.header().parent_hash,
            parent_beacon_block_root: block.header().parent_beacon_block_root,
            withdrawals: block.body().withdrawals.as_ref().map(Cow::Borrowed),
            parent_timestamp,
            finalize_change_address,
            validator_contract,
            rolling_finality: self.rolling_finality.clone(),
            posdao_transition: self
                .chain_spec
                .aura_config
                .as_ref()
                .and_then(|c| c.posdao_transition),
            block_rewards_override,
            aura_bytecode_rewrites,
        })
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader<GnosisHeader>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<GnosisBlockExecutionCtx<'_>, Self::Error> {
        let next_block = parent.number + 1;
        let validator_contract = self
            .chain_spec
            .aura_config
            .as_ref()
            .and_then(|c| c.validators.contract_address_at(next_block));
        let block_rewards_override = self.chain_spec.aura_config.as_ref().and_then(|c| {
            c.block_reward_contract_transitions
                .range(..=next_block)
                .next_back()
                .map(|(_, addr)| *addr)
        });
        let aura_bytecode_rewrites = self
            .chain_spec
            .aura_config
            .as_ref()
            .and_then(|c| c.rewrite_bytecode.get(&next_block).cloned());
        Ok(GnosisBlockExecutionCtx {
            parent_hash: parent.hash(),
            parent_beacon_block_root: attributes.parent_beacon_block_root,
            withdrawals: attributes.withdrawals.map(Cow::Owned),
            parent_timestamp: parent.timestamp,
            finalize_change_address: self.compute_finalize_change_address(next_block),
            validator_contract,
            rolling_finality: self.rolling_finality.clone(),
            posdao_transition: self
                .chain_spec
                .aura_config
                .as_ref()
                .and_then(|c| c.posdao_transition),
            block_rewards_override,
            aura_bytecode_rewrites,
        })
    }
    // modifications to EIP-1559 gas accounting handler has been moved to Handler in gnosis_evm.rs
    // ConfigureEvmEnv and BlockExecutionStrategyFactory traits are merged into a single ConfigureEvm trait
}

impl ConfigureEngineEvm<ExecutionData> for GnosisEvmConfig {
    fn evm_env_for_payload(&self, payload: &ExecutionData) -> Result<EvmEnvFor<Self>, Self::Error> {
        let timestamp = payload.payload.timestamp();
        let block_number = payload.payload.block_number();

        let blob_params = self.chain_spec().blob_params_at_timestamp(timestamp);
        let spec =
            revm_spec_by_timestamp_and_block_number(self.chain_spec(), timestamp, block_number);

        // configure evm env based on parent block
        // Payloads are always post-merge
        let mut cfg_env = get_cfg_env(self.chain_spec(), spec, timestamp, false);

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

        Ok(EvmEnv { cfg_env, block_env })
    }

    fn context_for_payload<'a>(
        &self,
        payload: &'a ExecutionData,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        // Look up parent header to get its timestamp for hardfork activation checks
        let parent_timestamp = self
            .header_lookup
            .header_by_hash(&payload.parent_hash())
            .map(|h| h.timestamp)
            .unwrap_or(0);

        Ok(GnosisBlockExecutionCtx {
            parent_hash: payload.parent_hash(),
            parent_beacon_block_root: payload.sidecar.parent_beacon_block_root(),
            withdrawals: payload
                .payload
                .withdrawals()
                .map(|w| Cow::Owned(w.clone().into())),
            parent_timestamp,
            // Payloads are post-merge — no finalizeChange needed
            finalize_change_address: None,
            validator_contract: None,
            rolling_finality: self.rolling_finality.clone(),
            posdao_transition: None,
            block_rewards_override: None,
            aura_bytecode_rewrites: None,
        })
    }

    fn tx_iterator_for_payload(
        &self,
        payload: &ExecutionData,
    ) -> Result<impl ExecutableTxIterator<Self>, Self::Error> {
        let txs = payload.payload.transactions().clone();
        let convert = |tx: Bytes| {
            let tx =
                TxTy::<Self::Primitives>::decode_2718_exact(tx.as_ref()).map_err(AnyError::new)?;
            let signer = tx.try_recover().map_err(AnyError::new)?;
            Ok::<_, AnyError>(tx.with_signer(signer))
        };

        Ok((txs, convert))
    }
}

/// A no-op header lookup that always returns None.
/// Used in CLI contexts (like stage commands) where the idempotent
/// bytecode rewrite check handles correctness.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopHeaderLookup;

impl HeaderLookup for NoopHeaderLookup {
    fn header_by_hash(&self, _hash: &B256) -> Option<GnosisHeader> {
        None
    }
}
