#[cfg(feature = "erc55")]
use {
    crate::StdResult,
    sha3::{Digest, Keccak256},
};
use {
    crate::{forward_ref_partial_eq, hash160, hash256, Hash, Hash160, Hash256, StdError},
    borsh::{BorshDeserialize, BorshSerialize},
    core::str,
    serde::{de, ser},
    std::{
        fmt,
        ops::{Deref, DerefMut},
        str::FromStr,
    },
};

/// An account address.
///
/// In Grug, addresses are of 20-byte length, in Hex encoding and the `0x` prefix.
/// In comparison, in the "vanilla" CosmWasm, addresses are either 20- or 32-byte,
/// in Bech32 encoding. The last 6 ASCII characters are the checksum.
///
/// In CosmWasm, when addresses are deserialized from JSON, no validation is
/// performed. An attacker can put a string that is not a valid address in a
/// message, and this would be deserialized into an `cosmwasm_std::Addr` without
/// error. Therefore, in CosmWasm, it is recommended to deserialize addresses
/// into `String`s first, then call `deps.api.addr_validate` to validate them.
/// This can be sometimes very cumbersome. It may be necessary to define two
/// versions of the same type, one "unchecked" version with `String`s, one
/// "checked" version with `Addr`s.
///
/// In Grug, addresses are validated during deserialization. If deserialization
/// doesn't throw an error, you can be sure the address is valid. Therefore it
/// is safe to use `Addr`s in JSON messages.
#[derive(BorshSerialize, BorshDeserialize, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Addr(pub(crate) Hash160);

forward_ref_partial_eq!(Addr, Addr);

impl Addr {
    /// Addresses have the 0x prefix.
    pub const PREFIX: &'static str = "0x";

    /// Create a new address from a 32-byte byte slice.
    pub const fn from_array(slice: [u8; Hash160::LENGTH]) -> Self {
        Self(Hash160::from_array(slice))
    }

    /// Compute a contract address as:
    ///
    /// ```plain
    /// address := ripemd160(sha256(deployer_addr | code_hash | salt))
    /// ```
    ///
    /// where `|` means byte concatenation.
    ///
    /// The double hash the same as used by Bitcoin, for [preventing length
    /// extension attacks](https://bitcoin.stackexchange.com/questions/8443/where-is-double-hashing-performed-in-bitcoin).
    pub fn compute(deployer: &Addr, code_hash: &Hash256, salt: &[u8]) -> Self {
        let mut preimage = Vec::with_capacity(Hash160::LENGTH + Hash256::LENGTH + salt.len());
        preimage.extend_from_slice(deployer.as_ref());
        preimage.extend_from_slice(code_hash.as_ref());
        preimage.extend_from_slice(salt);
        Self(hash160(hash256(preimage)))
    }

    /// Generate a mock address from use in testing.
    pub const fn mock(index: u8) -> Self {
        let mut bytes = [0u8; Hash160::LENGTH];
        bytes[Hash160::LENGTH - 1] = index;
        Self(Hash160::from_array(bytes))
    }
}

#[cfg(feature = "erc55")]
impl Addr {
    /// Convert the address to checksumed hex string according to
    /// [ERC-55](https://eips.ethereum.org/EIPS/eip-55#implementation).
    ///
    /// Adapted from
    /// [Alloy](https://github.com/alloy-rs/core/blob/v0.7.7/crates/primitives/src/bits/address.rs#L294-L320).
    pub fn to_erc55_string(&self) -> String {
        let mut buf = vec![0; 42];
        buf[0] = b'0';
        buf[1] = b'x';

        // This encodes hex in lowercase.
        hex::encode_to_slice(&self.0, &mut buf[2..]).unwrap();

        let mut hasher = Keccak256::new();
        // Note we're hashing the UTF-8 hex string, not the raw bytes.
        hasher.update(&buf[2..]);
        let hash = hasher.finalize();

        let mut hash_hex = [0; 64];
        hex::encode_to_slice(hash, &mut hash_hex).unwrap();

        for i in 0..40 {
            buf[2 + i] ^=
                0b0010_0000 * (buf[2 + i].is_ascii_lowercase() & (hash_hex[i] >= b'8')) as u8;
        }

        unsafe { String::from_utf8_unchecked(buf) }
    }

    /// Validate an ERC-55 checksumed address string.
    pub fn from_erc55_string(s: &str) -> StdResult<Self> {
        let addr = Addr::from_str(s)?;

        if s != addr.to_erc55_string() {
            return Err(StdError::deserialize::<Self, _>("invalid ERC-55 checksum"));
        }

        Ok(addr)
    }
}

impl AsRef<[u8]> for Addr {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsMut<[u8]> for Addr {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl Deref for Addr {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Addr {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl TryFrom<Vec<u8>> for Addr {
    type Error = StdError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        let Ok(bytes) = bytes.try_into() else {
            return Err(StdError::deserialize::<Self, _>(
                "address is not of the correct length",
            ));
        };

