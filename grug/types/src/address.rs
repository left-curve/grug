use {
    crate::{AddrEncoder, EncodedBytes, Hash256, HashExt},
    grug_math::Inner,
};
#[cfg(feature = "erc55")]
use {
    crate::{StdError, StdResult},
    sha3::{Digest, Keccak256},
    std::str::FromStr,
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
pub type Addr = EncodedBytes<[u8; 20], AddrEncoder>;

impl Addr {
    pub const LENGTH: usize = 20;

    /// Create a new address from a 32-byte byte slice.
    pub const fn from_array(array: [u8; Self::LENGTH]) -> Self {
        Self::from_inner(array)
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
    pub fn compute(deployer: Addr, code_hash: Hash256, salt: &[u8]) -> Self {
        let mut preimage = Vec::with_capacity(Self::LENGTH + Hash256::LENGTH + salt.len());
        preimage.extend_from_slice(deployer.as_ref());
        preimage.extend_from_slice(code_hash.as_ref());
        preimage.extend_from_slice(salt);
        Self::from_inner(preimage.hash256().hash160().into_inner())
    }

    /// Generate a mock address from use in testing.
    pub const fn mock(index: u8) -> Self {
        let mut bytes = [0; Self::LENGTH];
        bytes[Self::LENGTH - 1] = index;
        Self::from_inner(bytes)
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
        HEXLOWER.encode_mut(self.inner(), &mut buf[2..]);

        let mut hasher = Keccak256::new();
        // Note we're hashing the UTF-8 hex string, not the raw bytes.
        hasher.update(&buf[2..]);
        let hash = hasher.finalize();

        let mut hash_hex = [0; 64];
        HEXLOWER.encode_mut(&hash, &mut hash_hex);

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
            return Err(StdError::deserialize::<Self, _>(
                "hex",
                "invalid ERC-55 checksum",
            ));
        }

        Ok(addr)
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    #[cfg(feature = "erc55")]
    use test_case::test_case;
    use {
        crate::{json, Addr, JsonDeExt, JsonSerExt},
        hex_literal::hex,
        std::str::FromStr,
    };

    // the same as the mock hash from the Hash unit tests, except cropped to 20
    // bytes and with the `0x` prefix.
    const MOCK_STR: &str = "0x299663875422cc5a4574816e6165824d0c5bfdba";
    const MOCK_ADDR: Addr = Addr::from_inner(hex!("299663875422cc5a4574816e6165824d0c5bfdba"));

    #[test]
    fn serializing() {
        assert_eq!(MOCK_STR, MOCK_ADDR.to_string());
        assert_eq!(json!(MOCK_STR), MOCK_ADDR.to_json_value().unwrap());
    }

    #[test]
    fn deserializing() {
        assert_eq!(MOCK_ADDR, Addr::from_str(MOCK_STR).unwrap());
        assert_eq!(
            MOCK_ADDR,
            json!(MOCK_STR).deserialize_json::<Addr>().unwrap()
        );
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
