use {
    crate::{EncodedBytes, HashEncoder},
    grug_math::Inner,
};

/// A hash of a fixed length, in uppercase hex encoding.
pub type Hash<const N: usize> = EncodedBytes<[u8; N], HashEncoder>;

/// A 20-byte hash, in uppercase hex encoding.
pub type Hash160 = Hash<20>;

/// A 32-byte hash, in uppercase hex encoding.
pub type Hash256 = Hash<32>;

/// A 64-byte hash, in uppercase hex encoding.
pub type Hash512 = Hash<64>;

impl<const N: usize> Hash<N> {
    /// The length (number of bytes) of hashes.
    ///
    /// In Grug, we use SHA-256 hash everywhere, of which the length is 32 bytes.
    ///
    /// Do not confuse length in terms of bytes and in terms of ASCII characters.
    /// We use Hex encoding, which uses 2 ASCII characters per byte, so the
    /// ASCII length should be 64.
    pub const LENGTH: usize = N;
    /// A zeroed-out hash. Useful as mockups or placeholders.
    pub const ZERO: Self = Self::from_array([0; N]);

    /// Create a new hash from a byte array of the correct length.
    pub const fn from_array(array: [u8; N]) -> Self {
        Self::from_inner(array)
    }

    /// Cast the hash into a byte array.
    pub fn into_array(self) -> [u8; N] {
        self.into_inner()
    }

    /// Cast the hash into a byte vector.
    pub fn into_vec(self) -> Vec<u8> {
        self.inner().to_vec()
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        crate::{json, Hash256, JsonDeExt, JsonSerExt},
        hex_literal::hex,
        std::str::FromStr,
    };

    // just a random block hash I grabbed from MintScan
    const MOCK_JSON: &str = "299663875422CC5A4574816E6165824D0C5BFDBA3D58D94D37E8D832A572555B";
    const MOCK_HASH: Hash256 = Hash256::from_inner(hex!(
        "299663875422cc5a4574816e6165824d0c5bfdba3d58d94d37e8d832a572555b"
    ));

    #[test]
    fn serializing() {
        assert_eq!(MOCK_JSON, MOCK_HASH.to_string());
        assert_eq!(json!(MOCK_JSON), MOCK_HASH.to_json_value().unwrap());
    }

    #[test]
    fn deserializing() {
        assert_eq!(MOCK_HASH, Hash256::from_str(MOCK_JSON).unwrap());
        assert_eq!(
            MOCK_HASH,
            json!(MOCK_JSON).deserialize_json::<Hash256>().unwrap()
        );

        // Lowercase hex strings are not accepted
        let illegal_json = json!(MOCK_JSON.to_lowercase());
        assert!(illegal_json.deserialize_json::<Hash256>().is_err());

        // Incorrect length
        // Trim the last two characters, so the string only represents 31 bytes
        let illegal_json = json!(MOCK_JSON[..MOCK_JSON.len() - 2]);
        assert!(illegal_json.deserialize_json::<Hash256>().is_err());
    }
}
