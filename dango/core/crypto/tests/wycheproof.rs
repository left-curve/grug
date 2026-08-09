mod shared;

use {
    dango_crypto::{secp256k1_verify, secp256r1_verify, sha2_256},
    serde::{Deserialize, de},
    shared::{read_file, validate_recover_secp256k1, validate_recover_secp256r1},
};

// -------------------------------- file struct --------------------------------

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub number_of_tests: usize,
    pub test_groups: Vec<TestGroup>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TestGroup {
    pub public_key: Key,
    pub tests: Vec<TestCase>,
}

#[derive(Debug)]
pub struct Key {
    pub uncompressed: String,
    pub compressed: String,
}

impl<'de> de::Deserialize<'de> for Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct TempKey {
            uncompressed: String,
        }

        // Deserialize the uncompressed key
        let temp = TempKey::deserialize(deserializer)?;
        let uncompressed_byte = hex::decode(&temp.uncompressed).unwrap();

        // Since this struct is both used from the sepc256k1 and sepc256r1 tests,
        // try to verify the key in one of the two formats or return an error.
        let compressed =
            // Secp256k1
            if let Ok(vk) = k256::ecdsa::VerifyingKey::from_sec1_bytes(&uncompressed_byte) {
                vk.to_sec1_point(true).to_bytes()
            }
            // Secp256r1
            else if let Ok(vk) = p256::ecdsa::VerifyingKey::from_sec1_bytes(&uncompressed_byte) {
                vk.to_sec1_point(true).to_bytes()
            } else {
                Err(de::Error::custom(format!("Key {} is not in sep256k1 or sep256r1 format", temp.uncompressed)))?
            };

        Ok(Key {
            uncompressed: temp.uncompressed,
            compressed: hex::encode(compressed),
        })
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TestCase {
    pub tc_id: u32,
    pub comment: String,
    pub msg: String,
    pub sig: String,
    // "acceptable", "valid" or "invalid"
    pub result: String,
}

// ------------------------------ test definition ------------------------------

macro_rules! wycheproof_test {
    (
        $test_name:ident,
        $file_name:expr,
        $hash_fn:ident,
        $verify_fn:ident,
        $recover_fn:ident,
        $len:expr,
        $key_type:ident,
        $compressed:expr
    ) => {
        #[test]
        fn $test_name() {
            let mut tested: usize = 0;
            let File {
                number_of_tests,
                test_groups,
            } = read_file($file_name);

            for group in test_groups {
                let public_key = hex::decode(group.public_key.$key_type).unwrap();
                assert_eq!(public_key.len(), $len);

                for tc in group.tests {
                    tested += 1;
                    assert_eq!(tc.tc_id as usize, tested);

                    let message = hex::decode(tc.msg).unwrap();
                    let message_hash = $hash_fn(&message);
                    let der_signature = hex::decode(tc.sig).unwrap();

                    match tc.result.as_str() {
                        "valid" | "acceptable" => {
                            let signature = from_der(&der_signature).unwrap();
                            $verify_fn(&message_hash, &signature, &public_key).unwrap();
                            if tc.comment == "k*G has a large x-coordinate" {
                                // This case (recovery ID 2 and 3) was never supported in the implementation of
                                // secp256k1_recover_pubkey because the library we used at that time did not support it.
                                // If needed, we could enable it now in a consensus breaking change.
                            } else {
                                $recover_fn(
                                    &message_hash,
                                    &signature,
                                    &public_key,
                                    [0, 1],
                                    $compressed,
                                );
                            }
                        },
                        "invalid" => {
                            if let Ok(signature) = from_der(&der_signature) {
                                $verify_fn(&message_hash, &signature, &public_key).unwrap_err();
                            } else {
                                // invalid DER encoding, okay
                            }
                        },
                        _ => panic!("Found unexpected result value"),
                    }
                    if tc.result == "valid" {}
                }
            }
            assert_eq!(tested, number_of_tests);
        }
    };
    (K1 => $test_name:ident, $file_name:expr, $hash_fn:ident,compressed) => {
        wycheproof_test!(
            $test_name,
            $file_name,
            $hash_fn,
            secp256k1_verify,
            validate_recover_secp256k1,
            33,
            compressed,
            true
        );
    };
    (K1 => $test_name:ident, $file_name:expr, $hash_fn:ident,uncompressed) => {
        wycheproof_test!(
            $test_name,
            $file_name,
            $hash_fn,
            secp256k1_verify,
            validate_recover_secp256k1,
            65,
            uncompressed,
            false
        );
    };
    (R1 => $test_name:ident, $file_name:expr, $hash_fn:ident,compressed) => {
        wycheproof_test!(
            $test_name,
            $file_name,
            $hash_fn,
            secp256r1_verify,
            validate_recover_secp256r1,
            33,
            compressed,
            true
        );
    };
    (R1 => $test_name:ident, $file_name:expr, $hash_fn:ident,uncompressed) => {
        wycheproof_test!(
            $test_name,
            $file_name,
            $hash_fn,
            secp256r1_verify,
            validate_recover_secp256r1,
            65,
            uncompressed,
            false
        );
    };
}

