//! This file contains helper functions for use in implementing database- or
//! math- related functionalities. Generally they involve manipulating raw bytes.

use std::borrow::Cow;

use crate::{StdError, StdResult};

// --------------------------------- database ----------------------------------

/// Combine a namespace a one or more keys into a full byte path.
///
/// The namespace and all keys other than the last one is prefixed with
/// their lengths (2 bytes big-endian). This helps us know where a key ends
/// and where the next key starts.
///
/// E.g. if keys are [key1, key2, key3], the resulting byte path is:
/// len(namespace) | namespace | len(key1) | key1 | len(key2) | key2 | key3
///
/// Panics if any key's length exceeds u16::MAX (because we need to put the
/// length into 2 bytes)
#[doc(hidden)]
pub fn nested_namespaces_with_key(
    maybe_namespace: Option<&[u8]>,
    prefixes: &[Cow<[u8]>],
    maybe_key: Option<&Cow<[u8]>>,
) -> Vec<u8> {
    let mut size = 0;
    if let Some(namespace) = maybe_namespace {
        size += namespace.len() + 2;
    }
    for prefix in prefixes {
        size += prefix.as_ref().len() + 2;
    }
    if let Some(key) = maybe_key {
        size += key.as_ref().len();
    }

    let mut out = Vec::with_capacity(size);
    if let Some(namespace) = maybe_namespace {
        out.extend_from_slice(&encode_length(namespace));
        out.extend_from_slice(namespace);
    }
    for prefix in prefixes {
        out.extend_from_slice(&encode_length(prefix));
        out.extend_from_slice(prefix.as_ref());
    }
    if let Some(key) = maybe_key {
        out.extend_from_slice(key.as_ref());
    }
    out
}

/// Given a byte slice, return two bytes in big endian representing its length.
/// Panic if the given byte slice is longer than the biggest length that can be
/// represented by a two bytes (i.e. 65535).
#[doc(hidden)]
pub fn encode_length<B>(bytes: B) -> [u8; 2]
where
    B: AsRef<[u8]>,
{
    let len = bytes.as_ref().len();
    if len > 0xffff {
        panic!(
            "Can't encode length because byte slice is too long: {} > {}",
            len,
            u16::MAX
        );
    }

    (bytes.as_ref().len() as u16).to_be_bytes()
}

// NOTE: this doesn't work if the bytes are entirely 255.
// in practice, the input bytes is a length-prefixed Map namespace. for the
// bytes to be entirely 255, the namespace must be u16::MAX = 65535 byte long
// (so that the two prefixed length bytes are [255, 255]).
// we can prevent this by introducing a max length for the namespace.
// assert this max length at compile time when the user calls Map::new.
#[doc(hidden)]
pub fn increment_last_byte(mut bytes: Vec<u8>) -> Vec<u8> {
    debug_assert!(
        bytes.iter().any(|x| *x != u8::MAX),
        "bytes are entirely 255"
    );
    for byte in bytes.iter_mut().rev() {
        if *byte == u8::MAX {
            *byte = 0;
        } else {
            *byte += 1;
            break;
        }
    }
    bytes
}

/// Given an extendable byte slice, append a zero byte to the end of it.
/// This is useful for dealing with iterator bounds.
#[doc(hidden)]
pub fn extend_one_byte(mut bytes: Vec<u8>) -> Vec<u8> {
    bytes.push(0);
    bytes
}

/// Given two byte slices, make a new byte vector that is the two slices joined
/// end to end.
#[doc(hidden)]
pub fn concat(namespace: &[u8], key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(namespace.len() + key.len());
    out.extend_from_slice(namespace);
    out.extend_from_slice(key);
    out
}

/// Given a byte slice that is prefixed with a namespace, trim the namespace,
/// return the suffix. The reverse of what `concat` function does.
///
/// Note that this function only checks whether the byte slice is actually
/// prefixed with the namespace in debug mode. In release we skip this for
/// performance. You must make sure only use this function we you're sure the
/// slice actually is prefixed with the namespace.
#[doc(hidden)]
pub fn trim(namespace: &[u8], key: &[u8]) -> Vec<u8> {
    debug_assert!(
        key.starts_with(namespace),
        "byte slice doesn't start with the given namespace"
    );
    key[namespace.len()..].to_vec()
}

// ------------------------------------ FFI ------------------------------------

/// Safely converts input of type T to u32.
/// Errors with a cosmwasm_vm::errors::VmError::ConversionErr if conversion cannot be done.
#[doc(hidden)]
pub fn to_u32<T>(input: T) -> StdResult<u32>
where
    T: TryInto<u32> + ToString + Copy,
{
    input
        .try_into()
        .map_err(|_| StdError::overflow_conversion::<T, u32>(input))
}

/// Given a compound key consisting of [k1, k2, ..., kN] (N > 1) that is encoded
/// in the following way:
///
/// ```plain
/// len(k1) | k1 | len(k2) | k2 ... len(k{N-1}) | k{N-1} | k{N}
/// ```
///
/// Strip the first key, returns two new byte slices:
///
/// 1. `k1`
/// 2. `len(k2) | k2 ... len(k{N-1}) | k{N-1} | k{N}`
#[doc(hidden)]
pub fn split_one_key(bytes: &[u8]) -> (&[u8], &[u8]) {
    // Note: This panics if bytes.len() < 2
    let (len_bytes, bytes) = bytes.split_at(2);
    // this unwrap can't fail since split at position 2
    let len = u16::from_be_bytes(len_bytes.try_into().unwrap());
    bytes.split_at(len as usize)
}

