use {
    crate::{
        blake2b_512, blake2s_256, blake3, db_next, db_next_key, db_next_value, db_read, db_remove,
        db_remove_range, db_scan, db_write, debug, ed25519_batch_verify, ed25519_verify, keccak256,
        query_chain, read_then_wipe, secp256k1_pubkey_recover, secp256k1_verify, secp256r1_verify,
        sha2_256, sha2_512, sha2_512_truncated, sha3_256, sha3_512, sha3_512_truncated,
        write_to_memory, Cache, Environment, VmError, VmResult,
    },
    grug_app::{Instance, QuerierProvider, SharedGasTracker, StorageProvider, Vm},
    grug_types::{hash, to_borsh_vec, Context},
    std::{num::NonZeroUsize, sync::Arc},
    wasmer::{imports, CompilerConfig, Engine, Function, FunctionEnv, Module, Singlepass, Store},
    wasmer_middlewares::{
        metering::{get_remaining_points, set_remaining_points, MeteringPoints},
        Metering,
    },
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
        storage: StorageProvider,
        querier: QuerierProvider<Self>,
        code: &[u8],
        gas_tracker: SharedGasTracker,
    ) -> VmResult<WasmInstance> {
        let code_hash = hash(code);
        let (module, engine) = self.cache.get_or_build_with(&code_hash, || {
            let mut compiler = Singlepass::new();
            let metering = Metering::new(u64::MAX, |_| GAS_PER_OPERATION);
            compiler.canonicalize_nans(true);
            compiler.push_middleware(Arc::new(metering));
            let engine = Engine::from(compiler);
            let module = Module::new(&engine, code)?;
            Ok((module, engine))
        })?;

        // create Wasm store
        // for now we use the singlepass compiler
        let mut store = Store::new(engine);

        // create function environment and register imports
        // note: memory/store/instance in the env hasn't been set yet at this point
        let fe = FunctionEnv::new(
            &mut store,
            Environment::new(storage, querier, gas_tracker.clone()),
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

        // set memory/store/instance in the env
        let env = fe.as_mut(&mut store);
        env.set_memory(&instance)?;
        env.set_wasm_instance(instance.as_ref())?;

        Ok(WasmInstance {
            instance,
            store,
            fe,
            gas_tracker,
        })
    }
}

// --------------------------------- instance ----------------------------------

pub struct WasmInstance {
    instance: Box<wasmer::Instance>,
    store: Store,
    fe: FunctionEnv<Environment>,
    gas_tracker: SharedGasTracker,
}

impl WasmInstance {
    fn consume_gas(&mut self) -> VmResult<()> {
        match get_remaining_points(&mut self.store, &self.instance) {
            MeteringPoints::Remaining(remaining) => {
                // Reset gas consumed
                set_remaining_points(&mut self.store, &self.instance, u64::MAX);
                self.gas_tracker
                    .write_access()
                    .deduct(u64::MAX - remaining)?;
                Ok(())
            },
            MeteringPoints::Exhausted => {
                panic!("Out of gas, this should have been caught earlier!")
            },
        }
    }
}

impl Instance for WasmInstance {
    type Error = VmError;

    fn call_in_0_out_1(mut self, name: &str, ctx: &Context) -> VmResult<Vec<u8>> {
        let mut fe_mut = self.fe.clone().into_mut(&mut self.store);
        let (env, mut wasm_store) = fe_mut.data_and_store_mut();

        let ctx_ptr = write_to_memory(env, &mut wasm_store, &to_borsh_vec(ctx)?)?;
        let res_ptr: u32 = env
            .call_function1(&mut wasm_store, name, &[ctx_ptr.into()])?
            .try_into()
            .map_err(VmError::ReturnType)?;

        let data = read_then_wipe(env, &mut wasm_store, res_ptr)?;

        self.consume_gas()?;

        Ok(data)
    }

    fn call_in_1_out_1<P>(mut self, name: &str, ctx: &Context, param: &P) -> VmResult<Vec<u8>>
    where
        P: AsRef<[u8]>,
    {
        let mut fe_mut = self.fe.clone().into_mut(&mut self.store);
        let (env, mut wasm_store) = fe_mut.data_and_store_mut();

        let ctx_ptr = write_to_memory(env, &mut wasm_store, &to_borsh_vec(ctx)?)?;
        let param1_ptr = write_to_memory(env, &mut wasm_store, param.as_ref())?;
        let res_ptr: u32 = env
            .call_function1(&mut wasm_store, name, &[ctx_ptr.into(), param1_ptr.into()])?
            .try_into()
            .map_err(VmError::ReturnType)?;

        let data = read_then_wipe(env, &mut wasm_store, res_ptr)?;

        self.consume_gas()?;

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
        let (env, mut wasm_store) = fe_mut.data_and_store_mut();

        let ctx_ptr = write_to_memory(env, &mut wasm_store, &to_borsh_vec(ctx)?)?;
        let param1_ptr = write_to_memory(env, &mut wasm_store, param1.as_ref())?;
        let param2_ptr = write_to_memory(env, &mut wasm_store, param2.as_ref())?;
        let res_ptr: u32 = env
            .call_function1(&mut wasm_store, name, &[
                ctx_ptr.into(),
                param1_ptr.into(),
                param2_ptr.into(),
            ])?
            .try_into()
            .map_err(VmError::ReturnType)?;
        let data = read_then_wipe(env, &mut wasm_store, res_ptr)?;

        self.consume_gas()?;

        Ok(data)
    }
}
