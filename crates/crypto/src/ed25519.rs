use {
    crate::{to_sized, CryptoResult, Identity512, SignatureResultExt},
    ed25519_dalek::{DigestVerifier, Signature, VerifyingKey},
    grug_types::CryptoError,
};

const ED25519_DIGEST_LEN: usize = 64;
const ED25519_PUBKEY_LEN: usize = 32;
const ED25519_SIGNATURE_LEN: usize = 64;

/// Verify an ED25519 signature with the given hashed message and public
/// key.
///
/// NOTE: This function takes the hash of the message, not the prehash.
pub fn ed25519_verify(msg_hash: &[u8], sig: &[u8], pk: &[u8]) -> CryptoResult<()> {
    let msg_hash = to_sized::<ED25519_DIGEST_LEN>(msg_hash, CryptoError::InvalidMsg)?;
    let msg_hash = Identity512::from(msg_hash);

    let sig = to_sized::<ED25519_SIGNATURE_LEN>(sig, CryptoError::InvalidSig)?;
    let sig = Signature::from(sig);

    let vk = to_sized::<ED25519_PUBKEY_LEN>(pk, CryptoError::InvalidPk)?;
    let vk = VerifyingKey::from_bytes(&vk).crypto_invalid_pk_format()?;

    vk.verify_digest(msg_hash, &sig).crypto_verify_failed()
}

/// Verify a batch of Ed25519 signatures with the given _prehash_ messages and
/// and public keys.
///
/// NOTE: Unlike all other functions in this crate, this one takes the prehash
/// message, not it's hash.
pub fn ed25519_batch_verify(
    prehash_msgs: &[&[u8]],
    sigs: &[&[u8]],
    pks: &[&[u8]],
) -> CryptoResult<()> {
    let (sigs, vks): (Vec<_>, Vec<_>) = sigs
        .iter()
        .zip(pks.iter())
        .map(|(sig, pk)| {
            let sig = to_sized::<ED25519_SIGNATURE_LEN>(sig, CryptoError::InvalidSig)?;
            let sig = Signature::from(sig);

            let vk = to_sized::<ED25519_PUBKEY_LEN>(pk, CryptoError::InvalidPk)?;
            let vk = VerifyingKey::from_bytes(&vk).crypto_verify_failed()?;

            Ok((sig, vk))
        })
        .collect::<CryptoResult<Vec<_>>>()?
        .into_iter()
        .unzip();

    // No need to check the three slices (`msg_hashes`, `sigs`, `pks`) are of the
    // length; `ed25519_dalek::verify_batch` already does this.
    ed25519_dalek::verify_batch(prehash_msgs, &sigs, &vks).crypto_verify_failed()
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::sha2_512,
        ed25519_dalek::{DigestSigner, Signer, SigningKey},
        rand::rngs::OsRng,
    };

    #[test]
    fn verify_ed25519() {
        let sk = SigningKey::generate(&mut OsRng);
        let vk = VerifyingKey::from(&sk);
        let msg = b"Jake";
        let msg_hash = Identity512::from(sha2_512(msg));
        let sig = sk.sign_digest(msg_hash.clone());

        // Valid signature
        {
            assert!(ed25519_verify(&msg_hash, &sig.to_bytes(), vk.as_bytes()).is_ok());
        }

        // Incorrect private key
        {
            let false_sk = SigningKey::generate(&mut OsRng);
            let false_sig: Signature = false_sk.sign_digest(msg_hash.clone());
            assert!(ed25519_verify(&msg_hash, &false_sig.to_bytes(), vk.as_bytes()).is_err());
        }

        // Incorrect message
        {
            let false_msg = b"Larry";
            let false_msg_hash = sha2_512(false_msg);
            assert!(ed25519_verify(&false_msg_hash, &sig.to_bytes(), vk.as_bytes()).is_err());
        }
    }

    #[test]
    fn verify_batch_ed25519() {
        let (prehash_msg1, sig1, vk1) = ed25519_sign("Jake");
        let (prehash_msg2, sig2, vk2) = ed25519_sign("Larry");
        let (prehash_msg3, sig3, vk3) = ed25519_sign("Rhaki");

        // Valid signatures
        {
            assert!(ed25519_batch_verify(
                &[&prehash_msg1, &prehash_msg2, &prehash_msg3],
                &[&sig1, &sig2, &sig3],
                &[&vk1, &vk2, &vk3]
            )
            .is_ok());
        }

        // Wrong sign
        {
            assert!(ed25519_batch_verify(
                &[&prehash_msg1, &prehash_msg2, &prehash_msg3],
                &[&sig2, &sig1, &sig3],
                &[&vk1, &vk2, &vk3]
            )
            .is_err());
        }

        // Wrong len
        {
            assert!(ed25519_batch_verify(
                &[&prehash_msg1, &prehash_msg2, &prehash_msg3],
                &[&sig1, &sig2, &sig3],
                &[&vk1, &vk2]
            )
            .is_err());
        }
    }

    fn ed25519_sign(msg: &str) -> (Vec<u8>, [u8; 64], [u8; 32]) {
        let sk = SigningKey::generate(&mut OsRng);
        let vk = VerifyingKey::from(&sk);
        let sig = sk.sign(msg.as_bytes());
        (msg.as_bytes().to_vec(), sig.to_bytes(), vk.to_bytes())
    }
}
