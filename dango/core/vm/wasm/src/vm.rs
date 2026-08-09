use {
    crate::{
        Cache, Environment, Gatekeeper, LimitingTunables, VmError, VmResult, db_next, db_next_key,
        db_next_value, db_read, db_remove, db_remove_range, db_scan, db_write, debug, keccak256,
        query_chain, read_then_wipe, secp256k1_pubkey_recover, secp256k1_verify, secp256r1_verify,
        sha2_256, write_to_memory,
    },
    dango_app::{GasTracker, Instance, QuerierProvider, StorageProvider, Vm},
    dango_primitives::{BorshSerExt, Context, Hash256},
    std::{num::NonZeroUsize, sync::Arc},
    wasmer::{
        Engine, Function, FunctionEnv, Module, Store, StoreMut, WASM_PAGE_SIZE, imports,
        sys::{BaseTunables, CompilerConfig, NativeEngineExt, Singlepass, Target},
    },
    wasmer_middlewares::{Metering, metering::set_remaining_points},
};

/// Gas cost per Wasmer operation.
pub const GAS_PER_OPERATION: u64 = 1;

/// Maximum number of chained queries.
///
/// E.g. contract A queries contract B; when handling this query, contract B
/// calls contract C; so on.
///
/// Without a limit, this can leads to stack overflow which halts the chain.
pub const MAX_QUERY_DEPTH: usize = 3;

/// Wasm memory size limit in MiB.
///
/// ## Note
///
/// Do not confuse MiB (mebibyte) with MB (megabyte):
///
/// - 1 MiB = 1,024 KiB = 1,024 * 1,024 bytes
/// - 1 MB  = 1,000 KB  = 1,000 * 1,000 bytes
pub const MAX_MEMORY_MEBI: usize = 32;

/// Wasm memory size limit in pages.
///
/// In WebAssembly, a memory page is 64 KiB. There can be a maximum of 65,536
/// pages.
///
/// In Dango, we limit each contract instance's memory to 32 MiB or 512 pages,
/// consistent with [CosmWasm](https://github.com/CosmWasm/wasmd/blob/v0.53.0/x/wasm/keeper/keeper.go#L38-L40).
pub const MAX_MEMORY_PAGES: u32 = (MAX_MEMORY_MEBI * 1024 * 1024 / WASM_PAGE_SIZE) as u32;

// Assert at compile time that our instance memory is always smaller than the
// max memory that Wasmer can support.
const _: () = assert!(MAX_MEMORY_PAGES < wasmer::WASM_MAX_PAGES);

// ------------------------------------ vm -------------------------------------

#[derive(Clone)]
pub struct WasmVm {
    cache: Option<Cache>,
}

impl WasmVm {
    pub fn new(cache_capacity: usize) -> Self {
        Self {
            cache: NonZeroUsize::new(cache_capacity).map(Cache::new),
        }
    }
}

impl Vm for WasmVm {
    type Error = VmError;
    type Instance = WasmInstance;

    fn build_instance(
        &mut self,
        code: &[u8],
        code_hash: Hash256,
        storage: StorageProvider,
        state_mutable: bool,
        querier: Box<dyn QuerierProvider>,
        query_depth: usize,
        gas_tracker: GasTracker,
    ) -> VmResult<WasmInstance> {
        if query_depth > MAX_QUERY_DEPTH {
            return Err(VmError::exceed_max_query_depth());
        }

        let (module, engine) = if let Some(cache) = &self.cache {
            // Attempt to fetch a pre-built Wasmer module from the cache.
            // If not found, build it and insert it into the cache.
            cache.get_or_build_with(code_hash, || compile_wasmer(code))?
        } else {
            compile_wasmer(code)?
        };

        // Compute the amount of gas left for this call. This will be used as
        // the initial points in the Wasmer gas meter.
        //
        // E.g. If the tx gas limit is X, the very first call in the tx (which
        // would be the `authenticate` call) will have the limit as X. Suppose
        // this call consumed Y gas points, the next call will have its limit as
        // (X-Y); so on.
        let gas_remaining = gas_tracker.remaining().unwrap_or(u64::MAX);

        // Create the Wasmer store, function environment, and register import
        // functions.
        //
        // Note: The Wasmer instance hasn't been created at this point, so
        // `wasmer_memory` and `wasmer_instance` in the `Environment` are left
        // empty for now.
        let mut store = Store::new(engine);
        let fe = FunctionEnv::new(
            &mut store,
            Environment::new(
                storage,
                state_mutable,
                querier,
                query_depth,
                gas_tracker.clone(),
                gas_remaining,
            ),
        );
        let import_obj = imports! {
            "env" => {
                "db_read"                  => Function::new_typed_with_env(&mut store, &fe, db_read),
                "db_scan"                  => Function::new_typed_with_env(&mut store, &fe, db_scan),
                "db_next"                  => Function::new_typed_with_env(&mut store, &fe, db_next),
                "db_next_key"              => Function::new_typed_with_env(&mut store, &fe, db_next_key),
                "db_next_value"            => Function::new_typed_with_env(&mut store, &fe, db_next_value),
                "db_write"                 => Function::new_typed_with_env(&mut store, &fe, db_write),
                "db_remove"                => Function::new_typed_with_env(&mut store, &fe, db_remove),
                "db_remove_range"          => Function::new_typed_with_env(&mut store, &fe, db_remove_range),
                "secp256k1_verify"         => Function::new_typed_with_env(&mut store, &fe, secp256k1_verify),
                "secp256r1_verify"         => Function::new_typed_with_env(&mut store, &fe, secp256r1_verify),
                "secp256k1_pubkey_recover" => Function::new_typed_with_env(&mut store, &fe, secp256k1_pubkey_recover),
                "sha2_256"                 => Function::new_typed_with_env(&mut store, &fe, sha2_256),
                "keccak256"                => Function::new_typed_with_env(&mut store, &fe, keccak256),
                "debug"                    => Function::new_typed_with_env(&mut store, &fe, debug),
                "query_chain"              => Function::new_typed_with_env(&mut store, &fe, query_chain),
            }
        };

        // create wasmer instance
        let instance = wasmer::Instance::new(&mut store, &module, &import_obj)?;
        let instance = Box::new(instance);

        // Set gas limit on the metering
        set_remaining_points(&mut store, &instance, gas_remaining);

        // set memory/store/instance in the env
        let env = fe.as_mut(&mut store);
        env.set_wasmer_memory(&instance)?;
        env.set_wasmer_instance(instance.as_ref())?;

        Ok(WasmInstance {
            _instance: instance,
            store,
            fe,
        })
    }
}

