use {
    crate::{Environment, Iterator, VmError, VmResult, read_from_memory, write_to_memory},
    dango_app::GAS_COSTS,
    dango_primitives::{Addr, BorshDeExt, BorshSerExt, Query, Record, Storage},
    tracing::info,
    wasmer::FunctionEnvMut,
};

pub fn db_read(mut fe: FunctionEnvMut<Environment>, key_ptr: u32) -> VmResult<u32> {
    let (env, mut store) = fe.data_and_store_mut();

    let key = read_from_memory(env, &store, key_ptr)?;

    match env.storage.read(&key) {
        Some(value) => {
            env.consume_external_gas(
                &mut store,
                GAS_COSTS.db_read.cost(value.len()),
                "db_read/found",
            )?;
            write_to_memory(env, &mut store, &value)
        },
        None => {
            env.consume_external_gas(&mut store, GAS_COSTS.db_read.cost(0), "db_read/not_found")?;
            // If the record doesn't exist, return a zero pointer.
            Ok(0)
        },
    }
}

pub fn db_scan(
    mut fe: FunctionEnvMut<Environment>,
    min_ptr: u32,
    max_ptr: u32,
    order: i32,
) -> VmResult<i32> {
    let (env, mut store) = fe.data_and_store_mut();

    // Parse iteration parameters provided by the module and create iterator.
    let min = if min_ptr != 0 {
        Some(read_from_memory(env, &store, min_ptr)?)
    } else {
        None
    };
    let max = if max_ptr != 0 {
        Some(read_from_memory(env, &store, max_ptr)?)
    } else {
        None
    };
    let order = order.try_into()?;
    let iterator = Iterator::new(min, max, order);

    env.consume_external_gas(&mut store, GAS_COSTS.db_scan, "db_scan")?;

    Ok(env.add_iterator(iterator))
}

pub fn db_next(mut fe: FunctionEnvMut<Environment>, iterator_id: i32) -> VmResult<u32> {
    let (env, mut store) = fe.data_and_store_mut();

    match env.advance_iterator(iterator_id)? {
        Some((key, value)) => {
            env.consume_external_gas(
                &mut store,
                GAS_COSTS.db_next + GAS_COSTS.db_read.cost(key.len() + value.len()),
                "db_next/found",
            )?;

            write_to_memory(env, &mut store, &encode_record((key, value)))
        },
        None => {
            env.consume_external_gas(&mut store, GAS_COSTS.db_next, "db_next/not_found")?;

            Ok(0)
        },
    }
}

pub fn db_next_key(mut fe: FunctionEnvMut<Environment>, iterator_id: i32) -> VmResult<u32> {
    let (env, mut store) = fe.data_and_store_mut();

    match env.advance_iterator(iterator_id)? {
        Some((key, _)) => {
            env.consume_external_gas(
                &mut store,
                GAS_COSTS.db_next + GAS_COSTS.db_read.cost(key.len()),
                "db_next_key/found",
            )?;

            write_to_memory(env, &mut store, &key)
        },
        None => {
            env.consume_external_gas(&mut store, GAS_COSTS.db_next, "db_next_key/not_found")?;

            Ok(0)
        },
    }
}

pub fn db_next_value(mut fe: FunctionEnvMut<Environment>, iterator_id: i32) -> VmResult<u32> {
    let (env, mut store) = fe.data_and_store_mut();

    match env.advance_iterator(iterator_id)? {
        Some((_, value)) => {
            env.consume_external_gas(
                &mut store,
                GAS_COSTS.db_next + GAS_COSTS.db_read.cost(value.len()),
                "db_next_value/found",
            )?;

            write_to_memory(env, &mut store, &value)
        },
        None => {
            env.consume_external_gas(&mut store, GAS_COSTS.db_next, "db_next_value/not_found")?;

            Ok(0)
        },
    }
}

