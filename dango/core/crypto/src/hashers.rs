use {
    sha2::{Digest, Sha256},
    sha3::Keccak256,
};

pub fn sha2_256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    Keccak256::digest(data).into()
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

    hash_test!(sha2_256, "food" => "c1f026582fe6e8cb620d0c85a72fe421ddded756662a8ec00ed4c297ad10676b");
    hash_test!(keccak256, "food" => "a471c7c90860799b1facb54795f0a93d821fb727241025770865602471b765a8");
}
