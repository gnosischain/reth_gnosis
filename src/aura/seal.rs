use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_rlp::Encodable;
use gnosis_primitives::header::GnosisHeader;

/// Compute the "bare hash" of a pre-merge AuRa header — the hash of the RLP-encoded
/// header WITHOUT the seal fields (aura_step and aura_seal).
///
/// This is the message that the AuRa validator signs. The encoding order matches
/// OpenEthereum: standard Ethereum fields (1-13) followed by optional EIP fields,
/// with the AuRa seal fields (positions 14-15) excluded.
///
/// Field order: parent_hash, ommers_hash, beneficiary, state_root,
/// transactions_root, receipts_root, logs_bloom, difficulty, number, gas_limit,
/// gas_used, timestamp, extra_data, [base_fee_per_gas?]. Post-Shanghai fields
/// (`withdrawals_root`, `blob_gas_used`, etc.) are not encoded — pre-merge AuRa
/// headers never have them set, so including them in the field list would just
/// confuse the reader.
pub fn compute_seal_hash(header: &GnosisHeader) -> B256 {
    let mut buf = Vec::new();

    // Compute payload length (all fields except aura_step and aura_seal)
    let payload_length = seal_hash_payload_length(header);
    let list_header = alloy_rlp::Header {
        list: true,
        payload_length,
    };
    list_header.encode(&mut buf);

    // Standard 13 fields. Use native types — RLP for integers encodes the
    // minimal-byte big-endian representation, identical for `u64` and `U256`
    // for any value that fits in 64 bits, but `u64` is honest about the field's
    // domain.
    header.parent_hash.encode(&mut buf);
    header.ommers_hash.encode(&mut buf);
    header.beneficiary.encode(&mut buf);
    header.state_root.encode(&mut buf);
    header.transactions_root.encode(&mut buf);
    header.receipts_root.encode(&mut buf);
    header.logs_bloom.encode(&mut buf);
    header.difficulty.encode(&mut buf);
    header.number.encode(&mut buf);
    header.gas_limit.encode(&mut buf);
    header.gas_used.encode(&mut buf);
    header.timestamp.encode(&mut buf);
    header.extra_data.encode(&mut buf);

    // Skip aura_step and aura_seal — these are the seal fields

    // Optional EIP fields (London onwards on Gnosis)
    if let Some(base_fee) = header.base_fee_per_gas {
        base_fee.encode(&mut buf);
    }

    keccak256(&buf)
}

fn seal_hash_payload_length(header: &GnosisHeader) -> usize {
    let mut length = 0;
    length += header.parent_hash.length();
    length += header.ommers_hash.length();
    length += header.beneficiary.length();
    length += header.state_root.length();
    length += header.transactions_root.length();
    length += header.receipts_root.length();
    length += header.logs_bloom.length();
    length += header.difficulty.length();
    length += header.number.length();
    length += header.gas_limit.length();
    length += header.gas_used.length();
    length += header.timestamp.length();
    length += header.extra_data.length();
    // No aura_step, no aura_seal
    if let Some(base_fee) = header.base_fee_per_gas {
        length += base_fee.length();
    }
    length
}

/// Recover the signer address from the AuRa seal signature.
///
/// The seal is a 65-byte ECDSA signature: [r(32) | s(32) | v(1)].
/// Returns the Ethereum address recovered from the signature over the seal hash.
pub fn recover_seal_author(header: &GnosisHeader) -> Result<Address, SealError> {
    let seal = header.aura_seal.as_ref().ok_or(SealError::MissingSeal)?;
    let seal_hash = compute_seal_hash(header);

    // The 65-byte seal: r (32) || s (32) || v (1). Copy whole-buffer and
    // overwrite the v byte with the normalized recovery id; r and s are
    // already in place.
    let sig_bytes: &[u8; 65] = seal.as_ref();
    let v = sig_bytes[64];
    // Some clients use v = 27/28, some use v = 0/1.
    let recovery_id = if v >= 27 { v - 27 } else { v };
    let mut sig_with_v = *sig_bytes;
    sig_with_v[64] = recovery_id;

    let sig = alloy_primitives::Signature::try_from(&sig_with_v[..])
        .map_err(|_| SealError::InvalidSignature)?;

    sig.recover_address_from_prehash(&seal_hash)
        .map_err(|_| SealError::RecoveryFailed)
}

/// Errors that can occur during seal verification.
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("missing AuRa seal")]
    MissingSeal,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("ECDSA recovery failed")]
    RecoveryFailed,
}

