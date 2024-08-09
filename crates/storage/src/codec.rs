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

#[cfg(test)]
mod tests {
    use {
        super::Codec,
        crate::{Borsh, Proto},
        borsh::{BorshDeserialize, BorshSerialize},
        std::fmt::Debug,
        test_case::test_case,
    };

    #[derive(BorshSerialize, BorshDeserialize, prost::Message, PartialEq)]
    struct Test {
        #[prost(uint32, tag = "1")]
        foo: u32,
        #[prost(string, tag = "2")]
        bar: String,
    }

    impl Test {
        fn mock() -> Self {
            Self {
                foo: 3,
                bar: "bar".to_string(),
            }
        }
    }

    #[test_case(Test::mock(), Borsh; "borsh")]
    #[test_case(Test::mock(), Proto; "proto")]
    fn codec<T, C>(data: T, _codec: C)
    where
        T: PartialEq + Debug,
        C: Codec<T>,
    {
        let encoded = C::encode(&data).unwrap();
        let decoded = C::decode(&encoded).unwrap();
        assert_eq!(data, decoded);
    }
}
