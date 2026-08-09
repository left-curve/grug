use {
    crate::{Hash160, Hash256},
    ripemd::Ripemd160,
    sha2::{Digest, Sha256},
    sha3::Keccak256,
};

/// Represents a data that can be hashed.
pub trait HashExt {
    /// Produce a hash of the data using the RIPEMD-160 algorithm.
    fn ripemd160(&self) -> Hash160;

    /// Produce a hash of the data using the SHA2-256 algorithm.
    fn sha2_256(&self) -> Hash256;

    /// Produce a hash of the data using the Keccak256 algorithm.
    fn keccak256(&self) -> Hash256;
}

// Currently, we use RIPEMD-160 for 20-byte hashes, and SHA2-256 for 32-byte
// hashes. However, this can change prior to v1. We may consider BLAKE3 for `hash256`.
impl<T> HashExt for T
where
    T: AsRef<[u8]>,
{
    fn ripemd160(&self) -> Hash160 {
        let mut hasher = Ripemd160::new();
        hasher.update(self.as_ref());
        Hash160::from_inner(hasher.finalize().into())
    }

    fn sha2_256(&self) -> Hash256 {
        let mut hasher = Sha256::new();
        hasher.update(self.as_ref());
        Hash256::from_inner(hasher.finalize().into())
    }

    fn keccak256(&self) -> Hash256 {
        let mut hasher = Keccak256::new();
        hasher.update(self.as_ref());
        Hash256::from_inner(hasher.finalize().into())
    }
}
