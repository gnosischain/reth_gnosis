//! EIP-1283 SSTORE net gas metering for Gnosis's Constantinople window.
//!
//! Ethereum mainnet activated Constantinople and Petersburg at the same block, so EIP-1283 never
//! went live there and evm2 has no Constantinople spec. Gnosis ran a real
//! `[constantinopleBlock, petersburgBlock)` window (blocks 1_604_400..2_508_800 on mainnet) with
//! EIP-1283 active. Blocks in that window execute on evm2's `PETERSBURG` spec (Constantinople
//! minus EIP-1283) with two changes that reinstate EIP-1283:
//!
//! - [`apply_gas_params`] sets the SSTORE gas parameters to EIP-1283's schedule.
//! - [`execution_config`] swaps in an opcode table whose SSTORE uses net gas metering.

use evm2::{
    interpreter::{op, Host, InstrStop},
    version::{GasId, GasParams},
    EvmConfig, ExecutionConfig, OpcodeConfig, SpecId, Version,
};
use evm2_macros::instruction;

use super::GnosisEvmTypes;

/// EIP-1283 `SLOAD_GAS`: the base cost of every SSTORE.
const EIP1283_SLOAD_GAS: u32 = 200;
/// Cost of setting a zero slot to non-zero (`SSTORE_SET_GAS`).
const SSTORE_SET_GAS: u32 = 20_000;
/// Cost of changing a non-zero slot (`SSTORE_RESET_GAS`).
const SSTORE_RESET_GAS: u32 = 5_000;

/// Sets `gas_params` to EIP-1283's SSTORE schedule.
///
/// evm2 charges `SstoreStatic` up front and the set/reset costs on top, so the set/reset costs and
/// the matching refunds exclude the 200 gas base cost. The clearing refund (15_000) is the same
/// as Petersburg's and is left unchanged.
pub(crate) const fn apply_gas_params(gas_params: &mut GasParams) {
    gas_params.set(GasId::SstoreStatic, EIP1283_SLOAD_GAS);
    gas_params.set(
        GasId::SstoreSetWithoutLoadCost,
        SSTORE_SET_GAS - EIP1283_SLOAD_GAS,
    );
    gas_params.set(
        GasId::SstoreResetWithoutColdLoadCost,
        SSTORE_RESET_GAS - EIP1283_SLOAD_GAS,
    );
    gas_params.set(GasId::SstoreSetRefund, SSTORE_SET_GAS - EIP1283_SLOAD_GAS);
    gas_params.set(
        GasId::SstoreResetRefund,
        SSTORE_RESET_GAS - EIP1283_SLOAD_GAS,
    );
}

/// Returns the execution config for a block in the EIP-1283 window.
///
/// `version` must come from the `PETERSBURG` spec, with [`apply_gas_params`] applied.
pub(crate) fn execution_config(version: Version) -> ExecutionConfig<GnosisEvmTypes> {
    ExecutionConfig::for_config::<Eip1283Config>().with_version(version)
}

/// `PETERSBURG` opcode table with the EIP-1283 SSTORE.
struct Eip1283Config(());

impl EvmConfig<GnosisEvmTypes> for Eip1283Config {
    const BASE_SPEC_ID: SpecId = SpecId::PETERSBURG;
    const OPCODE_CONFIG: &'static OpcodeConfig<GnosisEvmTypes> = &eip1283_opcode_config();
}

const fn eip1283_opcode_config() -> OpcodeConfig<GnosisEvmTypes> {
    let mut config = OpcodeConfig::<GnosisEvmTypes>::base::<Eip1283Config>();
    config.set_instruction::<sstore_eip1283>(op::SSTORE, config.static_gas(op::SSTORE));
    config
}

/// SSTORE with EIP-1283 net gas metering.
///
/// This is evm2's standard SSTORE with net metering forced on, minus the EIP-2200 rule that
/// SSTORE fails when only the 2300 gas call stipend is left: EIP-1283 predates EIP-2200 and has
/// no such check. It also predates EIP-2929, so there are no cold-slot charges.
#[instruction(EvmTypes = GnosisEvmTypes, dynamic_gas)]
fn sstore_eip1283(cx: _, [key, value]: [Word]) -> Result {
    if cx.state.is_static() {
        return Err(InstrStop::StateChangeDuringStaticCall);
    }
    cx.gas
        .spend(cx.state.gas_params().get(GasId::SstoreStatic))?;
    let destination = &cx.state.message().destination;
    let slot = cx
        .state
        .host()
        .sstore(destination, key, value, false)
        .map_err(|error| cx.state.fail(error))?;
    cx.gas
        .spend(cx.state.gas_params().sstore_dynamic_gas(true, &slot))?;
    cx.gas
        .record_refund(cx.state.gas_params().sstore_refund(true, &slot));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gas_params_match_eip1283_schedule() {
        let mut version = Version::new(SpecId::PETERSBURG);
        apply_gas_params(&mut version.gas_params);
        let gp = &version.gas_params;

        // EIP-1283: 200 for every SSTORE, 20_000 for a fresh slot, 5_000 for a clean
        // non-zero slot.
        assert_eq!(gp.get(GasId::SstoreStatic), 200);
        assert_eq!(
            gp.get(GasId::SstoreStatic) + gp.get(GasId::SstoreSetWithoutLoadCost),
            20_000
        );
        assert_eq!(
            gp.get(GasId::SstoreStatic) + gp.get(GasId::SstoreResetWithoutColdLoadCost),
            5_000
        );
        assert_eq!(gp.get(GasId::SstoreSetRefund), 19_800);
        assert_eq!(gp.get(GasId::SstoreResetRefund), 4_800);
        assert_eq!(gp.get(GasId::SstoreClearingSlotRefund), 15_000);
    }

    #[test]
    fn opcode_table_overrides_only_sstore() {
        let config = execution_config(Version::new(SpecId::PETERSBURG));
        assert_eq!(config.base_spec_id(), SpecId::PETERSBURG);
        assert!(!config.has_base_opcode_table());
    }
}
