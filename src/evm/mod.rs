//! Gnosis integration with the evm2 execution engine.
//!
//! Gnosis diverges from Ethereum execution in three EVM-level ways, each implemented here:
//!
//! - Transaction fees: the London base fee (and, from Prague, the blob fee) is minted to the
//!   chain's EIP-1559 fee collector instead of being burned (the `handler` module).
//! - Gnosis ran EIP-1283 SSTORE net gas metering between its Constantinople and Petersburg
//!   blocks, which evm2 has no spec for (the `eip1283` module).
//! - The fee collector address is per-chain configuration, so it travels with each EVM instance
//!   in [`GnosisEvmExt`].

mod eip1283;
pub mod factory;
mod handler;

use alloy_primitives::Address;
use evm2::{ethereum::TxEnvelope, BaseEvmConfigSelector, Evm, EvmTypesHost, SpecId};

pub(crate) use eip1283::apply_gas_params as eip1283_gas_params;
pub use factory::GnosisEvmFactory;

/// Gnosis evm2 type family.
///
/// Identical to [`evm2::BaseEvmTypes`] except that every EVM carries a [`GnosisEvmExt`], which the
/// Gnosis transaction fee hooks read the fee collector from.
#[derive(Debug, Clone, Copy)]
pub struct GnosisEvmTypes(());

impl EvmTypesHost for GnosisEvmTypes {
    type ConfigSelector = BaseEvmConfigSelector;
    type SpecId = SpecId;
    type Tx = TxEnvelope;
    type EvmExt = GnosisEvmExt;
    type MessageExt = ();
    type MessageResultExt = ();
    type TxEnvExt = ();
    type TxResultExt = ();
    type BlockEnvExt = ();
    type Host<'a> = Evm<'a, Self>;
}

/// Per-EVM Gnosis state, set by [`GnosisEvmFactory`] when it creates an EVM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GnosisEvmExt {
    /// EIP-1559 fee collector (`eip1559collector` in the chain spec) that receives the minted
    /// base fee and, from Prague, the blob fee.
    pub fee_collector: Address,
}
