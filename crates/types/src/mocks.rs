use {
    crate::{Addr, Api, Order, Record, StdError, StdResult, Storage},
    std::{collections::BTreeMap, iter, ops::Bound},
};

// ---------------------------------- storage ----------------------------------

/// An in-memory KV store for testing purpose.
#[derive(Default, Debug, Clone)]
pub struct MockStorage {
    data: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl MockStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

macro_rules! range_bounds {
    ($min:ident, $max:ident) => {{
        // `BTreeMap::range` panics if
        // 1. start > end, or
        // 2. start == end and both are exclusive
        // For us, since we interpret min as inclusive and max as exclusive,
        // only the 1st case apply. However, we don't want to panic, we just
        // return an empty iterator.
        if let (Some(min), Some(max)) = ($min, $max) {
            if min > max {
                return Box::new(iter::empty());
            }
        }

        // Min is inclusive, max is exclusive.
        let min = $min.map_or(Bound::Unbounded, |bytes| Bound::Included(bytes.to_vec()));
        let max = $max.map_or(Bound::Unbounded, |bytes| Bound::Excluded(bytes.to_vec()));

        (min, max)
    }};
}

impl Storage for MockStorage {
    fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.data.get(key).cloned()
    }

    fn scan<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        let bounds = range_bounds!(min, max);
        let iter = self.data.range(bounds).map(|(k, v)| (k.clone(), v.clone()));
        match order {
            Order::Ascending => Box::new(iter),
            Order::Descending => Box::new(iter.rev()),
        }
    }

    fn scan_keys<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let bounds = range_bounds!(min, max);
        let iter = self.data.range(bounds).map(|(k, _)| k.clone());
        match order {
            Order::Ascending => Box::new(iter),
            Order::Descending => Box::new(iter.rev()),
        }
    }

    fn scan_values<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let bounds = range_bounds!(min, max);
        let iter = self.data.range(bounds).map(|(_, v)| v.clone());
        match order {
            Order::Ascending => Box::new(iter),
            Order::Descending => Box::new(iter.rev()),
        }
    }

    fn write(&mut self, key: &[u8], value: &[u8]) {
        self.data.insert(key.to_vec(), value.to_vec());
    }

    fn remove(&mut self, key: &[u8]) {
        self.data.remove(key);
    }

    fn remove_range(&mut self, min: Option<&[u8]>, max: Option<&[u8]>) {
        self.data.retain(|k, _| {
            if let Some(min) = min {
                if k.as_slice() < min {
                    return true;
                }
            }

            if let Some(max) = max {
                if max <= k.as_slice() {
                    return true;
                }
            }

            false
        });
    }

    fn scan_sized<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
        size: u32,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        // TODO: optimize this??
        Box::new(self.scan(min, max, order).take(size as usize))
    }
}

// ------------------------------------ api ------------------------------------

pub struct MockApi;

impl Api for MockApi {
    fn debug(&self, addr: &Addr, msg: &str) {
        println!("Contract emitted debug message! addr = {addr}, msg = {msg}");
    }

    fn secp256r1_verify(&self, msg_hash: &[u8], sig: &[u8], pk: &[u8]) -> StdResult<()> {
        grug_crypto::secp256r1_verify(msg_hash, sig, pk).map_err(|_| StdError::VerificationFailed)
    }

    fn secp256k1_verify(&self, msg_hash: &[u8], sig: &[u8], pk: &[u8]) -> StdResult<()> {
        grug_crypto::secp256k1_verify(msg_hash, sig, pk).map_err(|_| StdError::VerificationFailed)
    }

    fn secp256k1_pubkey_recover(
        &self,
        msg_hash: &[u8],
        sig: &[u8],
        recovery_id: u8,
        compressed: bool,
    ) -> StdResult<Vec<u8>> {
        grug_crypto::secp256k1_pubkey_recover(msg_hash, sig, recovery_id, compressed)
            .map_err(|_| StdError::VerificationFailed)
    }

    fn ed25519_verify(&self, msg_hash: &[u8], sig: &[u8], pk: &[u8]) -> StdResult<()> {
        grug_crypto::ed25519_verify(msg_hash, sig, pk).map_err(|_| StdError::VerificationFailed)
    }

    fn ed25519_batch_verify(
        &self,
        msgs_hash: &[&[u8]],
        sigs: &[&[u8]],
        pks: &[&[u8]],
    ) -> StdResult<()> {
        grug_crypto::ed25519_batch_verify(msgs_hash, sigs, pks)
            .map_err(|_| StdError::VerificationFailed)
    }

    fn sha2_256(&self, data: &[u8]) -> [u8; 32] {
        grug_crypto::sha2_256(data)
    }

    fn sha2_512(&self, data: &[u8]) -> [u8; 64] {
        grug_crypto::sha2_512(data)
    }

    fn sha2_512_truncated(&self, data: &[u8]) -> [u8; 32] {
        grug_crypto::sha2_512_truncated(data)
    }

    fn sha3_256(&self, data: &[u8]) -> [u8; 32] {
        grug_crypto::sha3_256(data)
    }

    fn sha3_512(&self, data: &[u8]) -> [u8; 64] {
        grug_crypto::sha3_512(data)
    }

    fn sha3_512_truncated(&self, data: &[u8]) -> [u8; 32] {
        grug_crypto::sha3_512_truncated(data)
    }

    fn keccak256(&self, data: &[u8]) -> [u8; 32] {
        grug_crypto::keccak256(data)
    }

    fn blake2s_256(&self, data: &[u8]) -> [u8; 32] {
        grug_crypto::blake2s_256(data)
    }

    fn blake2b_512(&self, data: &[u8]) -> [u8; 64] {
        grug_crypto::blake2b_512(data)
    }

    fn blake3(&self, data: &[u8]) -> [u8; 32] {
        grug_crypto::blake3(data)
    }
}
