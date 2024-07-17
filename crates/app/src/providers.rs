use {
    crate::{process_query, AppError, GasTracker, Vm},
    grug_types::{
        concat, increment_last_byte, BlockInfo, Order, Querier, QueryRequest, QueryResponse,
        Record, StdError, StdResult, Storage,
    },
};

// ---------------------------------- storage ----------------------------------

/// Provides access to an account's storage to the VM.
///
/// Essentially, this is a prefixed key-value storage. In Grug, the prefix is
/// the single byte `b"w"` (referring to Wasm) followed by the account address.
#[derive(Clone)]
pub struct StorageProvider {
    storage: Box<dyn Storage>,
    namespace: Vec<u8>,
}

impl StorageProvider {
    pub fn new(storage: Box<dyn Storage>, prefixes: &[&[u8]]) -> Self {
        let mut size = 0;
        for prefix in prefixes {
            size += prefix.len();
        }

        let mut namespace = Vec::with_capacity(size);
        for prefix in prefixes {
            namespace.extend_from_slice(prefix);
        }

        Self { storage, namespace }
    }
}

impl Storage for StorageProvider {
    fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
        let prefixed_key = concat(&self.namespace, key);
        self.storage.read(&prefixed_key)
    }

    fn scan<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        let (min, max) = prefixed_range_bounds(&self.namespace, min, max);
        let iter = self
            .storage
            .scan(Some(&min), Some(&max), order)
            .map(|(key, value)| (trim_wasm_prefix(&self.namespace, key), value));
        Box::new(iter)
    }

    fn scan_keys<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let (min, max) = prefixed_range_bounds(&self.namespace, min, max);
        let iter = self
            .storage
            .scan_keys(Some(&min), Some(&max), order)
            .map(|key| trim_wasm_prefix(&self.namespace, key));
        Box::new(iter)
    }

    fn scan_values<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let (min, max) = prefixed_range_bounds(&self.namespace, min, max);
        self.storage.scan_values(Some(&min), Some(&max), order)
    }

    fn write(&mut self, key: &[u8], value: &[u8]) {
        let prefixed_key = concat(&self.namespace, key);
        self.storage.write(&prefixed_key, value);
    }

    fn remove(&mut self, key: &[u8]) {
        let prefixed_key = concat(&self.namespace, key);
        self.storage.remove(&prefixed_key);
    }

    fn remove_range(&mut self, min: Option<&[u8]>, max: Option<&[u8]>) {
        let (min, max) = prefixed_range_bounds(&self.namespace, min, max);
        self.storage.remove_range(Some(&min), Some(&max))
    }

    fn scan_sized<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
        size: u32,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        let (min, max) = prefixed_range_bounds(&self.namespace, min, max);
        let iter = self
            .storage
            .scan_sized(Some(&min), Some(&max), order, size)
            .map(|(key, value)| (trim_wasm_prefix(&self.namespace, key), value));
        Box::new(iter)
    }
}

#[inline]
fn prefixed_range_bounds(
    prefix: &[u8],
    min: Option<&[u8]>,
    max: Option<&[u8]>,
) -> (Vec<u8>, Vec<u8>) {
    let min = match min {
        Some(bytes) => concat(prefix, bytes),
        None => prefix.to_vec(),
    };
    let max = match max {
        Some(bytes) => concat(prefix, bytes),
        None => increment_last_byte(prefix.to_vec()),
    };
    (min, max)
}

fn trim_wasm_prefix(namespace: &[u8], key: Vec<u8>) -> Vec<u8> {
    key[namespace.len()..].to_vec()
}
// ---------------------------------- querier ----------------------------------

/// Provides querier functionalities to the VM.
pub struct QuerierProvider<VM> {
    vm: VM,
    storage: Box<dyn Storage>,
    gas_tracker: GasTracker,
    block: BlockInfo,
}

impl<VM> QuerierProvider<VM> {
    pub fn new(
        vm: VM,
        storage: Box<dyn Storage>,
        gas_tracker: GasTracker,
        block: BlockInfo,
    ) -> Self {
        Self {
            vm,
            storage,
            gas_tracker,
            block,
        }
    }
}

impl<VM> Querier for QuerierProvider<VM>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    fn query_chain(&self, req: QueryRequest) -> StdResult<QueryResponse> {
        process_query(
            self.vm.clone(),
            self.storage.clone(),
            self.gas_tracker.clone(),
            self.block.clone(),
            req,
        )
        .map_err(|err| StdError::Generic(err.to_string()))
    }
}
