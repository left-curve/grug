use {
    crate::{Iterator, VmError, VmResult, WasmVm},
    grug_app::{QuerierProvider, SharedGasTracker, StorageProvider},
    std::{
        borrow::{Borrow, BorrowMut},
        collections::HashMap,
        ptr::NonNull,
        sync::{Arc, RwLock},
    },
    wasmer::{AsStoreMut, AsStoreRef, Instance, Memory, MemoryView, Value},
};

// TODO: add explaination on why wasm_instance field needs to be Options
// it has to do with the procedure how we create the ContextData when building the instance
pub struct ContextData {
    pub storage: StorageProvider,
    pub querier: QuerierProvider<WasmVm>,
    pub iterators: HashMap<i32, Iterator>,
    pub next_iterator_id: i32,
    pub gas_tracker: SharedGasTracker,
    /// A non-owning link to the wasmer instance. Need this for doing function
    /// calls (see Environment::call_function).
    wasmer_instance: Option<NonNull<Instance>>,
}

// Wasmer instance isn't Send/Sync. We manually mark it to be.
// cosmwasm_vm does the same:
// https://github.com/CosmWasm/cosmwasm/blob/v2.0.3/packages/vm/src/environment.rs#L120-L122
// TODO: need to think about whether this is safe
unsafe impl Send for ContextData {}
unsafe impl Sync for ContextData {}

pub struct Environment {
    memory: Option<Memory>,
    data: Arc<RwLock<ContextData>>,
}

impl Environment {
    pub fn new(
        storage: StorageProvider,
        querier: QuerierProvider<WasmVm>,
        gas_tracker: SharedGasTracker,
    ) -> Self {
        Self {
            memory: None,
            data: Arc::new(RwLock::new(ContextData {
                gas_tracker,
                storage,
                querier,
                iterators: HashMap::new(),
                next_iterator_id: 0,
                wasmer_instance: None,
            })),
        }
    }

    pub fn memory<'a>(&self, wasm_store: &'a impl AsStoreRef) -> VmResult<MemoryView<'a>> {
        self.memory
            .as_ref()
            .ok_or(VmError::MemoryNotSet)
            .map(|mem| mem.view(wasm_store))
    }

    pub fn with_context_data<C, T, E>(&self, callback: C) -> VmResult<T>
    where
        C: FnOnce(&ContextData) -> Result<T, E>,
        E: Into<VmError>,
    {
        let guard = self.data.read().map_err(|_| VmError::FailedReadLock)?;
        callback(guard.borrow()).map_err(Into::into)
    }

    pub fn with_context_data_mut<C, T, E>(&mut self, callback: C) -> VmResult<T>
    where
        C: FnOnce(&mut ContextData) -> Result<T, E>,
        E: Into<VmError>,
    {
        let mut guard = self.data.write().map_err(|_| VmError::FailedWriteLock)?;
        callback(guard.borrow_mut()).map_err(Into::into)
    }

    pub fn with_wasm_instance<C, T, E>(&self, callback: C) -> VmResult<T>
    where
        C: FnOnce(&wasmer::Instance) -> Result<T, E>,
        E: Into<VmError>,
    {
        self.with_context_data(|ctx| {
            let instance_ptr = ctx.wasmer_instance.ok_or(VmError::WasmerInstanceNotSet)?;
            let instance_ref = unsafe { instance_ptr.as_ref() };
            callback(instance_ref).map_err(Into::into)
        })
    }

    pub fn set_memory(&mut self, wasm_instance: &Instance) -> VmResult<()> {
        let memory = wasm_instance.exports.get_memory("memory")?;
        self.memory = Some(memory.clone());
        Ok(())
    }

    pub fn set_wasm_instance(&mut self, wasm_instance: &Instance) -> VmResult<()> {
        self.with_context_data_mut(|ctx| -> VmResult<_> {
            ctx.wasmer_instance = Some(NonNull::from(wasm_instance));
            Ok(())
        })
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
        // Note: Calling with_wasm_instance creates a read lock on the
        // ContextData. We must drop this lock before calling the function,
        // otherwise we get a deadlock (calling require a write lock which has
        // to wait for the previous read lock being dropped).
        let func = self.with_wasm_instance(|wasm_instance| -> VmResult<_> {
            let f = wasm_instance.exports.get_function(name)?;
            Ok(f.clone())
        })?;

        func.call(wasm_store, args).map_err(Into::into)
    }
}