fn from_der(data: &[u8]) -> Result<[u8; 64], String> {
    const DER_TAG_INTEGER: u8 = 0x02;

    let mut pos = 0;

    let Some(prefix) = data.get(pos) else {
        return Err("Could not read prefix".to_string());
    };
    pos += 1;
    if *prefix != 0x30 {
        return Err("Prefix 0x30 expected".to_string());
    }

    let Some(body_length) = data.get(pos) else {
        return Err("Could not read body length".to_string());
    };
    pos += 1;
    if data.len() - pos != *body_length as usize {
        return Err("Data length mismatch detected".to_string());
    }

    // r
    let Some(r_tag) = data.get(pos) else {
        return Err("Could not read r_tag".to_string());
    };
    pos += 1;
    if *r_tag != DER_TAG_INTEGER {
        return Err("INTEGER tag expected".to_string());
    }
    let Some(r_length) = data.get(pos).map(|rl: &u8| *rl as usize) else {
        return Err("Could not read r_length".to_string());
    };
    pos += 1;
    if r_length >= 0x80 {
        return Err("Decoding length values above 127 not supported".to_string());
    }
    if pos + r_length > data.len() {
        return Err("R length exceeds end of data".to_string());
    }
    let r_data = &data[pos..pos + r_length];
    pos += r_length;

    // s
    let Some(s_tag) = data.get(pos) else {
        return Err("Could not read s_tag".to_string());
    };
    pos += 1;
    if *s_tag != DER_TAG_INTEGER {
        return Err("INTEGER tag expected".to_string());
    }
    let Some(s_length) = data.get(pos).map(|sl| *sl as usize) else {
        return Err("Could not read s_length".to_string());
    };
    pos += 1;
    if s_length >= 0x80 {
        return Err("Decoding length values above 127 not supported".to_string());
    }
    if pos + s_length > data.len() {
        return Err("S length exceeds end of data".to_string());
    }
    let s_data = &data[pos..pos + s_length];
    pos += s_length;

    if pos != data.len() {
        return Err("Extra bytes in data input".to_string());
    }

    let r = decode_unsigned_integer(r_data, "r")?;
    let s = decode_unsigned_integer(s_data, "s")?;

    let mut out = [0u8; 64];
    out[0..32].copy_from_slice(&r);
    out[32..].copy_from_slice(&s);

    Ok(out)
}

fn decode_unsigned_integer(mut data: &[u8], name: &str) -> Result<[u8; 32], String> {
    if data.is_empty() {
        return Err(format!("{name} data is empty"));
    }

    // If high bit of first byte is set, this is interpreted as a negative integer.
    // A leading zero is needed to prevent this.
    if (data[0] & 0x80) != 0 {
        return Err(format!("{name} data missing leading zero"));
    }

    // "Leading octets of all 0's (or all 1's) are not allowed. In other words, the leftmost
    // nine bits of an encoded INTEGER value may not be all 0's or all 1's. This ensures that
    // an INTEGER value is encoded in the smallest possible number of octets."
    // https://www.oss.com/asn1/resources/asn1-made-simple/asn1-quick-reference/basic-encoding-rules.html

    // If leading byte is 0 and there is more than 1 byte, trim it.
    // If the high bit of the following byte is zero as well, the leading 0x00 was invalid.
    if data.len() > 1 && data[0] == 0 {
        data = &data[1..];
        if (data[0] & 0x80) == 0 {
            return Err(format!("{name} data has invalid leading zero"));
        }
    }

    // The other requirement (first 9 bits being all 1) is not yet checked

    // Do we need a better value range check here?
    if data.len() > 32 {
        return Err(format!("{name} data exceeded 32 bytes"));
    }

    Ok(pad_to_32(data))
}

fn pad_to_32(input: &[u8]) -> [u8; 32] {
    let shift = 32 - input.len();
    let mut out = [0u8; 32];
    out[shift..].copy_from_slice(input);
    out
}

// ------------------------------ secp256k1 tests ------------------------------

const SECP256K1_SHA256: &str = "./testdata/wycheproof/ecdsa_secp256k1_sha256_test.json";

wycheproof_test!(K1 =>
    ecdsa_secp256k1_sha256_compressed,
    SECP256K1_SHA256,
    sha2_256,
    compressed
);

wycheproof_test!(K1 =>
    ecdsa_secp256k1_sha256_uncompressed,
    SECP256K1_SHA256,
    sha2_256,
    uncompressed
);

// ------------------------------ secp256r1 tests ------------------------------

const SECP256R1_SHA256: &str = "./testdata/wycheproof/ecdsa_secp256r1_sha256_test.json";

wycheproof_test!(R1 =>
    ecdsa_secp256r1_sha256_compressed,
    SECP256R1_SHA256,
    sha2_256,
    compressed
);

wycheproof_test!(R1 =>
    ecdsa_secp256r1_sha256_uncompressed,
    SECP256R1_SHA256,
    sha2_256,
    uncompressed
);
