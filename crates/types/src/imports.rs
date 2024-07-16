//! This file describes the import API that the host provides to Wasm modules.
//!
//! Three types of import functions are provided:
//!
//! - database reads/writes,
//! - cryptography methods, and
//! - a method for querying the chain.
//!
//! These functions are abstracted into the `Storage`, `Api`, and `Querier`
//! traits.

use {
    crate::{
        from_json_value, to_json_value, AccountResponse, Addr, Batch, Binary, Coins, Hash,
        InfoResponse, Op, Order, QueryRequest, QueryResponse, Record, StdResult, Uint128,
    },
    dyn_clone::DynClone,
    serde::{de::DeserializeOwned, ser::Serialize},
};

// ---------------------------------- storage ----------------------------------

/// Describing a KV store that supports read, write, and iteration.
///
/// Note that the store must be clone-able, which is required by Wasmer runtime.
/// We can't use the std library Clone trait, which is not object-safe.
/// We use [DynClone](https://crates.io/crates/dyn-clone) instead, which is
/// object-safe, and use the `clone_trait_object!` macro below to derive std
/// Clone trait for any type that implements Storage.
///
/// The object must also be Send and Sync, which is required by Wasmer runtime.
pub trait Storage: DynClone + Send + Sync {
    /// Read a single key-value pair from the storage.
    ///
    /// Return `None` if the key doesn't exist.
    fn read(&self, key: &[u8]) -> Option<Vec<u8>>;

    /// Iterate over data in the KV store under the given bounds and order.
    ///
    /// Minimum bound is inclusive, maximum bound is exclusive.
    /// If `min` > `max`, an empty iterator is to be returned.
    ///
    /// Note: This is different from the behavior of Rust's `BTreeMap`, which
    /// panics if `min` > `max`.
    fn scan<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a>;

    /// Similar to `scan`, but only return the keys.
    fn scan_keys<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a>;

    /// Similar to `scan`, but only return the values.
    fn scan_values<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a>;

    /// Write a single key-value pair to the storage.
    fn write(&mut self, key: &[u8], value: &[u8]);

    /// Delete a single key-value pair from the storage.
    ///
    /// No-op if the key doesn't exist.
    fn remove(&mut self, key: &[u8]);

    /// Delete all key-value pairs whose keys are in the given range.
    ///
    /// Similar to `scan`, `min` is inclusive, while `max` is exclusive.
    /// No-op if `min` > `max`.
    fn remove_range(&mut self, min: Option<&[u8]>, max: Option<&[u8]>);

    /// Perform a batch of writes and removes altogether, ideally atomically.
    ///
    /// The batch is provided by value instead of by reference (unlike other
    /// trait methods above) because in some implementations a copy/clone can be
    /// avoided this way, improving performance.
    ///
    /// The default implementation here is just looping through the ops and
    /// applying them one by one, which is inefficient and not atomic.
    /// Overwrite this implementation if there are more efficient approaches.
    fn flush(&mut self, batch: Batch) {
        for (key, op) in batch {
            if let Op::Insert(value) = op {
                self.write(&key, &value);
            } else {
                self.remove(&key);
            }
        }
    }

    fn scan_sized<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
        size: u32,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        Box::new(self.scan(min, max, order).take(size as usize))
    }
}

// A boxed `Storage` is also a `Storage`.
//
// We need to use dynamic dispatch (i.e. `&dyn Storage` and `Box<dyn Storage>`)
// very often in Grug, because of the use of recursive in handling submessages.
// Each layer of recursion, the storage is wrapped in a `CachedStore<T>`. If
// using static dispatch, the compiler will go into infinite nesting:
// `CachedStore<CachedStore<CachedStore<...>>>` until it reaches the recursion
// limit (default to 128) and we get a compiler error.
impl Storage for Box<dyn Storage> {
    fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.as_ref().read(key)
    }

    fn scan<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        self.as_ref().scan(min, max, order)
    }

    fn scan_keys<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        self.as_ref().scan_keys(min, max, order)
    }

    fn scan_values<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        self.as_ref().scan_values(min, max, order)
    }

    fn write(&mut self, key: &[u8], value: &[u8]) {
        self.as_mut().write(key, value)
    }

    fn remove(&mut self, key: &[u8]) {
        self.as_mut().remove(key)
    }

    fn remove_range(&mut self, min: Option<&[u8]>, max: Option<&[u8]>) {
        self.as_mut().remove_range(min, max)
    }

    fn flush(&mut self, batch: Batch) {
        self.as_mut().flush(batch)
    }
}

// derive std Clone trait for any type that implements Storage
dyn_clone::clone_trait_object!(Storage);