pub fn db_write(mut fe: FunctionEnvMut<Environment>, key_ptr: u32, value_ptr: u32) -> VmResult<()> {
    let (env, mut store) = fe.data_and_store_mut();

    // Make sure the storage isn't set to be read only.
    //
    // This is the case for the `query` and `bank_query` calls. During these
    // calls, the contract isn't allowed to call the imports that mutate the
    // state, namely: `db_write`, `db_remove`, and `db_remove_range`.
    if !env.state_mutable {
        return Err(VmError::immutable_state());
    }

    let key = read_from_memory(env, &store, key_ptr)?;
    let value = read_from_memory(env, &store, value_ptr)?;

    let gas_cost = GAS_COSTS
        .db_write
        .cost(env.storage.namespace().len() + key.len() + value.len());

    env.consume_external_gas(&mut store, gas_cost, "db_write")?;

    env.storage.write(&key, &value);

    // Delete all existing iterators. This is necessary if the storage is to be
    // mutated.
    //
    // Let's consider what happens if we fail to do this.
    //
    // Assume the storage has the following keys: `a`, `b`, `c`. An existing
    // iterator with ascending order is now at `b`. If we are to call `db_next`
    // now, it would return the `c` record.
    //
    // Now, we perfrom `db_write` to insert a new record with key `bb`. Now the
    // storage contains: `a`, `b`, `bb`, `c`.
    //
    // Now we call `db_next`. It will still return `b`. This is an incorrect
    // result: should be `bb` instead!
    //
    // Think about this the other way: having an active iterator is like holding
    // an immutable reference to the storage (though there isn't actually a ref
    // since we're working over the FFI). Performing a `db_write` requires a
    // mutable reference, which requires the immutable ref to be dropped first,
    // which involves deleting the iterator.
    env.clear_iterators();

    Ok(())
}

pub fn db_remove(mut fe: FunctionEnvMut<Environment>, key_ptr: u32) -> VmResult<()> {
    let (env, mut store) = fe.data_and_store_mut();

    if !env.state_mutable {
        return Err(VmError::immutable_state());
    }

    let key = read_from_memory(env, &store, key_ptr)?;

    env.storage.remove(&key);
    env.clear_iterators();
    env.consume_external_gas(&mut store, GAS_COSTS.db_remove, "storage_remove")
}

pub fn db_remove_range(
    mut fe: FunctionEnvMut<Environment>,
    min_ptr: u32,
    max_ptr: u32,
) -> VmResult<()> {
    let (env, mut store) = fe.data_and_store_mut();

    if !env.state_mutable {
        return Err(VmError::immutable_state());
    }

    let min = if min_ptr != 0 {
        Some(read_from_memory(env, &store, min_ptr)?)
    } else {
        None
    };
    let max = if max_ptr != 0 {
        Some(read_from_memory(env, &store, max_ptr)?)
    } else {
        None
    };

    env.storage.remove_range(min.as_deref(), max.as_deref());
    env.clear_iterators();
    env.consume_external_gas(&mut store, GAS_COSTS.db_remove, "storage_remove_range")
}

pub fn debug(mut fe: FunctionEnvMut<Environment>, addr_ptr: u32, msg_ptr: u32) -> VmResult<()> {
    let (env, store) = fe.data_and_store_mut();

    let addr_bytes = read_from_memory(env, &store, addr_ptr)?;
    let addr = Addr::try_from(addr_bytes)?;
    let msg_bytes = read_from_memory(env, &store, msg_ptr)?;
    let msg = String::from_utf8(msg_bytes)?;

    info!(
        contract = addr.to_string(),
        msg, "Contract emitted debug message"
    );

    Ok(())
}

pub fn query_chain(mut fe: FunctionEnvMut<Environment>, req_ptr: u32) -> VmResult<u32> {
    let (env, mut store) = fe.data_and_store_mut();

    let req_bytes = read_from_memory(env, &store, req_ptr)?;
    let req: Query = req_bytes.deserialize_borsh()?;

    // Note that although the query may fail, we don't unwrap the result here.
    // Instead, we serialize the `GenericResult` and pass it to the contract.
    // Let the contract decide how to handle the error.
    let res = env.querier.do_query_chain(req, env.query_depth + 1); // important: increase query depth
    let res_bytes = res.to_borsh_vec()?;

    write_to_memory(env, &mut store, &res_bytes)
}

