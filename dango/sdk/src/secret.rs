use {
    bip32::{Mnemonic, XPrv},
    dango_auth::VerifyData,
    dango_primitives::{Addr, ByteArray, Hash256, HashExt, JsonSerExt, SignData},
    dango_types::auth::{Eip712Signature, Key, SignDoc, Signature},
    k256::{ecdsa::signature::hazmat::PrehashSigner, elliptic_curve::Generate},
};

/// Represents a secret key that can sign transactions.
pub trait Secret: Sized {
    /// Byte array representing the private key.
    type Private;

    /// Byte array representing the public key associated with this private key.
    type Public;

    /// Byte array representing the signature produced by this secret key.
    type Signature;

    /// Generate a new random private key using the system's secure RNG.
    fn new_random() -> Self;

    /// Recover a private key from raw bytes.
    fn from_bytes(bytes: Self::Private) -> anyhow::Result<Self>;

    /// Recover a private key from the given English mnemonic and BIP-44 coin type.
    fn from_mnemonic(mnemonic: &Mnemonic, coin_type: usize) -> anyhow::Result<Self>;

    /// Return the private key as a byte array.
    fn private_key(&self) -> Self::Private;

    /// Return the compressed public key as a byte array.
    fn public_key(&self) -> Self::Public;

    /// Return the [`Key`](dango_types::auth::Key) for using in the Dango account factory.
    fn key(&self) -> Key;

    /// Return the key hash for use in the Dango account factory.
    fn key_hash(&self) -> Hash256;

    /// Sign the given sign doc.
    fn sign_transaction(&self, sign_doc: SignDoc) -> anyhow::Result<Signature>;
}

// --------------------------------- Secp256r1 ---------------------------------

// TODO: Secp256r1 secret.

// --------------------------------- Secp256k1 ---------------------------------

/// An Secp256k1 private key.
#[derive(Debug, Clone)]
pub struct Secp256k1 {
    inner: k256::ecdsa::SigningKey,
}

impl Secret for Secp256k1 {
    type Private = [u8; 32];
    type Public = [u8; 33];
    type Signature = [u8; 64];

    fn new_random() -> Self {
        Self {
            inner: k256::ecdsa::SigningKey::generate(),
        }
    }

    fn from_bytes(bytes: [u8; 32]) -> anyhow::Result<Self> {
        Ok(Self {
            inner: k256::ecdsa::SigningKey::from_bytes(&bytes.into())?,
        })
    }

    fn from_mnemonic(mnemonic: &Mnemonic, coin_type: usize) -> anyhow::Result<Self> {
        // The `to_seed` function takes a password to generate salt.
        // Here we just use an empty str.
        // For reference, Terra Station and Keplr use an empty string as well:
        // - https://github.com/terra-money/terra.js/blob/v3.1.7/src/key/MnemonicKey.ts#L79
        // - https://github.com/chainapsis/keplr-wallet/blob/b6062a4d24f3dcb15dda063b1ece7d1fbffdbfc8/packages/crypto/src/mnemonic.ts#L63
        let seed = mnemonic.to_seed("");
        let path = format!("m/44'/{coin_type}'/0'/0/0");
        let xprv = XPrv::derive_from_path(&seed, &path.parse()?)?;

        // Go through the raw private key bytes instead of bip32's `From<XPrv>`
        // conversion, which targets the k256 version pinned by bip32 rather
        // than the one used by this crate.
        Self::from_bytes(xprv.to_bytes())
    }

    fn private_key(&self) -> [u8; 32] {
        self.inner.to_bytes().into()
    }

    fn public_key(&self) -> [u8; 33] {
        self.inner
            .verifying_key()
            .to_sec1_point(true)
            .as_bytes()
            .try_into()
            .expect("compressed pubkey is 33 bytes")
    }

    fn key(&self) -> Key {
        Key::Secp256k1(self.public_key().into())
    }

    fn key_hash(&self) -> Hash256 {
        self.public_key().sha2_256() // SHA-256 key
    }

    fn sign_transaction(&self, sign_doc: SignDoc) -> anyhow::Result<Signature> {
        let sign_data = sign_doc.to_sign_data()?;
        let signature: k256::ecdsa::Signature = self.inner.sign_prehash(&sign_data)?;

        Ok(Signature::Secp256k1(ByteArray::from_inner(
            signature.to_bytes().into(),
        )))
    }
}

// --------------------------------- Ethereum ----------------------------------

/// An Secp256k1 private key that signs message in Ethereum EIP-712 format.
#[derive(Debug, Clone)]
pub struct Eip712 {
    inner: Secp256k1,
    // This means the Ethereum address, not the Dango address.
    pub address: dango_eth_utils::Address,
}

impl Secret for Eip712 {
    type Private = <Secp256k1 as Secret>::Private;
    type Public = <Secp256k1 as Secret>::Public;
    // 64 bytes of signature + 1 byte of recovery ID
    type Signature = [u8; 65];

    fn new_random() -> Self {
        Secp256k1::new_random().into()
    }

    fn from_bytes(bytes: [u8; 32]) -> anyhow::Result<Self> {
        Secp256k1::from_bytes(bytes).map(Into::into)
    }

    fn from_mnemonic(mnemonic: &Mnemonic, coin_type: usize) -> anyhow::Result<Self> {
        Secp256k1::from_mnemonic(mnemonic, coin_type).map(Into::into)
    }

    fn private_key(&self) -> [u8; 32] {
        self.inner.private_key()
    }

    fn public_key(&self) -> [u8; 33] {
        self.inner.public_key()
    }

    fn key(&self) -> dango_types::auth::Key {
        dango_types::auth::Key::Ethereum(self.address.into())
    }

    fn key_hash(&self) -> Hash256 {
        Addr::from(self.address).to_string().as_bytes().sha2_256()
    }

    fn sign_transaction(&self, sign_doc: SignDoc) -> anyhow::Result<Signature> {
        // Build the typed data via `dango_auth`'s shared builder, so the SDK and
        // the on-chain verifier produce byte-identical EIP-712 hashes. `messages`
        // is bound as an array of canonical JSON strings (EIP-712 can't express
        // the `Message` enum as a struct).
        let data = dango_auth::build_eip712_typed_data(&VerifyData::Transaction(sign_doc))?;

        let sign_bytes = data.eip712_signing_hash()?;
        let (signature, recovery_id) = self.inner.inner.sign_prehash_recoverable(&sign_bytes.0);

        Ok(Signature::Eip712(Eip712Signature {
            typed_data: data.to_json_vec()?.into(),
            sig: dango_eth_utils::pack_signature(signature, recovery_id).into(),
        }))
    }
}

impl From<Secp256k1> for Eip712 {
    fn from(inner: Secp256k1) -> Self {
        let address = dango_eth_utils::derive_address(inner.inner.verifying_key());
        Self { inner, address }
    }
}
