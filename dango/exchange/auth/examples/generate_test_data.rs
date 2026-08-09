use {
    dango_auth::{MAX_NONCE_INCREASE, VerifyData, build_eip712_typed_data},
    dango_primitives::{
        Addr, Binary, ByteArray, Hash256, HashExt, Inner, JsonSerExt, MOCK_CHAIN_ID, Message,
        NonEmpty, SignData, Timestamp, Tx, coins,
    },
    dango_types::{
        account_factory::RegisterUserData,
        auth::{
            Credential, Eip712Signature, Metadata, PasskeySignature, SessionCredential,
            SessionInfo, SignDoc, Signature, StandardCredential,
        },
    },
    data_encoding::BASE64URL_NOPAD,
    k256::{ecdsa::signature::hazmat::PrehashSigner, elliptic_curve::Generate},
    rand::Rng,
    sha2::{Digest, Sha256},
};

// -------------------------------- secp256k1 ----------------------------------

fn generate_secp256k1_standard_test_data() -> anyhow::Result<()> {
    let (sk, vk, vk_hash) = generate_random_secp256k1_key_pair()?;
    let sign_doc = generate_random_unsigned_transaction()?;

    let credential = {
        let signature = secp256k1_sign(&sk, &sign_doc)?;
        Credential::Standard(StandardCredential {
            key_hash: vk_hash,
            signature: Signature::Secp256k1(signature),
        })
    };

    let tx = Tx {
        sender: sign_doc.sender,
        gas_limit: sign_doc.gas_limit,
        msgs: sign_doc.messages,
        data: sign_doc.data.to_json_value()?,
        credential: credential.to_json_value()?,
    };

    println!("user_address = {}", sign_doc.sender);
    println!("user_index   = {}", sign_doc.data.user_index);
    println!("user_keyhash = {}", hex::encode(vk_hash));
    println!("user_key     = {}", hex::encode(vk));
    println!("tx:\n{}", tx.to_json_string_pretty()?);

    Ok(())
}

fn generate_secp256k1_session_test_data() -> anyhow::Result<()> {
    // 1 is the main key, 2 is the session key.
    let (sk1, vk1, vk1_hash) = generate_random_secp256k1_key_pair()?;
    let (sk2, vk2, _) = generate_random_secp256k1_key_pair()?;
    let sign_doc = generate_random_unsigned_transaction()?;

    let session_info = SessionInfo {
        chain_id: MOCK_CHAIN_ID.to_string(),
        session_key: vk2.into(),
        expire_at: Timestamp::from_nanos(u128::MAX),
    };

    // Main key signs the authorization.
    let authorization = {
        let signature = secp256k1_sign(&sk1, &session_info)?;
        StandardCredential {
            key_hash: vk1_hash,
            signature: Signature::Secp256k1(signature),
        }
    };

    // Session keys signs the transaction.
    let session_signature = secp256k1_sign(&sk2, &sign_doc)?;

    let credential = Credential::Session(SessionCredential {
        session_info,
        session_signature,
        authorization,
    });

    let tx = Tx {
        sender: sign_doc.sender,
        gas_limit: sign_doc.gas_limit,
        msgs: sign_doc.messages,
        data: sign_doc.data.to_json_value()?,
        credential: credential.to_json_value()?,
    };

    println!("user_address = {}", sign_doc.sender);
    println!("user_index   = {}", sign_doc.data.user_index);
    println!("user_keyhash = {}", hex::encode(vk1_hash));
    println!("user_key     = {}", hex::encode(vk1));
    println!("tx:\n{}", tx.to_json_string_pretty()?);

    Ok(())
}

// ---------------------------------- eip712 -----------------------------------