pub fn secp256k1_verify(
    mut fe: FunctionEnvMut<Environment>,
    msg_hash_ptr: u32,
    sig_ptr: u32,
    pk_ptr: u32,
) -> VmResult<u32> {
    let (env, mut store) = fe.data_and_store_mut();

    let msg_hash = read_from_memory(env, &store, msg_hash_ptr)?;
    let sig = read_from_memory(env, &store, sig_ptr)?;
    let pk = read_from_memory(env, &store, pk_ptr)?;

    env.consume_external_gas(&mut store, GAS_COSTS.secp256k1_verify, "secp256k1_verify")?;

    match dango_crypto::secp256k1_verify(&msg_hash, &sig, &pk) {
        Ok(()) => Ok(0),
        Err(err) => Ok(err.into_error_code()),
    }
}

pub fn secp256r1_verify(
    mut fe: FunctionEnvMut<Environment>,
    msg_hash_ptr: u32,
    sig_ptr: u32,
    pk_ptr: u32,
) -> VmResult<u32> {
    let (env, mut store) = fe.data_and_store_mut();

    let msg_hash = read_from_memory(env, &store, msg_hash_ptr)?;
    let sig = read_from_memory(env, &store, sig_ptr)?;
    let pk = read_from_memory(env, &store, pk_ptr)?;

    env.consume_external_gas(&mut store, GAS_COSTS.secp256k1_verify, "secp256r1_verify")?;

    match dango_crypto::secp256r1_verify(&msg_hash, &sig, &pk) {
        Ok(()) => Ok(0),
        Err(err) => Ok(err.into_error_code()),
    }
}

pub fn secp256k1_pubkey_recover(
    mut fe: FunctionEnvMut<Environment>,
    msg_hash_ptr: u32,
    sig_ptr: u32,
    recovery_id: u8,
    compressed: u8,
) -> VmResult<u64> {
    let (env, mut store) = fe.data_and_store_mut();

    let msg_hash = read_from_memory(env, &store, msg_hash_ptr)?;
    let sig = read_from_memory(env, &store, sig_ptr)?;

    let compressed = match compressed {
        0 => false,
        1 => true,
        _ => return Ok(0),
    };

    env.consume_external_gas(
        &mut store,
        GAS_COSTS.secp256k1_pubkey_recover,
        "secp256k1_pubkey_recover",
    )?;

    // The return value for this function is an `u64`, of which:
    // - The first 4 bytes are the error code.
    //   If recovery is successful, these should be zero.
    // - the second 4 bytes are the memory address of the recovered pk.
    //   if recovery is unsuccessful, these should be zero.
    let (error_code, ptr) =
        match dango_crypto::secp256k1_pubkey_recover(&msg_hash, &sig, recovery_id, compressed) {
            Ok(pk) => (0, write_to_memory(env, &mut store, &pk)?),
            Err(err) => (err.into_error_code(), 0),
        };

    Ok(((error_code as u64) << 32) | (ptr as u64))
}

macro_rules! impl_hash_method {
    ($hasher:ident, $name:literal) => {
        pub fn $hasher(mut fe: FunctionEnvMut<Environment>, data_ptr: u32) -> VmResult<u32> {
            let (env, mut store) = fe.data_and_store_mut();

            let data = read_from_memory(env, &store, data_ptr)?;
            let hash = dango_crypto::$hasher(&data);

            env.consume_external_gas(&mut store, GAS_COSTS.$hasher.cost(data.len()), $name)?;

            write_to_memory(env, &mut store, &hash)
        }
    };
}

impl_hash_method!(sha2_256, "sha2_256");
impl_hash_method!(keccak256, "keccak256");

