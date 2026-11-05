//! Gnosis transaction fee handling.
//!
//! Gnosis mints fees that Ethereum burns to the chain's EIP-1559 fee collector
//! (<https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md>):
//!
//! - From London, `base_fee * gas_used` of every non-free transaction, after execution.
//! - From Prague, the EIP-4844 blob fee, before execution.
//!
//! The hooks wrap evm2's standard Ethereum transaction handlers, so validation, gas accounting,
//! and the priority-fee reward to the block beneficiary are unchanged.

use alloy_consensus::Transaction;
use alloy_eips::eip4844::DATA_GAS_PER_BLOB;
use alloy_primitives::{Address, U256};
use evm2::{
    ethereum::{
        charge_upfront, default_settle_gas, eip1559, eip2930, eip4844, eip7702, legacy, TxEnvelope,
    },
    handler::{GasSettlement, TxHandlerHooks},
    registry::{handler, HandlerResult, TxRegistry},
    Evm, EvmFeatures, SpecId, TxResult,
};

use super::GnosisEvmTypes;

/// Builds the transaction registry for `spec_id`.
///
/// Registers the same transaction types, at the same forks, as
/// [`evm2::ethereum::ethereum_tx_registry`], with the Gnosis fee hooks installed.
pub(crate) fn gnosis_tx_registry(
    spec_id: SpecId,
) -> TxRegistry<GnosisEvmTypes, TxResult<GnosisEvmTypes>> {
    type T = GnosisEvmTypes;
    type H = GnosisTxHooks;

    let mut registry = TxRegistry::new().with_handler(
        0,
        TxEnvelope::as_legacy,
        handler(
            legacy::prepare_with_hooks::<T, H>,
            legacy::execute_prepared::<T, H>,
        ),
    );
    if spec_id.enables(SpecId::BERLIN) {
        registry.register(
            1,
            TxEnvelope::as_eip2930,
            handler(
                eip2930::prepare_with_hooks::<T, H>,
                eip2930::execute_prepared::<T, H>,
            ),
        );
    }
    if spec_id.enables(SpecId::LONDON) {
        registry.register(
            2,
            TxEnvelope::as_eip1559,
            handler(
                eip1559::prepare_with_hooks::<T, H>,
                eip1559::execute_prepared::<T, H>,
            ),
        );
    }
    if spec_id.enables(SpecId::CANCUN) {
        registry.register(
            3,
            TxEnvelope::as_eip4844,
            handler(
                eip4844::prepare_with_hooks::<T, H>,
                eip4844::execute_prepared::<T, H>,
            ),
        );
    }
    if spec_id.enables(SpecId::PRAGUE) {
        registry.register(
            4,
            TxEnvelope::as_eip7702,
            handler(
                eip7702::prepare_with_hooks::<T, H>,
                eip7702::execute_prepared::<T, H>,
            ),
        );
    }
    registry
}

/// Transaction hooks that mint Gnosis fees to the fee collector.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GnosisTxHooks;

impl TxHandlerHooks<GnosisEvmTypes> for GnosisTxHooks {
    fn before_execution(
        host: &mut Evm<'_, GnosisEvmTypes>,
        envelope: &TxEnvelope,
        caller: Address,
        upfront_fee: U256,
    ) -> HandlerResult<()> {
        charge_upfront(host, caller, upfront_fee)?;

        if host.spec_id().enables(SpecId::PRAGUE) {
            let blob_fee = U256::from(blob_gas(envelope)).saturating_mul(host.block().blob_basefee);
            let charge_fees = host.feature(EvmFeatures::FEE_CHARGE);
            let fee_collector = host.ext().fee_collector;
            let mut collector = host.state_mut().account(&fee_collector)?;
            // The pre-evm2 implementation loaded the collector through the revm journal for every
            // Prague transaction, which also warmed it (EIP-2929). Keep that so transactions that
            // access the collector are charged the same gas as before.
            collector.warm();
            if charge_fees {
                // A zero fee still touches the collector, as the revm implementation did.
                collector.add_balance(blob_fee);
            }
        }
        Ok(())
    }

    fn settle_transaction(
        host: &mut Evm<'_, GnosisEvmTypes>,
        envelope: &TxEnvelope,
        gas: GasSettlement<GnosisEvmTypes>,
    ) -> HandlerResult<TxResult<GnosisEvmTypes>> {
        let result = default_settle_gas(host, gas)?;

        if host.spec_id().enables(SpecId::LONDON)
            && host.feature(EvmFeatures::FEE_CHARGE)
            && !is_free(envelope)
        {
            let base_fee = host
                .block()
                .basefee
                .saturating_mul(U256::from(result.tx_gas_used()));
            let fee_collector = host.ext().fee_collector;
            host.state_mut()
                .account(&fee_collector)?
                .add_balance(base_fee);
        }
        Ok(result)
    }
}

/// Returns whether `tx` is a Gnosis "service transaction" that pays no fees.
///
/// Service transactions do not pay the base fee, so nothing is minted to the collector for them.
/// Mirrors Nethermind's `eip1559Fees = !tx.IsFree() ? BaseFeePerGas * spentGas : 0`, where
/// `IsFree()` means a zero gas price and a zero priority fee.
fn is_free(tx: &TxEnvelope) -> bool {
    tx.max_fee_per_gas() == 0 && tx.max_priority_fee_per_gas().unwrap_or_default() == 0
}

/// Returns the EIP-4844 blob gas consumed by `tx`, zero for non-blob transactions.
fn blob_gas(tx: &TxEnvelope) -> u64 {
    tx.blob_versioned_hashes()
        .map_or(0, |hashes| hashes.len() as u64 * DATA_GAS_PER_BLOB)
}
