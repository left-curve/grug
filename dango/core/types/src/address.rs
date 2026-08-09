use crate::{AddrEncoder, EncodedBytes, Hash256, HashExt, Inner};

/// A shorthand for constructing a constant address from a hex string (without
/// the `0x`-prefix).
///
/// This is equivalent to:
///
/// ```ignore
/// Addr::from_inner(hex_literal::hex!("..."))
/// ```
#[macro_export]
macro_rules! addr {
    ($hex:literal) => {
        $crate::Addr::from_inner($crate::__private::hex_literal::hex!($hex))
    };
}

/// An account address.
///
/// In Dango, addresses are of 20-byte length, in Hex encoding and the `0x` prefix.
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
/// In Dango, addresses are validated during deserialization. If deserialization
/// doesn't throw an error, you can be sure the address is valid. Therefore it
/// is safe to use `Addr`s in JSON messages.
pub type Addr = EncodedBytes<[u8; 20], AddrEncoder>;

impl Addr {
    pub const LENGTH: usize = 20;
    pub const ZERO: Self = Self::from_inner([0; Self::LENGTH]);

    /// Generate a mock address from use in testing.
    pub const fn mock(index: u8) -> Self {
        let mut bytes = [0; Self::LENGTH];
        bytes[Self::LENGTH - 1] = index;
        Self::from_inner(bytes)
    }

    /// Derive a contract address as:
    ///
    /// ```plain
    /// address := ripemd160(sha256(deployer_addr | code_hash | salt))
    /// ```
    ///
    /// where `|` means byte concatenation.
    ///
    /// The double hash the same as used by Bitcoin, for [preventing length
    /// extension attacks](https://bitcoin.stackexchange.com/questions/8443/where-is-double-hashing-performed-in-bitcoin).
    pub fn derive(deployer: Addr, code_hash: Hash256, salt: &[u8]) -> Self {
        let mut preimage = Vec::with_capacity(Self::LENGTH + Hash256::LENGTH + salt.len());
        preimage.extend_from_slice(deployer.as_ref());
        preimage.extend_from_slice(code_hash.as_ref());
        preimage.extend_from_slice(salt);
        Self::from_inner(preimage.sha2_256().ripemd160().into_inner())
    }
}
