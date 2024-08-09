use {
    crate::{to_sized, CryptoResult, Identity256, SignatureResultExt},
    grug_types::CryptoError,
    k256::ecdsa::{signature::DigestVerifier, RecoveryId, Signature, VerifyingKey},
};

const SECP256K1_DIGEST_LEN: usize = 32;
const SECP256K1_PUBKEY_LENS: [usize; 2] = [33, 65]; // compressed, uncompressed
const SECP256K1_SIGNATURE_LEN: usize = 64;

/// NOTE: This function takes the hash of the message, not the prehash.
pub fn secp256k1_verify(msg_hash: &[u8], sig: &[u8], pk: &[u8]) -> CryptoResult<()> {
    let msg_hash = to_sized::<SECP256K1_DIGEST_LEN>(msg_hash, CryptoError::InvalidMsg)?;
    let msg_hash = Identity256::from(msg_hash);

    let sig = to_sized::<SECP256K1_SIGNATURE_LEN>(sig, CryptoError::InvalidSig)?;
    let mut sig = Signature::from_bytes(&sig.into()).crypto_invalid_sig_format()?;

    // High-S signatures require normalization since our verification implementation
    // rejects them by default. If we had a verifier that does not restrict to
    // low-S only, this step was not needed.
    if let Some(normalized) = sig.normalize_s() {
        sig = normalized;
    }

    if !SECP256K1_PUBKEY_LENS.contains(&pk.len()) {
        return Err(CryptoError::InvalidPk);
    }

    VerifyingKey::from_sec1_bytes(pk)
        .crypto_invalid_pk_format()?
        .verify_digest(msg_hash, &sig)
        .crypto_verify_failed()
}

/// Recover the Secp256k1 public key as SEC1 bytes from the _hashed_ message and
/// signature.
///
/// - `r`: the first 32 bytes of the signature;
/// - `s`: the last 32 bytes of the signature;
/// - `v`: the recovery ID.
///
/// `v` must be 0 or 1. The values 2 and 3 are unsupported by this implementation,
/// which is the same restriction [as Ethereum has](https://github.com/ethereum/go-ethereum/blob/v1.9.25/internal/ethapi/api.go#L466-L469).
/// All other values are invalid.
///
/// Note: This function takes the hash of the message, not the prehash.
pub fn secp256k1_pubkey_recover(
    msg_hash: &[u8],
    sig: &[u8],
    recovery_id: u8,
    compressed: bool,
) -> CryptoResult<Vec<u8>> {
    let msg_hash = to_sized::<SECP256K1_DIGEST_LEN>(msg_hash, CryptoError::InvalidMsg)?;
    let msg_hash = Identity256::from(msg_hash);

    let sig = to_sized::<SECP256K1_SIGNATURE_LEN>(sig, CryptoError::InvalidSig)?;
    let mut sig = Signature::from_bytes(&sig.into()).crypto_invalid_sig_format()?;

    let mut id = match recovery_id {
        0 => RecoveryId::new(false, false),
        1 => RecoveryId::new(true, false),
        _ => return Err(CryptoError::InvalidRecoveryId),
    };

    if let Some(normalized) = sig.normalize_s() {
        sig = normalized;
        id = RecoveryId::new(!id.is_y_odd(), id.is_x_reduced());
    }

    // Convert the public key to SEC1 bytes
    VerifyingKey::recover_from_digest(msg_hash, &sig, id)
        .map(|vk| vk.to_encoded_point(compressed).to_bytes().into())
        .crypto_recovery_failed()
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::sha2_256,
        k256::ecdsa::{signature::DigestSigner, Signature, SigningKey},
        rand::rngs::OsRng,
    };

    #[test]
    fn verifying_secp256k1() {
        // Generate a valid signature
        let sk = SigningKey::random(&mut OsRng);
        let vk = VerifyingKey::from(&sk);
        let msg = b"Jake";
        let msg_hash = Identity256::from(sha2_256(msg));
        let sig: Signature = sk.sign_digest(msg_hash.clone());

        // Valid signature
        {
            assert!(secp256k1_verify(&msg_hash, &sig.to_bytes(), &vk.to_sec1_bytes()).is_ok());
        }

        // Incorrect private key
        {
            let false_sk = SigningKey::random(&mut OsRng);
            let false_sig: Signature = false_sk.sign_digest(msg_hash.clone());
            assert!(
                secp256k1_verify(&msg_hash, &false_sig.to_bytes(), &vk.to_sec1_bytes()).is_err()
            );
        }

        // Incorrect public key
        {
            let false_sk = SigningKey::random(&mut OsRng);
            let false_vk = VerifyingKey::from(&false_sk);
            assert!(
                secp256k1_verify(&msg_hash, &sig.to_bytes(), &false_vk.to_sec1_bytes()).is_err()
            );
        }

        // Incorrect message
        {
            let false_msg = b"Larry";
            let false_msg_hash = sha2_256(false_msg);
            assert!(
                secp256k1_verify(&false_msg_hash, &sig.to_bytes(), &vk.to_sec1_bytes()).is_err()
            );
        }
    }

    #[test]
    fn recovering_secp256k1() {
        // Generate a valid signature
        let sk = SigningKey::random(&mut OsRng);
        let vk = VerifyingKey::from(&sk);
        let msg = b"Jake";
        let msg_hash = Identity256::from(sha2_256(msg));
        let (sig, recovery_id) = sk.sign_digest_recoverable(msg_hash.clone()).unwrap();

        // Recover compressed pk
        {
            let recovered_pk =
                secp256k1_pubkey_recover(&msg_hash, &sig.to_vec(), recovery_id.to_byte(), true)
                    .unwrap();
            assert_eq!(recovered_pk, vk.to_encoded_point(true).as_bytes());
        }

        // Recover uncompressed pk
        {
            let recovered_pk =
                secp256k1_pubkey_recover(&msg_hash, &sig.to_vec(), recovery_id.to_byte(), false)
                    .unwrap();
            assert_eq!(recovered_pk, vk.to_encoded_point(false).as_bytes());
        }
    }
}
