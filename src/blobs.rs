use alloy_eips::eip7840::BlobParams;
use reth_chainspec::HardforkBlobParams;
use revm::context_interface::block::BlobExcessGasAndPrice;
use revm_primitives::hardfork::SpecId;

pub static CANCUN_BLOB_PARAMS: BlobParams = BlobParams {
    target_blob_count: 1,
    max_blob_count: 2,
    update_fraction: 1112826,
    min_blob_fee: 1000000000,
};

pub static PRAGUE_BLOB_PARAMS: BlobParams = BlobParams {
    target_blob_count: 1,
    max_blob_count: 2,
    update_fraction: 1112826,
    min_blob_fee: 1000000000,
};

pub const GNOSIS_BLOB_SCHEDULE: HardforkBlobParams = HardforkBlobParams {
    cancun: CANCUN_BLOB_PARAMS,
    prague: PRAGUE_BLOB_PARAMS,
};

// helper function to create the evm's CfgEnv in get_cfg_env
pub fn evm_env_blob_schedule() -> Vec<(SpecId, u8, u8)> {
    vec![(SpecId::CANCUN, 1, 2), (SpecId::PRAGUE, 1, 2)]
}

pub fn get_blob_params(is_prague: bool) -> BlobParams {
    if is_prague {
        PRAGUE_BLOB_PARAMS
    } else {
        CANCUN_BLOB_PARAMS
    }
}

#[inline]
pub fn calc_blob_gasprice(excess_blob_gas: u64, is_prague: bool) -> u128 {
    fake_exponential(
        if is_prague {
            PRAGUE_BLOB_PARAMS.min_blob_fee
        } else {
            CANCUN_BLOB_PARAMS.min_blob_fee
        },
        excess_blob_gas as u128,
        if is_prague {
            PRAGUE_BLOB_PARAMS.update_fraction
        } else {
            CANCUN_BLOB_PARAMS.update_fraction
        },
    )
}

pub fn fake_exponential(factor: u128, numerator: u128, denominator: u128) -> u128 {
    assert_ne!(denominator, 0, "attempt to divide by zero");
    let mut i = 1;
    let mut output = 0;
    let mut numerator_accum = factor * denominator;
    while numerator_accum > 0 {
        output += numerator_accum;

        // Denominator is asserted as not zero at the start of the function.
        numerator_accum = (numerator_accum * numerator) / (denominator * i);
        i += 1;
    }
    output / denominator
}

pub fn next_blob_gas_and_price(excess_blob_gas: u64, is_prague: bool) -> BlobExcessGasAndPrice {
    let blob_gasprice = calc_blob_gasprice(excess_blob_gas, is_prague);
    BlobExcessGasAndPrice {
        excess_blob_gas,
        blob_gasprice,
    }
}