fn compile_wasmer(code: &[u8]) -> VmResult<(Module, Engine)> {
    let mut compiler = Singlepass::new();

    // Set up the gas metering middleware.
    //
    // Set `initial_points` as zero for now, because this engine will be
    // cached and to be used by other transactions, so it doesn't make
    // sense to put the current tx's gas limit here.
    //
    // We will properly set this tx's gas limit later, once we have
    // created the `Instance`.
    //
    // Also, compiling the module doesn't cost gas, so setting the limit
    // to zero won't raise out of gas errors.
    let metering = Metering::new(0, |_| GAS_PER_OPERATION);
    compiler.push_middleware(Arc::new(metering));

    // Set up the `Gatekeeper`. This rejects certain Wasm operators that
    // may cause non-determinism.
    compiler.push_middleware(Arc::new(Gatekeeper::default()));

    // Ensure determinism related to floating point numbers.
    compiler.canonicalize_nans(true);

    let mut engine = Engine::from(compiler);

    // Set memory limit for Wasm instances.
    let base = BaseTunables::for_target(&Target::default());
    let tunables = LimitingTunables::new(base, MAX_MEMORY_PAGES);
    engine.set_tunables(tunables);

    let module = Module::new(&engine, code)?;

    Ok((module, engine))
}

// --------------------------------- instance ----------------------------------

pub struct WasmInstance {
    _instance: Box<wasmer::Instance>,
    store: Store,
    fe: FunctionEnv<Environment>,
}

impl WasmInstance {
    fn use_env_mut<T, F>(&mut self, callback: F) -> T
    where
        F: FnOnce(&mut Environment, &mut StoreMut) -> T,
    {
        let mut fe_mut = self.fe.clone().into_mut(&mut self.store);
        let (env, mut store) = fe_mut.data_and_store_mut();
        callback(env, &mut store)
    }
}

impl Instance for WasmInstance {
    type Error = VmError;

    fn call_in_0_out_1(mut self, name: &'static str, ctx: &Context) -> VmResult<Vec<u8>> {
        self.use_env_mut(|env, store| {
            let ctx_ptr = write_to_memory(env, store, &ctx.to_borsh_vec()?)?;
            let res_ptr: u32 = env
                .call_function1(store, name, &[ctx_ptr.into()])?
                .try_into()
                .map_err(VmError::return_type)?;

            let data = read_then_wipe(env, store, res_ptr)?;

            Ok(data)
        })
    }

    fn call_in_1_out_1<P>(
        mut self,
        name: &'static str,
        ctx: &Context,
        param: &P,
    ) -> VmResult<Vec<u8>>
    where
        P: AsRef<[u8]>,
    {
        self.use_env_mut(|env, store| {
            let ctx_ptr = write_to_memory(env, store, &ctx.to_borsh_vec()?)?;
            let param1_ptr = write_to_memory(env, store, param.as_ref())?;
            let res_ptr: u32 = env
                .call_function1(store, name, &[ctx_ptr.into(), param1_ptr.into()])?
                .try_into()
                .map_err(VmError::return_type)?;

            let data = read_then_wipe(env, store, res_ptr)?;

            Ok(data)
        })
    }

    fn call_in_2_out_1<P1, P2>(
        mut self,
        name: &'static str,
        ctx: &Context,
        param1: &P1,
        param2: &P2,
    ) -> VmResult<Vec<u8>>
    where
        P1: AsRef<[u8]>,
        P2: AsRef<[u8]>,
    {
        self.use_env_mut(|env, store| {
            let ctx_ptr = write_to_memory(env, store, &ctx.to_borsh_vec()?)?;
            let param1_ptr = write_to_memory(env, store, param1.as_ref())?;
            let param2_ptr = write_to_memory(env, store, param2.as_ref())?;
            let res_ptr: u32 = env
                .call_function1(
                    store,
                    name,
                    &[ctx_ptr.into(), param1_ptr.into(), param2_ptr.into()],
                )?
                .try_into()
                .map_err(VmError::return_type)?;
            let data = read_then_wipe(env, store, res_ptr)?;

            Ok(data)
        })
    }
}
