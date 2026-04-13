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
    if let Some(ref root) = header.withdrawals_root {
        root.encode(&mut buf);
    }
    if let Some(ref blob_gas_used) = header.blob_gas_used {
        U256::from(*blob_gas_used).encode(&mut buf);
    }
    if let Some(ref excess_blob_gas) = header.excess_blob_gas {
        U256::from(*excess_blob_gas).encode(&mut buf);
    }
    if let Some(ref parent_beacon_block_root) = header.parent_beacon_block_root {
        parent_beacon_block_root.encode(&mut buf);
    }
    if let Some(ref requests_hash) = header.requests_hash {
        requests_hash.encode(&mut buf);
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
    if let Some(root) = header.withdrawals_root {
        length += root.length();
    }
    if let Some(blob_gas_used) = header.blob_gas_used {
        length += U256::from(blob_gas_used).length();
    }
    if let Some(excess_blob_gas) = header.excess_blob_gas {
        length += U256::from(excess_blob_gas).length();
    }
    if let Some(parent_beacon_block_root) = header.parent_beacon_block_root {
        length += parent_beacon_block_root.length();
    }
    if let Some(requests_hash) = header.requests_hash {
        length += requests_hash.length();
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