// ------------------------------------ api ------------------------------------

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
    fn debug(&self, addr: &Addr, msg: &str);

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

    /// Recover the compressed byte of the `public key` from the `signature` and `message hash`.
    /// - **r**: the first `32 bytes` of the signature;
    /// - **s**: the last `32 bytes` of the signature;
    /// - **v**: the `recovery id`.
    ///
    /// Note: this function takes the hash of the message, not the prehash.
    fn secp256k1_pubkey_recover(
        &self,
        msg_hash: &[u8],
        sig: &[u8],
        recovery_id: u8,
        compressed: bool,
    ) -> StdResult<Vec<u8>>;

    /// Verify an ED25519 signature with the given hashed message and public
    /// key.
    ///
    /// NOTE: This function takes the hash of the message, not the prehash.
    fn ed25519_verify(&self, msg_hash: &[u8], sig: &[u8], pk: &[u8]) -> StdResult<()>;

    /// Verify a batch of ED25519 signatures with the given hashed message and public
    /// key.
    /// NOTE: This function takes the hash of the messages, not the prehash.
    fn ed25519_batch_verify(
        &self,
        msgs_hash: &[&[u8]],
        sigs: &[&[u8]],
        pks: &[&[u8]],
    ) -> StdResult<()>;

    /// Perform the SHA2-256 hash.
    fn sha2_256(&self, data: &[u8]) -> [u8; 32];

    /// Perform the SHA2-512 hash.
    fn sha2_512(&self, data: &[u8]) -> [u8; 64];

    /// Perform the SHA2-512 hash, truncated to the first 32 bytes.
    fn sha2_512_truncated(&self, data: &[u8]) -> [u8; 32];

    /// Perform the SHA3-256 hash.
    fn sha3_256(&self, data: &[u8]) -> [u8; 32];

    /// Perform the SHA3-512 hash.
    fn sha3_512(&self, data: &[u8]) -> [u8; 64];

    /// Perform the SHA3-512 hash, truncated to the first 32 bytes.
    fn sha3_512_truncated(&self, data: &[u8]) -> [u8; 32];

    /// Perform the Keccak-256 hash.
    fn keccak256(&self, data: &[u8]) -> [u8; 32];

    /// Perform the BLAKE2s-256 hash.
    fn blake2s_256(&self, data: &[u8]) -> [u8; 32];

    /// Perform the BLAKE2b-512 hash.
    fn blake2b_512(&self, data: &[u8]) -> [u8; 64];

    /// Perform the BLAKE3 hash.
    fn blake3(&self, data: &[u8]) -> [u8; 32];
}

// ---------------------------------- querier ----------------------------------

pub trait Querier {
    /// Make a query. This is the only method that the context needs to manually
    /// implement. The other methods will be implemented automatically.
    fn query_chain(&self, req: QueryRequest) -> StdResult<QueryResponse>;
}

/// Wraps around a `Querier` to provide some convenience methods.
///
/// This is necessary because the `query_wasm_smart` method involves generics,
/// and a traits with generic methods isn't object-safe (i.e. we won't be able
/// to do `&dyn Querier`).
pub struct QuerierWrapper<'a> {
    inner: &'a dyn Querier,
}

impl<'a> QuerierWrapper<'a> {
    pub fn new(inner: &'a dyn Querier) -> Self {
        Self { inner }
    }

    pub fn query(&self, req: QueryRequest) -> StdResult<QueryResponse> {
        self.inner.query_chain(req)
    }

    pub fn query_info(&self) -> StdResult<InfoResponse> {
        self.inner
            .query_chain(QueryRequest::Info {})
            .map(|res| res.as_info())
    }

    pub fn query_balance(&self, address: Addr, denom: String) -> StdResult<Uint128> {
        self.inner
            .query_chain(QueryRequest::Balance { address, denom })
            .map(|res| res.as_balance().amount)
    }

    pub fn query_balances(
        &self,
        address: Addr,
        start_after: Option<String>,
        limit: Option<u32>,
    ) -> StdResult<Coins> {
        self.inner
            .query_chain(QueryRequest::Balances {
                address,
                start_after,
                limit,
            })
            .map(|res| res.as_balances())
    }

    pub fn query_supply(&self, denom: String) -> StdResult<Uint128> {
        self.inner
            .query_chain(QueryRequest::Supply { denom })
            .map(|res| res.as_supply().amount)
    }

    pub fn query_supplies(
        &self,
        start_after: Option<String>,
        limit: Option<u32>,
    ) -> StdResult<Coins> {
        self.inner
            .query_chain(QueryRequest::Supplies { start_after, limit })
            .map(|res| res.as_supplies())
    }

    pub fn query_code(&self, hash: Hash) -> StdResult<Binary> {
        self.inner
            .query_chain(QueryRequest::Code { hash })
            .map(|res| res.as_code())
    }

    pub fn query_codes(
        &self,
        start_after: Option<Hash>,
        limit: Option<u32>,
    ) -> StdResult<Vec<Hash>> {
        self.inner
            .query_chain(QueryRequest::Codes { start_after, limit })
            .map(|res| res.as_codes())
    }

    pub fn query_account(&self, address: Addr) -> StdResult<AccountResponse> {
        self.inner
            .query_chain(QueryRequest::Account { address })
            .map(|res| res.as_account())
    }

    pub fn query_accounts(
        &self,
        start_after: Option<Addr>,
        limit: Option<u32>,
    ) -> StdResult<Vec<AccountResponse>> {
        self.inner
            .query_chain(QueryRequest::Accounts { start_after, limit })
            .map(|res| res.as_accounts())
    }

    pub fn query_wasm_raw(&self, contract: Addr, key: Binary) -> StdResult<Option<Binary>> {
        self.inner
            .query_chain(QueryRequest::WasmRaw { contract, key })
            .map(|res| res.as_wasm_raw().value)
    }

    pub fn query_wasm_smart<M: Serialize, R: DeserializeOwned>(
        &self,
        contract: Addr,
        msg: &M,
    ) -> StdResult<R> {
        self.inner
            .query_chain(QueryRequest::WasmSmart {
                contract,
                msg: to_json_value(msg)?,
            })
            .and_then(|res| from_json_value(res.as_wasm_smart().data))
    }
}
