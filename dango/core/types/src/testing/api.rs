use crate::{Addr, Api, StdResult, VerificationError};

/// A mock implementation of the [`Api`](crate::Api) trait for testing purpose.
pub struct MockApi;

impl Api for MockApi {
    fn debug(&self, addr: Addr, msg: &str) {
        println!("Contract emitted debug message! addr = {addr}, msg = {msg}");
    }

    fn secp256r1_verify(&self, msg_hash: &[u8], sig: &[u8], pk: &[u8]) -> StdResult<()> {
        dango_crypto::secp256r1_verify(msg_hash, sig, pk)
            .map_err(|err| VerificationError::from_error_code(err.into_error_code()).into())
    }

    fn secp256k1_verify(&self, msg_hash: &[u8], sig: &[u8], pk: &[u8]) -> StdResult<()> {
        dango_crypto::secp256k1_verify(msg_hash, sig, pk)
            .map_err(|err| VerificationError::from_error_code(err.into_error_code()).into())
    }

    fn secp256k1_pubkey_recover(
        &self,
        msg_hash: &[u8],
        sig: &[u8],
        recovery_id: u8,
        compressed: bool,
    ) -> StdResult<Vec<u8>> {
        dango_crypto::secp256k1_pubkey_recover(msg_hash, sig, recovery_id, compressed)
            .map_err(|err| VerificationError::from_error_code(err.into_error_code()).into())
    }

    fn sha2_256(&self, data: &[u8]) -> [u8; 32] {
        dango_crypto::sha2_256(data)
    }

    fn keccak256(&self, data: &[u8]) -> [u8; 32] {
        dango_crypto::keccak256(data)
    }
}