fn generate_eip712_standard_test_data() -> anyhow::Result<()> {
    let (sk, eth_addr) = generate_random_ethereum_key_pair();
    let eth_addr_dango = Addr::from_inner(eth_addr);
    let eth_addr_key_hash = eth_addr.sha2_256();

    let verify_data = VerifyData::Transaction(generate_random_unsigned_transaction()?);

    let credential = {
        let eip712_sig = eip712_sign(&sk, &verify_data)?;
        Credential::Standard(StandardCredential {
            key_hash: eth_addr_key_hash,
            signature: Signature::Eip712(eip712_sig),
        })
    };

    let VerifyData::Transaction(sign_doc) = verify_data else {
        unreachable!()
    };

    let tx = Tx {
        sender: sign_doc.sender,
        gas_limit: sign_doc.gas_limit,
        msgs: sign_doc.messages,
        data: sign_doc.data.to_json_value()?,
        credential: credential.to_json_value()?,
    };

    println!("user_address = {}", sign_doc.sender);
    println!("user_index   = {}", sign_doc.data.user_index);
    println!("user_keyhash = {}", hex::encode(eth_addr_key_hash));
    println!("user_key     = ethereum:{}", eth_addr_dango);
    println!("tx:\n{}", tx.to_json_string_pretty()?);

    Ok(())
}

fn generate_eip712_session_test_data() -> anyhow::Result<()> {
    // Main key is an Ethereum key; session key is secp256k1.
    let (sk1, eth_addr) = generate_random_ethereum_key_pair();
    let eth_addr_dango = Addr::from_inner(eth_addr);
    let eth_addr_key_hash = eth_addr.sha2_256();

    let (sk2, vk2, _) = generate_random_secp256k1_key_pair()?;
    let sign_doc = generate_random_unsigned_transaction()?;

    let verify_data = VerifyData::Session(SessionInfo {
        chain_id: MOCK_CHAIN_ID.to_string(),
        session_key: vk2.into(),
        expire_at: Timestamp::from_nanos(u128::MAX),
    });

    // Main key signs the SessionInfo via EIP-712.
    let authorization = {
        let eip712_sig = eip712_sign(&sk1, &verify_data)?;
        StandardCredential {
            key_hash: eth_addr_key_hash,
            signature: Signature::Eip712(eip712_sig),
        }
    };

    let VerifyData::Session(session_info) = verify_data else {
        unreachable!()
    };

    // Session key signs the transaction.
    let session_signature = secp256k1_sign(&sk2, &sign_doc)?;

    let credential = Credential::Session(SessionCredential {
        session_info,
        session_signature,
        authorization,
    });

    let tx = Tx {
        sender: sign_doc.sender,
        gas_limit: sign_doc.gas_limit,
        msgs: sign_doc.messages,
        data: sign_doc.data.to_json_value()?,
        credential: credential.to_json_value()?,
    };

    println!("user_address = {}", sign_doc.sender);
    println!("user_index   = {}", sign_doc.data.user_index);
    println!("user_keyhash = {}", hex::encode(eth_addr_key_hash));
    println!("user_key     = ethereum:{}", eth_addr_dango);
    println!("tx:\n{}", tx.to_json_string_pretty()?);

    Ok(())
}

fn generate_eip712_onboard_test_data() -> anyhow::Result<()> {
    let (sk, eth_addr) = generate_random_ethereum_key_pair();
    let eth_addr_dango = Addr::from_inner(eth_addr);
    let eth_addr_key_hash = eth_addr.sha2_256();

    let verify_data = VerifyData::Onboard(RegisterUserData {
        chain_id: MOCK_CHAIN_ID.to_string(),
        key: dango_types::auth::Key::Ethereum(eth_addr_dango),
        key_hash: eth_addr_key_hash,
        seed: 0,
        referrer: None,
    });

    let eip712_sig = eip712_sign(&sk, &verify_data)?;
    let signature_json = Signature::Eip712(eip712_sig).to_json_string_pretty()?;

    println!("user_key     = ethereum:{}", eth_addr_dango);
    println!("user_keyhash = {}", hex::encode(eth_addr_key_hash));
    println!("chain_id     = {}", MOCK_CHAIN_ID);
    println!("signature:\n{}", signature_json);

    Ok(())
}

// ---------------------------------- passkey ----------------------------------