/// Pack a KV pair into a single byte array in the following format:
///
/// ```plain
/// key | value | len(key)
/// ```
///
/// where `len()` is two bytes (u16 big endian).
#[inline]
fn encode_record((mut k, v): Record) -> Vec<u8> {
    let key_len = k.len();
    k.extend(v);
    k.extend_from_slice(&(key_len as u16).to_be_bytes());
    k
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        crate::{
            Environment, GAS_PER_OPERATION, VmResult, WasmVm, db_read, db_remove, db_remove_range,
            db_scan, db_write, debug, read_from_memory, write_to_memory,
        },
        dango_app::{APP_CONFIG, GAS_COSTS, GasTracker, QuerierProviderImpl, StorageProvider},
        dango_primitives::{
            Addr, BlockInfo, BorshDeExt, BorshSerExt, GenericResult, Hash256, MockStorage, Order,
            Query, QueryResponse, ResultExt, Shared, Storage, Timestamp, json,
        },
        std::{fmt::Debug, sync::Arc},
        test_case::test_case,
        wasmer::{
            Engine, Function, FunctionEnv, FunctionEnvMut, Instance, Module, Store, imports,
            sys::{CompilerConfig, Singlepass},
        },
        wasmer_middlewares::{Metering, metering::set_remaining_points},
    };

    const TESTER_CONTRACT: &[u8] = include_bytes!("../testdata/dango_tester.wasm");

    const MOCK_BLOCK: BlockInfo = BlockInfo {
        height: 1,
        timestamp: Timestamp::from_nanos(100),
        hash: Hash256::ZERO,
    };

    const NAMESPACE_CONTRACT: &[u8] = b"contract";

    /// Helper struct to hold the necessary data for testing.
    struct Suite {
        fe: FunctionEnv<Environment>,
        storage: Box<dyn Storage>,
        storage_provider: StorageProvider,
        store: Store,
        _instance: Box<Instance>,
    }

    impl Suite {
        fn write(&mut self, data: &[u8]) -> VmResult<u32> {
            let mut fe_mut = self.fe_mut();
            let (env, mut store) = fe_mut.data_and_store_mut();
            write_to_memory(env, &mut store, data)
        }

        fn fe_mut(&mut self) -> FunctionEnvMut<'_, Environment> {
            self.fe.clone().into_mut(&mut self.store)
        }

        fn read(&mut self, ptr: u32) -> VmResult<Vec<u8>> {
            let mut fe_mut = self.fe_mut();
            let (env, store) = fe_mut.data_and_store_mut();
            read_from_memory(env, &store, ptr)
        }

        fn env_mut(&mut self) -> &mut Environment {
            self.fe.as_mut(&mut self.store)
        }
    }

    fn setup_test() -> Suite {
        let gas_checkpoint = u64::MAX;
        let gas_tracker = GasTracker::new_limitless();
        // Compile the contract; create Wasmer store and instance.
        let (mut store, instance) = {
            let mut compiler = Singlepass::new();
            compiler.push_middleware(Arc::new(Metering::new(0, |_| GAS_PER_OPERATION)));

            let engine = Engine::from(compiler);
            let module = Module::new(&engine, TESTER_CONTRACT).unwrap();

            let mut store = Store::new(engine);

            // Import functions are not used but need to be defined.
            let import_obj = imports! {
                "env" => {
                    "db_read"                  => Function::new_typed(&mut store, |_: u32|                       -> u32 { 0 }),
                    "db_scan"                  => Function::new_typed(&mut store, |_: u32, _: u32, _: i32|       -> u32 { 0 }),
                    "db_next"                  => Function::new_typed(&mut store, |_: u32|                       -> u32 { 0 }),
                    "db_next_key"              => Function::new_typed(&mut store, |_: u32|                       -> u32 { 0 }),
                    "db_next_value"            => Function::new_typed(&mut store, |_: u32|                       -> u32 { 0 }),
                    "db_write"                 => Function::new_typed(&mut store, |_: u32, _: u32|                      {   }),
                    "db_remove"                => Function::new_typed(&mut store, |_: u32|                              {   }),
                    "db_remove_range"          => Function::new_typed(&mut store, |_: u32, _: u32|                      {   }),
                    "secp256k1_verify"         => Function::new_typed(&mut store, |_: u32, _: u32, _: u32|       -> u32 { 0 }),
                    "secp256r1_verify"         => Function::new_typed(&mut store, |_: u32, _: u32, _: u32|       -> u32 { 0 }),
                    "secp256k1_pubkey_recover" => Function::new_typed(&mut store, |_: u32, _: u32, _: u8, _: u8| -> u64 { 0 }),
                    "sha2_256"                 => Function::new_typed(&mut store, |_: u32|                       -> u32 { 0 }),
                    "keccak256"                => Function::new_typed(&mut store, |_: u32|                       -> u32 { 0 }),
                    "debug"                    => Function::new_typed(&mut store, |_: u32, _: u32|                      {   }),
                    "query_chain"              => Function::new_typed(&mut store, |_: u32,|                      -> u32 { 0 }),
                },
            };

            let instance = Instance::new(&mut store, &module, &import_obj).unwrap();
            let instance = Box::new(instance);

            set_remaining_points(&mut store, &instance, gas_checkpoint);

            (store, instance)
        };

        // Create the function environment.
        //
        // For production (in `WasmVm::build_instance`) this needs to be done
        // before creating the instance, but here for testing purpose, we don't
        // need any import function, so this can be done later, which is simpler.
        let (fe, storage, storage_provider) = {
            let storage = Shared::new(MockStorage::new());
            let storage_provider =
                StorageProvider::new(Box::new(storage.clone()), &[NAMESPACE_CONTRACT]);

            let querier_provider = QuerierProviderImpl::new_boxed(
                WasmVm::new(0),
                Box::new(storage.clone()),
                gas_tracker.clone(),
                MOCK_BLOCK,
            );

            let env = Environment::new(
                storage_provider.clone(),
                true,
                querier_provider,
                10,
                gas_tracker,
                gas_checkpoint,
            );

            let fe = FunctionEnv::new(&mut store, env);

            let env = fe.as_mut(&mut store);

            env.set_wasmer_memory(&instance).unwrap();
            env.set_wasmer_instance(&instance).unwrap();
            (fe, storage, storage_provider)
        };

        Suite {
            store,
            _instance: instance,
            fe,
            storage: Box::new(storage),
            storage_provider,
        }
    }

    // -------------------------------- db_read --------------------------------

    #[test]
    fn db_read_works() {
        let mut suite = setup_test();

        let (k, v) = (b"key", b"value");

        suite.storage_provider.write(k, v);

        let ptr_key = suite.write(k).unwrap();

        let ptr_result = db_read(suite.fe_mut(), ptr_key).unwrap();

        let result = suite.read(ptr_result).unwrap();

        assert_eq!(result, v);
    }

    // -------------------------------- db_scan --------------------------------

    #[test_case(
        Some(b"key1"), Some(b"key3"), Order::Ascending,
        &[
            Some((b"key1", b"value1")),
            Some((b"key2", b"value2")),
            None,
        ];
        "ascending"
    )]
    #[test_case(
        Some(b"key1"), Some(b"key3"), Order::Descending,
        &[
            Some((b"key2", b"value2")),
            Some((b"key1", b"value1")),
            None,
        ];
        "descending"
    )]
    #[test_case(
        None, None, Order::Ascending,
        &[
            Some((b"key1", b"value1")),
            Some((b"key2", b"value2")),
            Some((b"key3", b"value3")),
            None,
        ];
        "no bound"
    )]
    fn db_scan_works(
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
        expected: &[Option<(&[u8], &[u8])>],
    ) {
        let mut suite = setup_test();

        suite.storage_provider.write(b"key1", b"value1");
        suite.storage_provider.write(b"key2", b"value2");
        suite.storage_provider.write(b"key3", b"value3");

        let min_ptr = if let Some(min) = min {
            suite.write(min).unwrap()
        } else {
            0
        };

        let max_ptr = if let Some(max) = max {
            suite.write(max).unwrap()
        } else {
            0
        };

        let iterator_id = db_scan(suite.fe_mut(), min_ptr, max_ptr, order as i32).unwrap();

        let env = suite.env_mut();

        for expected in expected {
            let maybe = env.advance_iterator(iterator_id).unwrap();
            assert_eq!(maybe, expected.map(|(k, v)| (k.to_vec(), v.to_vec())));
        }
    }

    // ----------------- db_next / db_next_key / db_next_value -----------------

    #[test_case(
        crate::db_next,
        // Copied from ffi
        |mut data| {
            let (Some(byte1), Some(byte2)) = (data.pop(), data.pop()) else {
                panic!("[ExternalIterator]: can't read length suffix");
            };

            // Note the order here between the two bytes
            let key_len = u16::from_be_bytes([byte2, byte1]);
            let value = data.split_off(key_len.into());

            (data, value)
        },
        &[
            Some((b"key1".to_vec(), b"value1".to_vec())),
            Some((b"key2".to_vec(), b"value2".to_vec())),
            None,
        ];
        "db_next"
    )]
    #[test_case(
        crate::db_next_key,
        |data| data,
        &[
            Some(b"key1".to_vec()),
            Some(b"key2".to_vec()),
            None,
        ];
        "db_next_key"
    )]
    #[test_case(
        crate::db_next_value,
        |data| data,
        &[
            Some(b"value1".to_vec()),
            Some(b"value2".to_vec()),
            None,
        ];
        "db_next_value"
    )]
    fn db_nexts_works<F, P, R>(next: F, parse: P, expected: &[Option<R>])
    where
        F: Fn(FunctionEnvMut<Environment>, i32) -> VmResult<u32>,
        P: Fn(Vec<u8>) -> R,
        R: PartialEq + Debug,
    {
        let mut suite = setup_test();

        suite.storage_provider.write(b"key1", b"value1");
        suite.storage_provider.write(b"key2", b"value2");
        suite.storage_provider.write(b"key3", b"value3");

        // For create iterator use db_scan function.
        // It also possible to create iterator directly on env
        // but why not more tests?

        let min_ptr = suite.write(b"key1").unwrap();
        let max_ptr = suite.write(b"key3").unwrap();

        let iterator_id =
            db_scan(suite.fe_mut(), min_ptr, max_ptr, Order::Ascending as i32).unwrap();

        for expected in expected {
            let next_ptr = next(suite.fe_mut(), iterator_id).unwrap();

            if next_ptr == 0 {
                assert_eq!(expected, &None);
                break;
            } else {
                let next_ptr = suite.read(next_ptr).unwrap();

                let next = parse(next_ptr);

                assert_eq!(expected, &Some(next));
            }
        }
    }

    // ------------------------------- db_write --------------------------------

    #[test]
    fn db_write_works() {
        let mut suite = setup_test();

        let (k, v) = (b"key", b"value");

        let ptr_key = suite.write(k).unwrap();
        let ptr_value = suite.write(v).unwrap();

        let gas_pre = suite.env_mut().gas_tracker.used();

        db_write(suite.fe_mut(), ptr_key, ptr_value).unwrap();

        let result = suite.storage_provider.read(k).unwrap();

        assert_eq!(result, v);

        // Check gas consumption

        let gas_consumed = suite.env_mut().gas_tracker.used() - gas_pre;

        let cost = GAS_COSTS
            .db_write
            .cost(NAMESPACE_CONTRACT.len() + k.len() + v.len());

        assert_eq!(gas_consumed, cost);
    }

    // ------------------------------- db_remove -------------------------------

    #[test]
    fn db_remove_works() {
        let mut suite = setup_test();

        let (k, v) = (b"key", b"value");

        suite.storage_provider.write(k, v);

        let ptr_key = suite.write(k).unwrap();

        let gas_pre = suite.env_mut().gas_tracker.used();

        db_remove(suite.fe_mut(), ptr_key).unwrap();

        let result = suite.storage_provider.read(k);

        assert_eq!(result, None);

        // Check gas consumption

        let gas_consumed = suite.env_mut().gas_tracker.used() - gas_pre;

        assert_eq!(gas_consumed, GAS_COSTS.db_remove);
    }

    // ---------------------------- db_remove_range ----------------------------

    #[test_case(
        Some(b"key1"), None,
        &[
            (b"key2", None),
            (b"key3", None),
        ];
        "min"
    )]
    #[test_case(
        None, Some(b"key3"),
        &[
            (b"key1", None),
            (b"key2", None),
        ];
        "max"
    )]
    fn db_remove_range_works(
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        expected: &[(&[u8], Option<&[u8]>)],
    ) {
        let mut suite = setup_test();

        let (k1, v1) = (b"key1", b"value1");
        let (k2, v2) = (b"key2", b"value2");
        let (k3, v3) = (b"key3", b"value3");

        suite.storage_provider.write(k1, v1);
        suite.storage_provider.write(k2, v2);
        suite.storage_provider.write(k3, v3);

        let min_ptr = if let Some(min) = min {
            suite.write(min).unwrap()
        } else {
            0
        };

        let max_ptr = if let Some(max) = max {
            suite.write(max).unwrap()
        } else {
            0
        };

        let gas_pre = suite.env_mut().gas_tracker.used();

        db_remove_range(suite.fe_mut(), min_ptr, max_ptr).unwrap();

        for (k, expected) in expected {
            let result = suite.storage_provider.read(k);

            assert_eq!(result, expected.map(|v| v.to_vec()));
        }

        // Check gas consumption

        let gas_consumed = suite.env_mut().gas_tracker.used() - gas_pre;

        assert_eq!(gas_consumed, GAS_COSTS.db_remove);
    }

    // -------------------------------- debug ----------------------------------

    #[test]
    fn debug_works() {
        let mut suite = setup_test();

        let addr = Addr::mock(1);
        let msg = b"msg";

        let ptr_addr = suite.write(addr.as_ref()).unwrap();
        let ptr_msg = suite.write(msg).unwrap();

        // Just call the function
        // It could be possible to check logs but it could create conflicts
        // with other tests
        debug(suite.fe_mut(), ptr_addr, ptr_msg).unwrap();
    }

    // ----------------------------- query_chain -------------------------------

    #[test]
    fn query_chain_works() {
        let mut suite = setup_test();

        // Use AppConfig query as example
        // We don't want to test the query itself, just the query_chain function
        let request = Query::app_config();

        APP_CONFIG
            .save(&mut suite.storage, &json!({ "foo": "bar" }))
            .unwrap();

        let ptr_key = suite.write(&request.to_borsh_vec().unwrap()).unwrap();
        let ptr_result = crate::query_chain(suite.fe_mut(), ptr_key).unwrap();

        suite
            .read(ptr_result)
            .unwrap()
            .deserialize_borsh::<GenericResult<QueryResponse>>()
            .unwrap()
            .should_succeed_and_equal(QueryResponse::AppConfig(json!({ "foo": "bar" })));
    }

    // ----------------------------- Crypto Verify -----------------------------

    const MSG: &[u8] = b"msg";

    const WRONG_MSG: &[u8] = b"wrong_msg";

    struct VerifyTest {
        pub msg_hash: Vec<u8>,
        pub sig: Vec<u8>,
        pub pk: Vec<u8>,
        pub wrong_msg: Vec<u8>,
    }

    fn generate_secp256r1_verify_request() -> VerifyTest {
        use p256::{
            ecdsa::{Signature, SigningKey, VerifyingKey, signature::hazmat::PrehashSigner},
            elliptic_curve::Generate,
        };

        let sk = SigningKey::generate();
        let vk = VerifyingKey::from(&sk);
        let msg_hash = dango_crypto::sha2_256(MSG);
        let sig: Signature = sk.sign_prehash(&msg_hash).unwrap();

        VerifyTest {
            pk: vk.to_sec1_bytes().to_vec(),
            sig: sig.to_bytes().to_vec(),
            msg_hash: msg_hash.into(),
            wrong_msg: dango_crypto::sha2_256(WRONG_MSG).to_vec(),
        }
    }

    fn generate_secp256k1_verify_request() -> VerifyTest {
        use k256::{
            ecdsa::{Signature, SigningKey, VerifyingKey, signature::hazmat::PrehashSigner},
            elliptic_curve::Generate,
        };

        let sk = SigningKey::generate();
        let vk = VerifyingKey::from(&sk);
        let msg_hash = dango_crypto::sha2_256(MSG);
        let sig: Signature = sk.sign_prehash(&msg_hash).unwrap();

        VerifyTest {
            pk: vk.to_sec1_bytes().to_vec(),
            sig: sig.to_bytes().to_vec(),
            msg_hash: msg_hash.into(),
            wrong_msg: dango_crypto::sha2_256(WRONG_MSG).to_vec(),
        }
    }

    #[test_case(
        crate::secp256k1_verify,
        generate_secp256k1_verify_request;
        "secp256k1_verify"
    )]
    #[test_case(
        crate::secp256r1_verify,
        generate_secp256r1_verify_request;
        "secp256kr_verify"
    )]
    fn verify_works<V, G>(verify: V, generate: G)
    where
        V: Fn(FunctionEnvMut<Environment>, u32, u32, u32) -> VmResult<u32>,
        G: Fn() -> VerifyTest,
    {
        let mut suite = setup_test();

        let test_data = generate();

        // Ok
        {
            let ptr_msg = suite.write(&test_data.msg_hash).unwrap();
            let ptr_sig = suite.write(&test_data.sig).unwrap();
            let ptr_pk = suite.write(&test_data.pk).unwrap();

            let result = verify(suite.fe_mut(), ptr_msg, ptr_sig, ptr_pk).unwrap();

            assert_eq!(result, 0);
        }

        // Fail
        {
            let ptr_msg = suite.write(&test_data.wrong_msg).unwrap();
            let ptr_sig = suite.write(&test_data.sig).unwrap();
            let ptr_pk = suite.write(&test_data.pk).unwrap();

            let result = verify(suite.fe_mut(), ptr_msg, ptr_sig, ptr_pk).unwrap();

            assert_eq!(result, 3);
        }
    }

    // ---------------------- secp256k1_pubkey_recover -------------------------

    #[test]
    fn secp256k1_pubkey_recover_works() {
        let mut suite = setup_test();

        let (pk, sig, msg_hash, recovery_id, compressed) = {
            use k256::{
                ecdsa::{SigningKey, VerifyingKey},
                elliptic_curve::Generate,
            };

            let sk = SigningKey::generate();
            let vk = VerifyingKey::from(&sk);
            let msg_hash = dango_crypto::sha2_256(MSG);
            let (sig, recovery_id) = sk.sign_prehash_recoverable(&msg_hash);

            (
                vk.to_sec1_bytes().to_vec(),
                sig.to_vec(),
                msg_hash.to_vec(),
                recovery_id.to_byte(),
                1,
            )
        };

        // OK
        {
            let ptr_msg = suite.write(&msg_hash).unwrap();
            let ptr_sig = suite.write(&sig).unwrap();

            let result = crate::secp256k1_pubkey_recover(
                suite.fe_mut(),
                ptr_msg,
                ptr_sig,
                recovery_id,
                compressed,
            )
            .unwrap();

            let error_code = (result >> 32) as u32;
            let pk_ptr = result as u32;

            assert_eq!(error_code, 0);
            assert_eq!(suite.read(pk_ptr).unwrap(), pk);
        }

        // Fail
        {
            let ptr_msg = suite.write(WRONG_MSG).unwrap();
            let ptr_sig = suite.write(&sig).unwrap();

            let result = crate::secp256k1_pubkey_recover(
                suite.fe_mut(),
                ptr_msg,
                ptr_sig,
                recovery_id,
                compressed,
            )
            .unwrap();

            let error_code = (result >> 32) as u32;
            let pk_ptr = result as u32;

            assert_eq!(error_code, 1);
            assert_eq!(pk_ptr, 0);
        }
    }

    // --------------------------------- Hash ----------------------------------

    #[test_case(
        crate::sha2_256,
        dango_crypto::sha2_256;
        "sha2_256"
    )]
    #[test_case(
        crate::keccak256,
        dango_crypto::keccak256;
        "keccak256"
    )]
    fn hash_works<H, G, const S: usize>(hash: H, generate: G)
    where
        H: Fn(FunctionEnvMut<Environment>, u32) -> VmResult<u32>,
        G: Fn(&[u8]) -> [u8; S],
    {
        let mut suite = setup_test();

        let ptr_data = suite.write(MSG).unwrap();

        let result = hash(suite.fe_mut(), ptr_data).unwrap();

        let result = suite.read(result).unwrap();

        assert_eq!(result, generate(MSG));
    }
}
