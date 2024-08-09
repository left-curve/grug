use grug::{Empty, ImmutableCtx, Number, StdResult, Uint128};

use crate::CryptoVerifyType;

pub fn query_loop(iterations: u64) -> StdResult<Empty> {
    // Keep the same operation per iteration for consistency
    for _ in 0..iterations {
        let number = Uint128::new(100);
        number.checked_add(number)?;
        number.checked_sub(number)?;
        number.checked_mul(number)?;
        number.checked_div(number)?;
        number.checked_pow(2)?;
    }

    Ok(Empty {})
}

pub fn query_force_write(_key: &str, _value: &str) -> Empty {
    #[cfg(target_arch = "wasm32")]
    {
        use grug::Region;

        extern "C" {
            fn db_write(key_ptr: usize, value_ptr: usize);
        }

        let key_region = Region::build(_key.as_bytes());
        let key_ptr = &*key_region as *const Region;

        let value_region = Region::build(_value.as_bytes());
        let value_ptr = &*value_region as *const Region;

        // This should fail!
        unsafe {
            db_write(key_ptr as usize, value_ptr as usize);
        }
    }

    Empty {}
}

pub fn query_crypto_verify(
    ctx: ImmutableCtx,
    ty: CryptoVerifyType,
    pk: Vec<u8>,
    sig: Vec<u8>,
    msg_hash: Vec<u8>,
) -> StdResult<()> {
    match ty {
        CryptoVerifyType::Ed25519 => ctx.api.ed25519_verify(&msg_hash, &sig, &pk),
        CryptoVerifyType::Secp256k1 => ctx.api.secp256k1_verify(&msg_hash, &sig, &pk),
        CryptoVerifyType::Secp256r1 => ctx.api.secp256r1_verify(&msg_hash, &sig, &pk),
    }
}

pub fn query_crypto_recover_secp256k1(
    ctx: ImmutableCtx,
    sig: Vec<u8>,
    msg_hash: Vec<u8>,
    recovery_id: u8,
    compressed: bool,
) -> StdResult<Vec<u8>> {
    ctx.api
        .secp256k1_pubkey_recover(&msg_hash, &sig, recovery_id, compressed)
}

macro_rules! slice_of_slices {
    ($vec:expr) => {{
        let slice_of_slices: Vec<&[u8]> = $vec.iter().map(|v| &v[..]).collect();
        slice_of_slices
    }};
}

pub fn query_crypto_ed25519_batch_verify(
    ctx: ImmutableCtx,
    prehash_msgs: Vec<Vec<u8>>,
    sigs: Vec<Vec<u8>>,
    pks: Vec<Vec<u8>>,
) -> StdResult<()> {
    let m = slice_of_slices!(prehash_msgs);
    let s = slice_of_slices!(sigs);
    let p = slice_of_slices!(pks);

    ctx.api.ed25519_batch_verify(&m, &s, &p)
}
