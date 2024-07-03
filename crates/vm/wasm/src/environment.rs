use {
    crate::{Iterator, VmError, VmResult, WasmVm},
    grug_app::{GasTracker, QuerierProvider, StorageProvider},
    grug_types::Record,
    std::{collections::HashMap, ptr::NonNull},
    wasmer::{AsStoreMut, AsStoreRef, Instance, Memory, MemoryView, Value},
};

pub struct Environment {
    pub storage: StorageProvider,
    pub querier: QuerierProvider<WasmVm>,
    pub gas_tracker: GasTracker,
    iterators: HashMap<i32, Iterator>,
    next_iterator_id: i32,
    wasmer_memory: Option<Memory>,
    /// A non-owning link to the wasmer instance. Need this for doing function
    /// calls (see Environment::call_function).
    wasmer_instance: Option<NonNull<Instance>>,
}

// Wasmer instance isn't Send/Sync. We manually mark it to be.
// cosmwasm_vm does the same:
// https://github.com/CosmWasm/cosmwasm/blob/v2.0.3/packages/vm/src/environment.rs#L120-L122
// TODO: need to think about whether this is safe
unsafe impl Send for Environment {}
unsafe impl Sync for Environment {}

impl Environment {
    pub fn new(
        storage: StorageProvider,
        querier: QuerierProvider<WasmVm>,
        gas_tracker: GasTracker,
    ) -> Self {
        Self {
            gas_tracker,
            storage,
            querier,
            iterators: HashMap::new(),
            next_iterator_id: 0,
            wasmer_memory: None,
            wasmer_instance: None,
        }
    }

    /// Add a new iterator to the environment, increment the next iterator ID.
    ///
    /// Return the ID of the iterator that was just added.
    pub fn add_iterator(&mut self, iterator: Iterator) -> i32 {
        let iterator_id = self.next_iterator_id;
        self.iterators.insert(iterator_id, iterator);
        self.next_iterator_id += 1;

        iterator_id
    }

    /// Get the next record in the iterator specified by the ID.
    ///
    /// Error if the iterator is not found.
    /// `None` if the iterator is found but has reached its end.
    pub fn advance_iterator(&mut self, iterator_id: i32) -> VmResult<Option<Record>> {
        self.iterators
            .get_mut(&iterator_id)
            .ok_or(VmError::IteratorNotFound { iterator_id })
            .map(|iter| iter.next(&self.storage))
    }

    pub fn set_wasmer_memory(&mut self, instance: &Instance) -> VmResult<()> {
        let memory = instance.exports.get_memory("memory")?;
        self.wasmer_memory = Some(memory.clone());
        Ok(())
    }

    pub fn set_wasmer_instance(&mut self, instance: &Instance) {
        self.wasmer_instance = Some(NonNull::from(instance));
    }

    pub fn get_wasmer_memory<'a>(
        &self,
        wasm_store: &'a impl AsStoreRef,
    ) -> VmResult<MemoryView<'a>> {
        self.wasmer_memory
            .as_ref()
            .ok_or(VmError::MemoryNotSet)
            .map(|mem| mem.view(wasm_store))
    }

    pub fn get_wasmer_instance(&self) -> VmResult<&Instance> {
        let instance_ptr = self.wasmer_instance.ok_or(VmError::WasmerInstanceNotSet)?;
        unsafe { Ok(instance_ptr.as_ref()) }
    }

    pub fn call_function1(
        &self,
        wasm_store: &mut impl AsStoreMut,
        name: &str,
        args: &[Value],
    ) -> VmResult<Value> {
        let ret = self.call_function(wasm_store, name, args)?;
        if ret.len() != 1 {
            return Err(VmError::ReturnCount {
                name: name.into(),
                expect: 1,
                actual: ret.len(),
            });
        }
        Ok(ret[0].clone())
    }

    pub fn call_function0(
        &self,
        wasm_store: &mut impl AsStoreMut,
        name: &str,
        args: &[Value],
    ) -> VmResult<()> {
        let ret = self.call_function(wasm_store, name, args)?;
        if ret.len() != 0 {
            return Err(VmError::ReturnCount {
                name: name.into(),
                expect: 0,
                actual: ret.len(),
            });
        }
        Ok(())
    }

    fn call_function(
        &self,
        wasm_store: &mut impl AsStoreMut,
        name: &str,
        args: &[Value],
    ) -> VmResult<Box<[Value]>> {
        self.get_wasmer_instance()?
            .exports
            .get_function(name)?
            .call(wasm_store, args)
            .map_err(Into::into)
    }
}
