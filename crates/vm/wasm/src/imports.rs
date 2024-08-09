use {
    crate::{read_from_memory, write_to_memory, Environment, Iterator, VmError, VmResult},
    grug_app::GAS_COSTS,
    grug_crypto::CryptoResult,
    grug_types::{
        decode_sections, from_json_slice, to_json_vec, Addr, QueryRequest, Record, Storage,
    },
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
    // This is the case for the `query`, `bank_query`, and `ibc_client_query`
    // calls. During these calls, the contract isn't allowed to call the imports
    // that mutates the state, namely: `db_write`, `db_remove`, and `db_remove_range`.
    if env.storage_readonly {
        return Err(VmError::ReadOnly);
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

    if env.storage_readonly {
        return Err(VmError::ReadOnly);
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

    if env.storage_readonly {
        return Err(VmError::ReadOnly);
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
    let req: QueryRequest = from_json_slice(req_bytes)?;

    // Note that although the query may fail, we don't unwrap the result here.
    // Instead, we serialize the `GenericResult` and pass it to the contract.
    // Let the contract decide how to handle the error.
    let res = env.querier.do_query_chain(req);
    let res_bytes = to_json_vec(&res)?;

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

    let res = grug_crypto::secp256k1_verify(&msg_hash, &sig, &pk);

    write_verify_result(res)
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

    let res = grug_crypto::secp256r1_verify(&msg_hash, &sig, &pk);

    write_verify_result(res)
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

    match grug_crypto::secp256k1_pubkey_recover(&msg_hash, &sig, recovery_id, compressed) {
        Ok(pk) => {
            let ptr = write_to_memory(env, &mut store, &pk)?;
            Ok(to_high_half(ptr))
        },
        Err(err) => Ok(to_low_half(err.into())),
    }

    // let res_bytes = to_borsh_vec(&res)?;
    // write_to_memory(env, &mut store, &res_bytes)
}

pub fn ed25519_verify(
    mut fe: FunctionEnvMut<Environment>,
    msg_hash_ptr: u32,
    sig_ptr: u32,
    pk_ptr: u32,
) -> VmResult<u32> {
    let (env, mut store) = fe.data_and_store_mut();

    let msg_hash = read_from_memory(env, &store, msg_hash_ptr)?;
    let sig = read_from_memory(env, &store, sig_ptr)?;
    let pk = read_from_memory(env, &store, pk_ptr)?;

    env.consume_external_gas(&mut store, GAS_COSTS.ed25519_verify, "ed25519_verify")?;

    let res = grug_crypto::ed25519_verify(&msg_hash, &sig, &pk);

    write_verify_result(res)
}

pub fn ed25519_batch_verify(
    mut fe: FunctionEnvMut<Environment>,
    msgs_hash_ptr: u32,
    sigs_ptr: u32,
    pks_ptr: u32,
) -> VmResult<u32> {
    let (env, mut store) = fe.data_and_store_mut();

    let msgs_hash = read_from_memory(env, &store, msgs_hash_ptr)?;
    let sigs = read_from_memory(env, &store, sigs_ptr)?;
    let pks = read_from_memory(env, &store, pks_ptr)?;

    let msgs_hash = decode_sections(&msgs_hash);
    let sigs = decode_sections(&sigs);
    let pks = decode_sections(&pks);

    env.consume_external_gas(
        &mut store,
        GAS_COSTS.ed25519_batch_verify.cost(msgs_hash.len()),
        "ed25519_batch_verify",
    )?;

    let res = grug_crypto::ed25519_batch_verify(&msgs_hash, &sigs, &pks);

    write_verify_result(res)
}

macro_rules! impl_hash_method {
    ($hasher:ident, $name:literal) => {
        pub fn $hasher(mut fe: FunctionEnvMut<Environment>, data_ptr: u32) -> VmResult<u32> {
            let (env, mut store) = fe.data_and_store_mut();

            let data = read_from_memory(env, &store, data_ptr)?;
            let hash = grug_crypto::$hasher(&data);

            env.consume_external_gas(&mut store, GAS_COSTS.$hasher.cost(data.len()), $name)?;

            write_to_memory(env, &mut store, &hash)
        }
    };
}

impl_hash_method!(sha2_256, "sha2_256");
impl_hash_method!(sha2_512, "sha2_512");
impl_hash_method!(sha2_512_truncated, "sha2_512_truncated");
impl_hash_method!(sha3_256, "sha3_256");
impl_hash_method!(sha3_512, "sha3_512");
impl_hash_method!(sha3_512_truncated, "sha3_512_truncated");
impl_hash_method!(keccak256, "keccak256");
impl_hash_method!(blake2s_256, "blake2s_256");
impl_hash_method!(blake2b_512, "blake2b_512");
impl_hash_method!(blake3, "blake3");

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

#[inline]
fn write_verify_result(result: CryptoResult<()>) -> VmResult<u32> {
    match result {
        Ok(()) => Ok(0),
        Err(err) => Ok(err.into()),
    }
}

#[inline]
fn to_high_half(data: u32) -> u64 {
    // See https://stackoverflow.com/a/58956419/2013738 to understand
    // why this is endianness agnostic.
    (data as u64) << 32
}

/// Returns the data copied to the 4 least significant bytes.
///
/// This is independent of endianness. But to get the idea, it would be
/// `0x00000000 || data` in big endian representation.
#[inline]
fn to_low_half(data: u32) -> u64 {
    data.into()
}
