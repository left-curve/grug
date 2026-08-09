//! Generate a random seed phrase. Derive the corresponding Secp256k1 key pair
//! using Ethereum's derivation path.

use {
    bip32::{Language, Mnemonic, XPrv},
    k256::elliptic_curve::Generate,
};

const HD_PATH: &str = "m/44'/60'/0'/0/0";

fn main() {
    // Generate a random seed phrase. Generate the entropy ourselves instead of
    // using bip32's `random` constructor, which requires RNGs of the outdated
    // rand_core version pinned by bip32.
    let mnemonic = Mnemonic::from_entropy(<[u8; 32]>::generate(), Language::English);
    let seed = mnemonic.to_seed(""); // empty password
    let path = HD_PATH.parse().unwrap();

    // Seed phrase --> private key
    let sk = XPrv::derive_from_path(seed, &path).unwrap();
    let sk_hex = hex::encode(sk.to_bytes());

    // Private key --> public key
    let pk = sk.public_key();
    let pk_hex = hex::encode(pk.to_bytes());

    println!("Seed phrase:\n{}", mnemonic.phrase());
    println!("\nPrivate key:\n{sk_hex}");
    println!("\nPublic key:\n{pk_hex}");
}