        Ok(Self(Hash(bytes)))
    }
}

impl TryFrom<&[u8]> for Addr {
    type Error = StdError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let Ok(bytes) = bytes.try_into() else {
            return Err(StdError::deserialize::<Self, _>(
                "address is not of the correct length",
            ));
        };

        Ok(Self(Hash(bytes)))
    }
}

impl FromStr for Addr {
    type Err = StdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // The address must have the `0x` prefix.
        let hex_str = s
            .strip_prefix(Self::PREFIX)
            .ok_or_else(|| StdError::deserialize::<Self, _>("incorrect address prefix"))?;

        // Decode the hex string
        let bytes = hex::decode(hex_str)?;

        // Make sure the byte slice of the correct length.
        let hash = Hash160::from_array(bytes.as_slice().try_into().map_err(|_| {
            StdError::deserialize::<Self, _>(format!(
                "invalid bytes len: expected: {} got: {}",
                Hash160::LENGTH,
                bytes.len()
            ))
        })?);

        Ok(Self(hash))
    }
}

impl From<Addr> for String {
    fn from(addr: Addr) -> Self {
        addr.to_string()
    }
}

// Convert the raw bytes to checksumed hex string according to ERC-55:
// https://eips.ethereum.org/EIPS/eip-55#implementation
//
// Adapted from alloy-rs:
// https://github.com/alloy-rs/core/blob/v0.7.7/crates/primitives/src/bits/address.rs#L294-L320
impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::PREFIX, hex::encode(&self.0))
    }
}

impl fmt::Debug for Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Addr({})", self)
    }
}

impl ser::Serialize for Addr {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> de::Deserialize<'de> for Addr {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(AddrVisitor)
    }
}

struct AddrVisitor;

impl<'de> de::Visitor<'de> for AddrVisitor {
    type Value = Addr;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string representing an address conforming to ERC-55")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Addr::from_str(v).map_err(E::custom)
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    #[cfg(feature = "erc55")]
    use test_case::test_case;

    use {
        crate::{from_json_value, to_json_value, Addr},
        hex_literal::hex,
        serde_json::json,
        std::str::FromStr,
    };

    // the same as the mock hash from the Hash unit tests, except cropped to 20
    // bytes and with the `0x` prefix.
    const MOCK_STR: &str = "0x299663875422cc5a4574816e6165824d0c5bfdba";
    const INVALID_MOCK_STR: &str = "0x299663875422cc5a4574816e6165824d0c5bfdba01";
    const MOCK_ADDR: Addr = Addr::from_array(hex!("299663875422cc5a4574816e6165824d0c5bfdba"));

    #[test]
    fn serializing() {
        assert_eq!(MOCK_STR, MOCK_ADDR.to_string());
        assert_eq!(json!(MOCK_STR), to_json_value(&MOCK_ADDR).unwrap());
    }

    #[test]
    fn deserializing() {
        assert_eq!(MOCK_ADDR, Addr::from_str(MOCK_STR).unwrap());
        assert_eq!(MOCK_ADDR, from_json_value::<Addr>(json!(MOCK_STR)).unwrap());
        Addr::from_str(&INVALID_MOCK_STR).unwrap_err();
    }

    // Test cases from ERC-55 spec:
    // https://github.com/ethereum/ercs/blob/master/ERCS/erc-55.md#test-cases
    #[cfg(feature = "erc55")]
    #[test_case(
        hex!("52908400098527886e0f7030069857d2e4169ee7"),
        "0x52908400098527886E0F7030069857D2E4169EE7";
        "all caps 1"
    )]
    #[test_case(
        hex!("8617e340b3d01fa5f11f306f4090fd50e238070d"),
        "0x8617E340B3D01FA5F11F306F4090FD50E238070D";
        "all caps 2"
    )]
    #[test_case(
        hex!("de709f2102306220921060314715629080e2fb77"),
        "0xde709f2102306220921060314715629080e2fb77";
        "all lower 1"
    )]
    #[test_case(
        hex!("27b1fdb04752bbc536007a920d24acb045561c26"),
        "0x27b1fdb04752bbc536007a920d24acb045561c26";
        "all lower 2"
    )]
    #[test_case(
        hex!("5aaeb6053f3e94c9b9a09f33669435e7ef1beaed"),
        "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
        "normal 1"
    )]
    #[test_case(
        hex!("fb6916095ca1df60bb79ce92ce3ea74c37c5d359"),
        "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359";
        "normal 2"
    )]
    #[test_case(
        hex!("dbf03b407c01e7cd3cbea99509d93f8dddc8c6fb"),
        "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB";
        "normal 3"
    )]
    #[test_case(
        hex!("d1220a0cf47c7b9be7a2e6ba89f429762e7b9adb"),
        "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb";
        "normal 4"
    )]
    fn stringify_erc55(raw: [u8; 20], expect: &str) {
        let addr = Addr::from_array(raw);
        let actual = addr.to_erc55_string();
        assert_eq!(actual, expect);
    }
}
