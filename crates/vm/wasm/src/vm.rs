use {
    crate::{
        blake2b_512, blake2s_256, blake3, db_next, db_next_key, db_next_value, db_read, db_remove,
        db_remove_range, db_scan, db_write, debug, ed25519_batch_verify, ed25519_verify, keccak256,
        query_chain, read_then_wipe, secp256k1_pubkey_recover, secp256k1_verify, secp256r1_verify,
        sha2_256, sha2_512, sha2_512_truncated, sha3_256, sha3_512, sha3_512_truncated,
        write_to_memory, Cache, Environment, MemoryLimit, VmError, VmResult,
    },
    grug_app::{GasTracker, Instance, Memory, QuerierProvider, StorageProvider, Vm},
    grug_types::{to_borsh_vec, Context, Hash},
    std::{num::NonZeroUsize, sync::Arc},
    wasmer::{
        imports, CompilerConfig, Engine, Function, FunctionEnv, Module, NativeEngineExt,
        Singlepass, Store, Target,
    },
    wasmer_middlewares::{metering::set_remaining_points, Metering},
};

/// Gas cost per operation
///
/// TODO: Mocked to 1 now, need to be discussed
const GAS_PER_OPERATION: u64 = 1;

// ------------------------------------ vm -------------------------------------

#[derive(Clone)]
pub struct WasmVm {
    cache: Cache,
}

impl WasmVm {
    pub fn new(cache_capacity: usize) -> Self {
        // TODO: handle the case where cache capacity is zero (which means not to use a cache)
        Self {
            cache: Cache::new(NonZeroUsize::new(cache_capacity).unwrap()),
        }
    }
}

impl Vm for WasmVm {
    type Error = VmError;
    type Instance = WasmInstance;

    fn build_instance(
        &mut self,
        code: &[u8],
        code_hash: &Hash,
        storage: StorageProvider,
        storage_readonly: bool,
        querier: QuerierProvider<Self>,
        gas_tracker: GasTracker,
        memory: Memory,
    ) -> VmResult<WasmInstance> {
        // Attempt to fetch a pre-built Wasmer module from the cache.
        // If not found, build it and insert it into the cache.
        let (module, engine) = self.cache.get_or_build_with(code_hash, || {
            let mut compiler = Singlepass::new();
            compiler.canonicalize_nans(true);

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

            let mut engine = Engine::from(compiler);

            let Memory::StaticLimited(bytes) = memory;
            let tunables = MemoryLimit::new(
                bytes,
                wasmer::sys::BaseTunables::for_target(&Target::default()),
            );
            engine.set_tunables(tunables);
            let module = Module::new(&engine, code)?;

            Ok((module, engine))
        })?;

        // Compute the amount of gas left for this call. This will be used as
        // the initial points in the Wasmer gas meter.
        //
        // E.g. If the tx gas limit is X, the very first call in the tx (which
        // would be the `before_tx` call) will have the limit as X. Suppose this
        // call consumed Y gas points, the next call will have its limit as (X-Y);
        // so on.
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
                storage_readonly,
                querier,
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
                "ed25519_verify"           => Function::new_typed_with_env(&mut store, &fe, ed25519_verify),
                "ed25519_batch_verify"     => Function::new_typed_with_env(&mut store, &fe, ed25519_batch_verify),
                "sha2_256"                 => Function::new_typed_with_env(&mut store, &fe, sha2_256),
                "sha2_512"                 => Function::new_typed_with_env(&mut store, &fe, sha2_512),
                "sha2_512_truncated"       => Function::new_typed_with_env(&mut store, &fe, sha2_512_truncated),
                "sha3_256"                 => Function::new_typed_with_env(&mut store, &fe, sha3_256),
                "sha3_512"                 => Function::new_typed_with_env(&mut store, &fe, sha3_512),
                "sha3_512_truncated"       => Function::new_typed_with_env(&mut store, &fe, sha3_512_truncated),
                "keccak256"                => Function::new_typed_with_env(&mut store, &fe, keccak256),
                "blake2s_256"              => Function::new_typed_with_env(&mut store, &fe, blake2s_256),
                "blake2b_512"              => Function::new_typed_with_env(&mut store, &fe, blake2b_512),
                "blake3"                   => Function::new_typed_with_env(&mut store, &fe, blake3),
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

// --------------------------------- instance ----------------------------------

pub struct WasmInstance {
    _instance: Box<wasmer::Instance>,
    store: Store,
    fe: FunctionEnv<Environment>,
}

impl Instance for WasmInstance {
    type Error = VmError;

    fn call_in_0_out_1(mut self, name: &str, ctx: &Context) -> VmResult<Vec<u8>> {
        let mut fe_mut = self.fe.clone().into_mut(&mut self.store);
        let (env, mut store) = fe_mut.data_and_store_mut();

        let ctx_ptr = write_to_memory(env, &mut store, &to_borsh_vec(ctx)?)?;
        let res_ptr: u32 = env
            .call_function1(&mut store, name, &[ctx_ptr.into()])?
            .try_into()
            .map_err(VmError::ReturnType)?;

        let data = read_then_wipe(env, &mut store, res_ptr)?;

        Ok(data)
    }

    fn call_in_1_out_1<P>(mut self, name: &str, ctx: &Context, param: &P) -> VmResult<Vec<u8>>
    where
        P: AsRef<[u8]>,
    {
        let mut fe_mut = self.fe.clone().into_mut(&mut self.store);
        let (env, mut store) = fe_mut.data_and_store_mut();

        let ctx_ptr = write_to_memory(env, &mut store, &to_borsh_vec(ctx)?)?;
        let param1_ptr = write_to_memory(env, &mut store, param.as_ref())?;
        let res_ptr: u32 = env
            .call_function1(&mut store, name, &[ctx_ptr.into(), param1_ptr.into()])?
            .try_into()
            .map_err(VmError::ReturnType)?;

        let data = read_then_wipe(env, &mut store, res_ptr)?;

        Ok(data)
    }

    fn call_in_2_out_1<P1, P2>(
        mut self,
        name: &str,
        ctx: &Context,
        param1: &P1,
        param2: &P2,
    ) -> VmResult<Vec<u8>>
    where
        P1: AsRef<[u8]>,
        P2: AsRef<[u8]>,
    {
        let mut fe_mut = self.fe.clone().into_mut(&mut self.store);
        let (env, mut store) = fe_mut.data_and_store_mut();

        let ctx_ptr = write_to_memory(env, &mut store, &to_borsh_vec(ctx)?)?;
        let param1_ptr = write_to_memory(env, &mut store, param1.as_ref())?;
        let param2_ptr = write_to_memory(env, &mut store, param2.as_ref())?;
        let res_ptr: u32 = env
            .call_function1(&mut store, name, &[
                ctx_ptr.into(),
                param1_ptr.into(),
                param2_ptr.into(),
            ])?
            .try_into()
            .map_err(VmError::ReturnType)?;
        let data = read_then_wipe(env, &mut store, res_ptr)?;

        Ok(data)
    }
}