/// Encodes multiple sections of data into one vector.
///
/// Each section is suffixed by a section length encoded as big endian uint32.
/// Using suffixes instead of prefixes allows reading sections in reverse order,
/// such that the first element does not need to be re-allocated if the contract's
/// data structure supports truncation (such as a Rust vector).
///
/// The resulting data looks like this:
///
/// ```plain
/// section1 | section1_len | section2 | section2_len | section3 | section3_len | …
/// ```
#[doc(hidden)]
pub fn encode_sections(sections: &[&[u8]]) -> StdResult<Vec<u8>> {
    let mut out_len: usize = sections.iter().map(|section| section.len()).sum();
    out_len += 4 * sections.len();
    let mut out_data = Vec::with_capacity(out_len);
    for section in sections {
        let section_len = to_u32(section.len())?.to_be_bytes();
        out_data.extend_from_slice(section);
        out_data.extend_from_slice(&section_len);
    }
    debug_assert_eq!(out_data.len(), out_len);
    debug_assert_eq!(out_data.capacity(), out_len);
    Ok(out_data)
}

/// Decodes sections of data into multiple slices.
///
/// Each encoded section is suffixed by a section length, encoded as big endian uint32.
///
/// See also: `encode_section`.
#[doc(hidden)]
pub fn decode_sections(data: &[u8]) -> Vec<&[u8]> {
    let mut result: Vec<&[u8]> = vec![];
    let mut remaining_len = data.len();
    while remaining_len >= 4 {
        let tail_len = u32::from_be_bytes([
            data[remaining_len - 4],
            data[remaining_len - 3],
            data[remaining_len - 2],
            data[remaining_len - 1],
        ]) as usize;
        result.push(&data[remaining_len - 4 - tail_len..remaining_len - 4]);
        remaining_len -= 4 + tail_len;
    }
    debug_assert!(remaining_len == 0);
    result.reverse();
    result
}

// ----------------------------------- math ------------------------------------

#[doc(hidden)]
pub const fn grow_be_int<const INPUT_SIZE: usize, const OUTPUT_SIZE: usize>(
    input: [u8; INPUT_SIZE],
) -> [u8; OUTPUT_SIZE] {
    debug_assert!(INPUT_SIZE <= OUTPUT_SIZE);

    // check if sign bit is set
    let mut output = if input[0] & 0b10000000 != 0 {
        // negative number is filled up with 1s
        [0b11111111u8; OUTPUT_SIZE]
    } else {
        [0u8; OUTPUT_SIZE]
    };
    let mut i = 0;

    // copy input to the end of output
    // copy_from_slice is not const, so we have to do this manually
    while i < INPUT_SIZE {
        output[OUTPUT_SIZE - INPUT_SIZE + i] = input[i];
        i += 1;
    }

    output
}

#[doc(hidden)]
pub const fn grow_le_int<const INPUT_SIZE: usize, const OUTPUT_SIZE: usize>(
    input: [u8; INPUT_SIZE],
) -> [u8; OUTPUT_SIZE] {
    debug_assert!(INPUT_SIZE <= OUTPUT_SIZE);

    // check if sign bit is set
    let mut output = if input[INPUT_SIZE - 1] & 0b10000000 != 0 {
        // negative number is filled up with 1s
        [0b11111111u8; OUTPUT_SIZE]
    } else {
        [0u8; OUTPUT_SIZE]
    };
    let mut i = 0;

    // copy input to the beginning of output
    // copy_from_slice is not const, so we have to do this manually
    while i < INPUT_SIZE {
        output[i] = input[i];
        i += 1;
    }

    output
}

#[doc(hidden)]
pub const fn grow_be_uint<const INPUT_SIZE: usize, const OUTPUT_SIZE: usize>(
    input: [u8; INPUT_SIZE],
) -> [u8; OUTPUT_SIZE] {
    debug_assert!(INPUT_SIZE <= OUTPUT_SIZE);

    let mut output = [0u8; OUTPUT_SIZE];
    let mut i = 0;

    // copy input to the end of output
    // copy_from_slice is not const, so we have to do this manually
    while i < INPUT_SIZE {
        output[OUTPUT_SIZE - INPUT_SIZE + i] = input[i];
        i += 1;
    }

    output
}

#[doc(hidden)]
pub const fn grow_le_uint<const INPUT_SIZE: usize, const OUTPUT_SIZE: usize>(
    input: [u8; INPUT_SIZE],
) -> [u8; OUTPUT_SIZE] {
    debug_assert!(INPUT_SIZE <= OUTPUT_SIZE);

    let mut output = [0u8; OUTPUT_SIZE];
    let mut i = 0;

    // copy input to the beginning of output
    // copy_from_slice is not const, so we have to do this manually
    while i < INPUT_SIZE {
        output[i] = input[i];
        i += 1;
    }

    output
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use crate::{decode_sections, encode_sections};

    #[test]
    fn encode_decode() {
        let data: &[&[u8]] = &[b"this", b"is", b"composite", b"array"];
        let encoded = encode_sections(data).unwrap();
        assert_eq!(data, decode_sections(&encoded));
    }

    #[test]
    fn encode_section() {
        let data: &[&[u8]] = &[b"Jake", b"Larry", b"Rhaki"];

        let encode_section = crate::encode_sections(data).unwrap();
        let decode_section = crate::decode_sections(&encode_section);

        assert_eq!(data, decode_section);

        println!("data: {:?}", data);
        println!("encode_section: {:?}", encode_section);
        println!("decode_section: {:?}", decode_section);
    }
}
