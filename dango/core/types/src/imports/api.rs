use crate::{Addr, StdResult};

// Note: I prefer to use generics (e.g. `impl AsRef<[u8]>`) over `&[u8]` for
// input data, but by doing that the trait won't be object-safe (i.e. we won't
// be able to do `&dyn Api`). Traits with methods that have generic parameters
// can't be object-safe.
//
// Also note that trait methods must include `&self` in order to be object-safe.
pub trait Api {
    /// Send a message to the host, which will be printed to the host's logging.
    /// Takes two arguments: the contract's address as raw bytes, and the message
    /// as UTF-8 bytes.
    ///
    /// Note: unlike Rust's built-in `dbg!` macro, which is only included in
    /// debug builds, this `debug` method is also included in release builds,
    /// and incurs gas cost. Make sure to comment this out before compiling your
    /// contracts.
    fn debug(&self, addr: Addr, msg: &str);

    /// Verify an Secp256r1 signature with the given hashed message and public
    /// key.
    ///
    /// Note: this function takes the hash of the message, not the prehash.
    fn secp256r1_verify(&self, msg_hash: &[u8], sig: &[u8], pk: &[u8]) -> StdResult<()>;

    /// Verify an Secp256k1 signature with the given hashed message and public
    /// key.
    ///
    /// Note: this function takes the hash of the message, not the prehash.
    fn secp256k1_verify(&self, msg_hash: &[u8], sig: &[u8], pk: &[u8]) -> StdResult<()>;

    /// Recover the Secp256k1 public key from the signature over a message.
    ///
    /// Note: this function takes the hash of the message, not the prehash.
    fn secp256k1_pubkey_recover(
        &self,
        msg_hash: &[u8],
        sig: &[u8],
        recovery_id: u8,
        compressed: bool,
    ) -> StdResult<Vec<u8>>;

    /// Perform the SHA2-256 hash.
    fn sha2_256(&self, data: &[u8]) -> [u8; 32];

    /// Perform the Keccak-256 hash.
    fn keccak256(&self, data: &[u8]) -> [u8; 32];
}
