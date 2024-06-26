use {
    borsh::{BorshDeserialize, BorshSerialize},
    grug_types::{from_borsh_slice, from_proto_slice, to_borsh_vec, to_proto_vec, StdResult},
    prost::Message,
};

/// A marker that designates encoding/decoding schemes.
pub trait Codec<T> {
    fn encode(data: &T) -> StdResult<Vec<u8>>;

    fn decode(data: &[u8]) -> StdResult<T>;
}

/// Represents the Borsh encoding scheme.
pub struct Borsh;

impl<T> Codec<T> for Borsh
where
    T: BorshSerialize + BorshDeserialize,
{
    fn encode(data: &T) -> StdResult<Vec<u8>> {
        to_borsh_vec(&data)
    }

    fn decode(data: &[u8]) -> StdResult<T> {
        from_borsh_slice(data)
    }
}

/// Represents the Protobuf encoding scheme.
pub struct Proto;

impl<T> Codec<T> for Proto
where
    T: Message + Default,
{
    fn encode(data: &T) -> StdResult<Vec<u8>> {
        Ok(to_proto_vec(data))
    }

    fn decode(data: &[u8]) -> StdResult<T> {
        from_proto_slice(data)
    }
}
