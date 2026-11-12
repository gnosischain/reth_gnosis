//! Construction of Gnosis evm2 instances.

use core::ops::Range;

use alloy_primitives::Address;
use evm2::{
    evm::{precompile::PrecompileProvider, DynDatabase},
    registry::TxRegistry,
    Evm, ExecutionConfig, Precompiles, SpecId, TxResult,
};
use reth_evm_ethereum::EthEvmEnv;

use super::{eip1283, handler::gnosis_tx_registry, GnosisEvmExt, GnosisEvmTypes};

/// Gnosis EVM environment.
pub type GnosisEvmEnv = EthEvmEnv<GnosisEvmTypes>;

/// Creates evm2 instances configured for a Gnosis chain.
#[derive(Debug, Clone, Default)]
pub struct GnosisEvmFactory {
    /// EIP-1559 fee collector that receives minted base and blob fees.
    pub fee_collector_address: Address,
    /// Gnosis ran EIP-1283 (SSTORE net gas metering) in the half-open
    /// `[constantinopleBlock, petersburgBlock)` window. evm2 has no Constantinople spec, so those
    /// blocks execute on `PETERSBURG` and the factory installs the EIP-1283 SSTORE for blocks in
    /// this window. `None` means never active (e.g. Chiado).
    pub eip1283_window: Option<Range<u64>>,
}

impl GnosisEvmFactory {
    /// Whether the Gnosis EIP-1283 SSTORE override applies at `block_number`.
    fn eip1283_active(&self, block_number: u64) -> bool {
        self.eip1283_window
            .as_ref()
            .is_some_and(|window| window.contains(&block_number))
    }

    /// Returns the transaction registry for `spec`.
    pub fn tx_registry(
        &self,
        spec: SpecId,
    ) -> TxRegistry<GnosisEvmTypes, TxResult<GnosisEvmTypes>> {
        gnosis_tx_registry(spec)
    }

    /// Returns the precompiles for `spec`. Gnosis uses the standard Ethereum set.
    pub fn precompiles(&self, spec: SpecId) -> Precompiles<GnosisEvmTypes> {
        Precompiles::base(spec)
    }

    /// Returns the execution config (version data and opcode table) for `env`.
    fn execution_config(&self, env: &GnosisEvmEnv) -> ExecutionConfig<GnosisEvmTypes> {
        let block_number = env.block.number.saturating_to::<u64>();
        // The window maps to PETERSBURG (see `gnosis_spec_id`); the custom table is a
        // PETERSBURG overlay, so it can only be installed on that spec.
        if env.spec == SpecId::PETERSBURG && self.eip1283_active(block_number) {
            eip1283::execution_config(env.version)
        } else {
            ExecutionConfig::for_spec_and_version(env.spec, env.version)
        }
    }

    /// Creates an EVM over `db` for `env` with the standard precompiles.
    pub fn create_evm<'a>(
        &self,
        db: impl DynDatabase + 'a,
        env: GnosisEvmEnv,
    ) -> Evm<'a, GnosisEvmTypes> {
        let precompiles = self.precompiles(env.spec);
        self.create_evm_with_precompiles(db, env, precompiles)
    }

    /// Creates an EVM over `db` for `env` with the given precompile provider.
    pub fn create_evm_with_precompiles<'a>(
        &self,
        db: impl DynDatabase + 'a,
        env: GnosisEvmEnv,
        precompiles: impl PrecompileProvider<GnosisEvmTypes> + 'a,
    ) -> Evm<'a, GnosisEvmTypes> {
        let config = self.execution_config(&env);
        Evm::new_with_execution_config_and_ext(
            config,
            env.spec,
            env.block,
            self.tx_registry(env.spec),
            db,
            precompiles,
            GnosisEvmExt {
                fee_collector: self.fee_collector_address,
            },
        )
    }
}