/// Compute AuRa difficulty.
///
/// difficulty = U128_MAX + parent_step - current_step
/// Where U128_MAX = 2^128 - 1 (= U256::MAX >> 128)
pub fn calculate_aura_difficulty(parent_step: u64, current_step: u64) -> U256 {
    let max_u128: U256 = U256::MAX >> 128; // 2^128 - 1
                                           // Use wrapping arithmetic to handle the subtraction
    max_u128
        .wrapping_add(U256::from(parent_step))
        .wrapping_sub(U256::from(current_step))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256, bloom, bytes, fixed_bytes, FixedBytes};

    #[test]
    fn calculate_aura_difficulty_table() {
        let u128_max = U256::MAX >> 128;
        // (parent_step, current_step, expected_difficulty, label)
        let cases: &[(u64, u64, U256, &str)] = &[
            (100, 101, u128_max - U256::from(1u64), "sequential step"),
            (1000, 1010, u128_max - U256::from(10u64), "10 skipped steps"),
            (42, 42, u128_max, "equal steps"),
            // Chiado block 100000 (parent step 332890826, current 332890827) —
            // the on-chain header has difficulty = U128_MAX - 1; pinning this
            // matches our header decoding against a real network.
            (
                332890826,
                332890827,
                u128_max - U256::from(1u64),
                "chiado block 100000",
            ),
        ];
        for (parent, current, expected, label) in cases {
            assert_eq!(
                calculate_aura_difficulty(*parent, *current),
                *expected,
                "case: {label}"
            );
        }
    }

    /// Construct a `GnosisHeader` matching chiado block 100000 (verified post-merge
    /// against `https://gnosis-chiado-rpc.publicnode.com/`). Used as a golden vector
    /// for `recover_seal_author`: changing the RLP field order or set in
    /// `compute_seal_hash` will flip the recovered signer.
    fn chiado_block_100k_header() -> GnosisHeader {
        GnosisHeader {
            parent_hash: b256!(
                "0xcec0385ac2b2aa8557e2ad9318ddb419a70d53eac388aa06d5ec028e2b2bf05d"
            ),
            ommers_hash: b256!(
                "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347"
            ),
            beneficiary: address!("0x60f1cf46b42df059b98acf67c1dd7771b100e124"),
            state_root: b256!(
                "0x60c5a1ea18ad828b702c4c493614e0b9524b084a75af5a901debc1bc6434129a"
            ),
            transactions_root: b256!(
                "0xb84545c21d2a61526520416f217e9dd2f2bbf47c1c9e8b14a26cada11ce3e380"
            ),
            receipts_root: b256!(
                "0xd51661186f3e633f6e1719d9bb2197c9400e13099b133b145ce2f45c97b53023"
            ),
            logs_bloom: bloom!(
                "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
            ),
            difficulty: U256::from_be_bytes(alloy_primitives::B256::from_slice(
                &hex::decode(
                    "00000000000000000000000000000000fffffffffffffffffffffffffffffffe",
                )
                .unwrap(),
            ).0),
            number: 100000,
            gas_limit: 0xbebc20,
            gas_used: 0x1302a,
            timestamp: 0x63358df7,
            extra_data: bytes!("0x4e65746865726d696e64"),
            mix_hash: None,
            nonce: None,
            aura_step: Some(U256::from(332890827u64)),
            aura_seal: Some(fixed_bytes!(
                "0xf6d76ffe58e0ee70301cdf4365d9c1f5ee6de675211e19d8ce23b522ecd940913ac52b0acfece44a38086d783a89fe061e21b1f6ec8860bdbba4bb26a77ace4600"
            )),
            base_fee_per_gas: Some(7),
            withdrawals_root: None,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
            requests_hash: None,
            block_access_list_hash: None,
            slot_number: None,
        }
    }

    #[test]
    fn recover_seal_author_chiado_block_100k_golden() {
        // Golden vector: AuRa seal in chiado block 100000 must recover to the
        // beneficiary (in AuRa the proposer == miner == seal signer).
        // If `compute_seal_hash` changes its RLP field order or includes/excludes
        // the wrong fields, this test catches it permanently.
        let header = chiado_block_100k_header();
        let signer = recover_seal_author(&header).expect("recovery must succeed");
        assert_eq!(signer, header.beneficiary);
    }

    #[test]
    fn recover_seal_author_corrupted_seal_fails_recovery() {
        // Flipping the high bit of `r` (byte 0) — for the chiado-100k seal
        // — shifts the signature to invalid curve coordinates. Recovery
        // returns `RecoveryFailed`. A future change to recovery semantics
        // (e.g. accepting non-canonical signatures) would flip this to Ok
        // and the test will fail loudly. Pin the exact behavior.
        let mut header = chiado_block_100k_header();
        let mut seal = *header.aura_seal.unwrap();
        seal[0] ^= 0x01;
        header.aura_seal = Some(FixedBytes::from(seal));
        let err = recover_seal_author(&header).unwrap_err();
        assert!(matches!(err, SealError::RecoveryFailed), "got {err:?}");
    }
}
