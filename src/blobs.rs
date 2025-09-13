use alloy_eips::eip7840::BlobParams;
use alloy_eips::BlobScheduleBlobParams;

pub static CANCUN_BLOB_PARAMS: BlobParams = BlobParams {
    target_blob_count: 1,
    max_blob_count: 2,
    update_fraction: 1112826,
    min_blob_fee: 1000000000,
    max_blobs_per_tx: 2,
    blob_base_cost: 0,
};

pub static PRAGUE_BLOB_PARAMS: BlobParams = BlobParams {
    target_blob_count: 1,
    max_blob_count: 2,
    update_fraction: 1112826,
    min_blob_fee: 1000000000,
    max_blobs_per_tx: 2,
    blob_base_cost: 0,
};

pub fn gnosis_blob_schedule() -> BlobScheduleBlobParams {
    BlobScheduleBlobParams {
        cancun: CANCUN_BLOB_PARAMS,
        prague: PRAGUE_BLOB_PARAMS,
        osaka: PRAGUE_BLOB_PARAMS,
        scheduled: vec![],
    }
}
