use data_encoding::{Encoding, BASE64, HEXLOWER, HEXUPPER};

/// Describes a scheme for encoding bytes to strings.
pub trait Encoder {
    const NAME: &str;
    const ENCODING: Encoding;
    const PREFIX: &str;
}

/// Binary encoder for raw bytes using the base64 scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Base64Encoder;

impl Encoder for Base64Encoder {
    const ENCODING: Encoding = BASE64;
    const NAME: &str = "Base64";
    const PREFIX: &str = "";
}

/// Binary encoder for addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AddrEncoder;

impl Encoder for AddrEncoder {
    const ENCODING: Encoding = HEXLOWER;
    const NAME: &str = "Addr";
    const PREFIX: &str = "0x";
}

/// Binary encoder for hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HashEncoder;

impl Encoder for HashEncoder {
    const ENCODING: Encoding = HEXUPPER;
    const NAME: &str = "Hash";
    const PREFIX: &str = "";
}