fn generate_passkey_session_test_data() -> anyhow::Result<()> {
    // Main key is a passkey (Secp256r1); session key is secp256k1.
    let sk1 = p256::ecdsa::SigningKey::generate();
    let vk1: [u8; 33] = sk1
        .verifying_key()
        .to_sec1_point(true)
        .as_bytes()
        .try_into()?;
    let vk1_hash = vk1.sha2_256();

    let (sk2, vk2, _) = generate_random_secp256k1_key_pair()?;
    let sign_doc = generate_random_unsigned_transaction()?;

    let session_info = SessionInfo {
        chain_id: MOCK_CHAIN_ID.to_string(),
        session_key: vk2.into(),
        expire_at: Timestamp::from_nanos(u128::MAX),
    };

    // Sign SessionInfo via simulated passkey/WebAuthn.
    let authorization = {
        let passkey_sig = passkey_sign(&sk1, &session_info)?;
        StandardCredential {
            key_hash: vk1_hash,
            signature: Signature::Passkey(passkey_sig),
        }
    };

    // Session key signs the transaction.
    let session_signature = secp256k1_sign(&sk2, &sign_doc)?;

    let credential = Credential::Session(SessionCredential {
        session_info,
        session_signature,
        authorization,
    });

    let tx = Tx {
        sender: sign_doc.sender,
        gas_limit: sign_doc.gas_limit,
        msgs: sign_doc.messages,
        data: sign_doc.data.to_json_value()?,
        credential: credential.to_json_value()?,
    };

    println!("user_address = {}", sign_doc.sender);
    println!("user_index   = {}", sign_doc.data.user_index);
    println!("user_keyhash = {}", hex::encode(vk1_hash));
    println!("user_key     = secp256r1:{}", hex::encode(vk1));
    println!("tx:\n{}", tx.to_json_string_pretty()?);

    Ok(())
}

// ---------------------------------- helpers -----------------------------------

fn generate_random_unsigned_transaction() -> anyhow::Result<SignDoc> {
    let mut sender = Addr::mock(0);
    rand::rng().fill_bytes(&mut sender);

    let mut recipient = Addr::mock(0);
    rand::rng().fill_bytes(&mut recipient);

    let user_index = rand::random();
    let nonce = rand::random_range(0..MAX_NONCE_INCREASE);
    // EIP-712 types `gas_limit` as uint32 (see `tx_eip712_resolver`), matching
    // the frontend and the raw-secp256k1 range in practice, so keep the
    // generated value within u32 range.
    let gas_limit = u64::from(rand::random::<u32>());

    let messages = NonEmpty::new_unchecked(vec![Message::transfer(
        recipient,
        coins! { "bridge/usdc" => 100_000_000 },
    )?]);

    let data = Metadata {
        chain_id: MOCK_CHAIN_ID.to_string(),
        user_index,
        nonce,
        expiry: None,
    };

    Ok(SignDoc {
        sender,
        gas_limit,
        messages,
        data,
    })
}

/// Return the private key, public key, and SHA-256 hash of the public key.
fn generate_random_secp256k1_key_pair()
-> anyhow::Result<(k256::ecdsa::SigningKey, [u8; 33], Hash256)> {
    let sk = k256::ecdsa::SigningKey::generate();
    let vk: [u8; 33] = sk
        .verifying_key()
        .to_sec1_point(true)
        .as_bytes()
        .try_into()?;
    let vk_hash = vk.sha2_256();

    Ok((sk, vk, vk_hash))
}

/// Generate a random Ethereum key pair, returning (signing_key, 20-byte address).
fn generate_random_ethereum_key_pair() -> (k256::ecdsa::SigningKey, [u8; 20]) {
    let sk = k256::ecdsa::SigningKey::generate();
    let addr = dango_eth_utils::derive_address(sk.verifying_key());
    (sk, addr)
}

fn secp256k1_sign<T>(sk: &k256::ecdsa::SigningKey, sign_doc: &T) -> anyhow::Result<ByteArray<64>>
where
    T: SignData,
    anyhow::Error: From<T::Error>,
{
    let prehash_sign_data = sign_doc.to_prehash_sign_data()?;
    let sign_data = prehash_sign_data.sha2_256();
    let signature: k256::ecdsa::Signature = sk.sign_prehash(&sign_data.into_inner())?;

    Ok(ByteArray::from_inner(signature.to_bytes().into()))
}

