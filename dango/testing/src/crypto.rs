use {
    dango_primitives::ByteArray,
    k256::{
        ecdsa::{Signature, SigningKey, signature::hazmat::PrehashSigner},
        elliptic_curve::Generate,
    },
};

pub fn generate_random_key() -> (SigningKey, ByteArray<33>) {
    let sk = SigningKey::generate();
    let pk = sk
        .verifying_key()
        .to_sec1_point(true)
        .to_bytes()
        .as_ref()
        .try_into()
        .unwrap();

    (sk, pk)
}

/// Note: This function expects the _hashed_ sign data.
pub fn create_signature(sk: &SigningKey, sign_data: [u8; 32]) -> ByteArray<64> {
    let signature: Signature = sk.sign_prehash(&sign_data).unwrap();

    ByteArray::from_inner(signature.to_bytes().into())
}
