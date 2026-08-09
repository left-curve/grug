use {
    crate::Secret,
    aes_gcm::{
        Aes256Gcm, Key, KeyInit,
        aead::{Aead, Generate, Nonce},
    },
    anyhow::anyhow,
    dango_primitives::{Binary, ByteArray, Inner, JsonDeExt, JsonSerExt},
    pbkdf2::pbkdf2_hmac,
    sha2::Sha256,
    std::{fs, path::Path},
};

const SECP256K1_COMPRESSED_PUBKEY_LEN: usize = 33;
const PBKDF2_ITERATIONS: u32 = 600_000;
const PBKDF2_SALT_LEN: usize = 16;
const PBKDF2_KEY_LEN: usize = 32;
const AES256GCM_NONCE_LEN: usize = 12;

/// Data structure for encrypting a 32-byte private key before saving on disk.
#[dango_primitives::derive(Serde)]
pub struct Keystore {
    pub pk: ByteArray<SECP256K1_COMPRESSED_PUBKEY_LEN>,
    pub salt: ByteArray<PBKDF2_SALT_LEN>,
    pub nonce: ByteArray<AES256GCM_NONCE_LEN>,
    pub ciphertext: Binary,
}

impl Keystore {
    /// Read and decrypt a keystore file.
    pub fn from_file<F, P>(filename: F, password: P) -> anyhow::Result<[u8; 32]>
    where
        F: AsRef<Path>,
        P: AsRef<[u8]>,
    {
        // read keystore file
        let keystore_str = fs::read_to_string(filename)?;
        let keystore: Keystore = keystore_str.deserialize_json()?;

        // recover encryption key from password and salt
        let mut password_hash = [0u8; PBKDF2_KEY_LEN];
        pbkdf2_hmac::<Sha256>(
            password.as_ref(),
            &keystore.salt,
            PBKDF2_ITERATIONS,
            &mut password_hash,
        );

        // decrypt the private key
        let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(password_hash));

        cipher
            .decrypt(
                &keystore.nonce.into_inner().into(),
                keystore.ciphertext.as_ref(),
            )?
            .try_into()
            .map_err(|bytes: Vec<u8>| {
                anyhow!(
                    "incorrect private key length! expecting: 32, got: {}",
                    bytes.len()
                )
            })
    }

    /// Encrypt a key and save it to a file.
    pub fn write_to_file<S, F, P>(secret: &S, filename: F, password: P) -> anyhow::Result<Self>
    where
        S: Secret,
        S::Private: AsRef<[u8]>,
        S::Public: Into<ByteArray<SECP256K1_COMPRESSED_PUBKEY_LEN>>,
        F: AsRef<Path>,
        P: AsRef<[u8]>,
    {
        // generate encryption key
        let salt = <[u8; PBKDF2_SALT_LEN]>::generate();
        let mut password_hash = [0u8; PBKDF2_KEY_LEN];
        pbkdf2_hmac::<Sha256>(
            password.as_ref(),
            &salt,
            PBKDF2_ITERATIONS,
            &mut password_hash,
        );

        // encrypt the private key
        let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(password_hash));
        let nonce = Nonce::<Aes256Gcm>::generate();
        let ciphertext = cipher.encrypt(&nonce, secret.private_key().as_ref())?;

        // write keystore to file
        let keystore = Keystore {
            pk: secret.public_key().into(),
            salt: salt.into(),
            nonce: (&nonce[..]).try_into()?,
            ciphertext: ciphertext.into(),
        };
        let keystore_str = keystore.to_json_string_pretty()?;
        fs::write(filename, keystore_str.as_bytes())?;

        Ok(keystore)
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {super::*, crate::Secp256k1};

    #[test]
    fn keystore_roundtrip() {
        let path = std::env::temp_dir().join("dango_sdk_keystore_roundtrip_test.json");
        let _ = fs::remove_file(&path);

        let secret = Secp256k1::new_random();
        Keystore::write_to_file(&secret, &path, "correct horse battery staple").unwrap();

        // Decrypting with the correct password recovers the private key.
        let sk = Keystore::from_file(&path, "correct horse battery staple").unwrap();
        assert_eq!(sk, secret.private_key());

        // Decrypting with an incorrect password fails.
        assert!(Keystore::from_file(&path, "hunter2").is_err());

        fs::remove_file(&path).unwrap();
    }
}
