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
/// Field order: parent_hash, ommers_hash, beneficiary, state_root, transactions_root,
/// receipts_root, logs_bloom, difficulty, number, gas_limit, gas_used, timestamp,
/// extra_data, [base_fee_per_gas?, withdrawals_root?, blob_gas_used?, excess_blob_gas?,
/// parent_beacon_block_root?, requests_hash?]
pub fn compute_seal_hash(header: &GnosisHeader) -> B256 {
    let mut buf = Vec::new();

    // Compute payload length (all fields except aura_step and aura_seal)
    let payload_length = seal_hash_payload_length(header);
    let list_header = alloy_rlp::Header {
        list: true,
        payload_length,
    };
    list_header.encode(&mut buf);

    // Standard 13 fields
    header.parent_hash.encode(&mut buf);
    header.ommers_hash.encode(&mut buf);
    header.beneficiary.encode(&mut buf);
    header.state_root.encode(&mut buf);
    header.transactions_root.encode(&mut buf);
    header.receipts_root.encode(&mut buf);
    header.logs_bloom.encode(&mut buf);
    header.difficulty.encode(&mut buf);
    U256::from(header.number).encode(&mut buf);
    U256::from(header.gas_limit).encode(&mut buf);
    U256::from(header.gas_used).encode(&mut buf);
    header.timestamp.encode(&mut buf);
    header.extra_data.encode(&mut buf);

    // Skip aura_step and aura_seal — these are the seal fields

    // Optional EIP fields
    if let Some(ref base_fee) = header.base_fee_per_gas {
        U256::from(*base_fee).encode(&mut buf);
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
    length += U256::from(header.number).length();
    length += U256::from(header.gas_limit).length();
    length += U256::from(header.gas_used).length();
    length += header.timestamp.length();
    length += header.extra_data.length();
    // No aura_step, no aura_seal
    if let Some(base_fee) = header.base_fee_per_gas {
        length += U256::from(base_fee).length();
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

    // The 65-byte seal: first 32 bytes = r, next 32 bytes = s, last byte = v
    let sig_bytes: &[u8; 65] = seal.as_ref();
    let r = &sig_bytes[0..32];
    let s = &sig_bytes[32..64];
    let v = sig_bytes[64];

    // Create alloy signature (v is the recovery id, 0 or 1)
    // Some clients use v = 27/28, some use v = 0/1
    let recovery_id = if v >= 27 { v - 27 } else { v };

    let mut sig_with_v = [0u8; 65];
    sig_with_v[..32].copy_from_slice(r);
    sig_with_v[32..64].copy_from_slice(s);
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
    #[error("missing AuRa step")]
    MissingStep,
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

    fn u128_max() -> U256 {
        U256::MAX >> 128
    }

    #[test]
    fn calculate_aura_difficulty_step_diff_one() {
        // Standard sequential block: current = parent + 1.
        // diff = (2^128 - 1) + parent - current = U128_MAX - 1
        let d = calculate_aura_difficulty(100, 101);
        assert_eq!(d, u128_max() - U256::from(1));
    }

    #[test]
    fn calculate_aura_difficulty_step_diff_large() {
        // Skipped steps: current = parent + 10.
        let d = calculate_aura_difficulty(1000, 1010);
        assert_eq!(d, u128_max() - U256::from(10));
    }

    #[test]
    fn calculate_aura_difficulty_equal_steps() {
        // current == parent: diff = U128_MAX exactly.
        let d = calculate_aura_difficulty(42, 42);
        assert_eq!(d, u128_max());
    }

    #[test]
    fn calculate_aura_difficulty_chiado_block_100k() {
        // Real chiado block 100000: aura_step = 332890827.
        // Block 99999's step would be 332890826 (sequential).
        // header.difficulty = 0xfffffffffffffffffffffffffffffffe = U128_MAX - 1.
        let d = calculate_aura_difficulty(332890826, 332890827);
        let expected = U256::from_be_slice(
            &hex::decode("00000000000000000000000000000000fffffffffffffffffffffffffffffffe")
                .unwrap(),
        );
        assert_eq!(d, expected);
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
    fn recover_seal_author_missing_seal() {
        let mut header = chiado_block_100k_header();
        header.aura_seal = None;
        match recover_seal_author(&header) {
            Err(SealError::MissingSeal) => {}
            other => panic!("expected MissingSeal, got {:?}", other),
        }
    }

    #[test]
    fn recover_seal_author_corrupted_seal_recovers_different_address() {
        // Mutating any byte of the seal flips the recovered address (or fails recovery).
        // Either outcome must not silently match the original signer.
        let mut header = chiado_block_100k_header();
        let mut seal = *header.aura_seal.unwrap();
        seal[0] ^= 0x01;
        header.aura_seal = Some(FixedBytes::from(seal));
        let original_beneficiary = header.beneficiary;
        match recover_seal_author(&header) {
            Ok(addr) => assert_ne!(addr, original_beneficiary),
            Err(_) => {} // recovery failure is also acceptable
        }
    }
}