/// Sign a payload via EIP-712, producing an `Eip712Signature`.
///
/// Routes through the same `dango_auth::build_eip712_typed_data` the chain
/// verifies against, so generated fixtures always match the on-chain
/// reconstruction. Enum-typed fields (transaction `messages`, onboarding `key`)
/// are bound as canonical JSON strings.
fn eip712_sign(sk: &k256::ecdsa::SigningKey, data: &VerifyData) -> anyhow::Result<Eip712Signature> {
    let typed_data = build_eip712_typed_data(data)?;
    let signing_hash = typed_data.eip712_signing_hash()?;

    // Sign with an Ethereum-style recoverable signature.
    let sig_bytes = dango_eth_utils::sign_digest(signing_hash.0, sk);

    Ok(Eip712Signature {
        typed_data: Binary::from(typed_data.to_json_vec()?),
        sig: ByteArray::from_inner(sig_bytes),
    })
}

/// Simulate a WebAuthn/passkey signature over SignData.
fn passkey_sign<T>(sk: &p256::ecdsa::SigningKey, data: &T) -> anyhow::Result<PasskeySignature>
where
    T: SignData,
    anyhow::Error: From<T::Error>,
{
    // Compute the sign data (SHA-256 hash of the prehash).
    let sign_data = data.to_sign_data()?;
    let challenge = BASE64URL_NOPAD.encode(&sign_data);

    // Construct client_data JSON (must match what WebAuthn produces).
    let client_data_json = format!(
        r#"{{"type":"webauthn.get","challenge":"{}","origin":"http://localhost:5080","crossOrigin":false}}"#,
        challenge
    );
    let client_data_bytes = client_data_json.as_bytes();

    // Fixed authenticator_data: 32-byte rpIdHash for localhost + flags(0x19) + counter(0).
    let authenticator_data: [u8; 37] = [
        0x49, 0x96, 0x0d, 0xe5, 0x88, 0x0e, 0x8c, 0x68, 0x74, 0x34, 0x17, 0x0f, 0x64, 0x76, 0x60,
        0x5b, 0x8f, 0xe4, 0xae, 0xb9, 0xa2, 0x86, 0x32, 0xc7, 0x99, 0x5c, 0xf3, 0xba, 0x83, 0x1d,
        0x97, 0x63, 0x19, 0x00, 0x00, 0x00, 0x00,
    ];

    // Compute signed_hash = sha256(authenticator_data || sha256(client_data))
    let client_data_hash: [u8; 32] = Sha256::digest(client_data_bytes).into();
    let signed_data = [authenticator_data.as_slice(), client_data_hash.as_slice()].concat();
    let signed_hash: [u8; 32] = Sha256::digest(&signed_data).into();

    // Sign with p256.
    let signature: p256::ecdsa::Signature = sk.sign_prehash(&signed_hash)?;

    Ok(PasskeySignature {
        sig: ByteArray::from_inner(signature.to_bytes().into()),
        client_data: Binary::from(client_data_bytes.to_vec()),
        authenticator_data: Binary::from(authenticator_data.to_vec()),
    })
}

fn main() -> anyhow::Result<()> {
    println!("===================== Secp256k1 Standard ======================");
    generate_secp256k1_standard_test_data()?;

    println!("\n====================== Secp256k1 Session ======================");
    generate_secp256k1_session_test_data()?;

    println!("\n======================= EIP712 Standard ========================");
    generate_eip712_standard_test_data()?;

    println!("\n======================== EIP712 Session ========================");
    generate_eip712_session_test_data()?;

    println!("\n======================== EIP712 Onboard ========================");
    generate_eip712_onboard_test_data()?;

    println!("\n======================== Passkey Session ========================");
    generate_passkey_session_test_data()?;

    Ok(())
}
