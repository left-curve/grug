use {
    crate::{to_sized, CryptoResult, Identity256, SignatureResultExt},
    grug_types::CryptoError,
    p256::ecdsa::{signature::DigestVerifier, Signature, VerifyingKey},
};

const SECP256R1_DIGEST_LEN: usize = 32;
const SECP256R1_PUBKEY_LENS: [usize; 2] = [33, 65]; // compressed, uncompressed
const SECP256R1_SIGNATURE_LEN: usize = 64;

/// NOTE: This function takes the hash of the message, not the prehash.
pub fn secp256r1_verify(msg_hash: &[u8], sig: &[u8], pk: &[u8]) -> CryptoResult<()> {
    let msg_hash = to_sized::<SECP256R1_DIGEST_LEN>(msg_hash, CryptoError::InvalidMsg)?;
    let msg_hash = Identity256::from(msg_hash);

    let sig = to_sized::<SECP256R1_SIGNATURE_LEN>(sig, CryptoError::InvalidSig)?;
    let mut sig = Signature::from_bytes(&sig.into()).crypto_invalid_sig_format()?;

    // High-S signatures require normalization since our verification implementation
    // rejects them by default. If we had a verifier that does not restrict to
    // low-S only, this step was not needed.
    if let Some(normalized) = sig.normalize_s() {
        sig = normalized;
    }

    if !SECP256R1_PUBKEY_LENS.contains(&pk.len()) {
        return Err(CryptoError::InvalidPk);
    }

    VerifyingKey::from_sec1_bytes(pk)
        .crypto_invalid_pk_format()?
        .verify_digest(msg_hash, &sig)
        .crypto_verify_failed()
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::sha2_256,
        p256::ecdsa::{signature::DigestSigner, Signature, SigningKey},
        rand::rngs::OsRng,
    };

    #[test]
    fn verifying_secp256r1() {
        // Generate a valid signature
        let sk = SigningKey::random(&mut OsRng);
        let vk = VerifyingKey::from(&sk);
        let msg = b"Jake";
        let msg_hash = Identity256::from(sha2_256(msg));
        let sig: Signature = sk.sign_digest(msg_hash.clone());

        // Valid signature
        {
            assert!(secp256r1_verify(&msg_hash, &sig.to_bytes(), &vk.to_sec1_bytes()).is_ok());
        }

        // Incorrect private key
        {
            let false_sk = SigningKey::random(&mut OsRng);
            let false_sig: Signature = false_sk.sign_digest(msg_hash.clone());
            assert!(
                secp256r1_verify(&msg_hash, &false_sig.to_bytes(), &vk.to_sec1_bytes()).is_err()
            );

            let a = secp256r1_verify(&msg_hash, &false_sig.to_bytes(), &vk.to_sec1_bytes())
                .unwrap_err();
            println!("{}", a);
        }

        // Incorrect public key
        {
            let false_sk = SigningKey::random(&mut OsRng);
            let false_vk = VerifyingKey::from(&false_sk);
            assert!(
                secp256r1_verify(&msg_hash, &sig.to_bytes(), &false_vk.to_sec1_bytes()).is_err()
            );
        }

        // Incorrect message
        {
            let false_msg = b"Larry";
            let false_msg_hash = sha2_256(false_msg);
            assert!(
                secp256r1_verify(&false_msg_hash, &sig.to_bytes(), &vk.to_sec1_bytes()).is_err()
            );
        }
    }
}
