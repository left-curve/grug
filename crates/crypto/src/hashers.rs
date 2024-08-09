use {
    crate::truncate,
    blake2::{Blake2b512, Blake2s256},
    digest::Digest,
    grug_types::CryptoError,
    sha2::{Sha256, Sha512},
    sha3::{Keccak256, Sha3_256, Sha3_512},
};

pub fn sha2_256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn sha2_512(data: &[u8]) -> [u8; 64] {
    Sha512::digest(data).into()
}

pub fn sha2_512_truncated(data: &[u8]) -> [u8; 32] {
    truncate(&sha2_512(data), CryptoError::InvalidMsg).unwrap()
}

pub fn sha3_256(data: &[u8]) -> [u8; 32] {
    Sha3_256::digest(data).into()
}

pub fn sha3_512(data: &[u8]) -> [u8; 64] {
    Sha3_512::digest(data).into()
}

pub fn sha3_512_truncated(data: &[u8]) -> [u8; 32] {
    truncate(&sha3_512(data), CryptoError::InvalidMsg).unwrap()
}

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    Keccak256::digest(data).into()
}

pub fn blake2s_256(data: &[u8]) -> [u8; 32] {
    Blake2s256::digest(data).into()
}

pub fn blake2b_512(data: &[u8]) -> [u8; 64] {
    Blake2b512::digest(data).into()
}

pub fn blake3(data: &[u8]) -> [u8; 32] {
    blake3::hash(data).into()
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use hex_literal::hex;

    macro_rules! hash_test {
        ($hash_fn:ident, $word:expr => $hash:expr) => {
            #[test]
            fn $hash_fn() {
                let hash = super::$hash_fn($word.as_bytes());
                assert_eq!(hash, hex!($hash))
            }
        };
    }

    hash_test!(sha2_256,           "food" => "c1f026582fe6e8cb620d0c85a72fe421ddded756662a8ec00ed4c297ad10676b");
    hash_test!(sha2_512,           "food" => "c235548cfe84fc87678ff04c9134e060cdcd7512d09ed726192151a995541ed8db9fda5204e72e7ac268214c322c17787c70530513c59faede52b7dd9ce64331");
    hash_test!(sha2_512_truncated, "food" => "c235548cfe84fc87678ff04c9134e060cdcd7512d09ed726192151a995541ed8");
    hash_test!(sha3_256,           "food" => "c6ab604b6867239fce3071722ab1ca096063d0164e730dfc557d1f3f0fcead6b");
    hash_test!(sha3_512,           "food" => "8cd7cab3e1e542c16c56d91e105f48145313557c34dab00014b5ed56151eb78f96d58948646579904192e9c88d6577d74a69702de7d52f519e31dad1cef3115d");
    hash_test!(sha3_512_truncated, "food" => "8cd7cab3e1e542c16c56d91e105f48145313557c34dab00014b5ed56151eb78f");
    hash_test!(keccak256,          "food" => "a471c7c90860799b1facb54795f0a93d821fb727241025770865602471b765a8");
    hash_test!(blake2s_256,        "food" => "5a1ec796f11f3dfc7e8ca5de13828edf2e910eb7dd41caaac356a4acbefb1758");
    hash_test!(blake2b_512,        "food" => "b1f115361afc179415d93d4f58dc2fc7d8fa434192d7cb9b65fca592f6aa904103d1f12b28655c2355478e10908ab002c418dc52a4367d8e645309cd25e3a504");
    hash_test!(blake3,             "food" => "f775a8ccf8cb78cd1c63ade4e9802de4ead836b36cea35242accf31d2c6a3697");
}
