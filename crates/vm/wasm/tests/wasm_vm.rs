use {
    grug_crypto::{sha2_256, sha2_512, Identity256, Identity512},
    grug_tester::{CryptoVerifyType, QueryMsg as TesterQueryMsg},
    grug_testing::TestBuilder,
    grug_types::{
        to_json_value, Addr, Binary, Coins, Empty, Message, MultiplyFraction, NonZero, NumberConst,
        Udec128, Uint256,
    },
    grug_vm_wasm::{VmError, WasmVm},
    rand::rngs::OsRng,
    std::{collections::BTreeMap, fs, io, str::FromStr, vec},
    test_case::test_case,
};

const WASM_CACHE_CAPACITY: usize = 10;
const DENOM: &str = "ugrug";
const FEE_RATE: &str = "0.1";

fn read_wasm_file(filename: &str) -> io::Result<Binary> {
    let path = format!("{}/testdata/{filename}", env!("CARGO_MANIFEST_DIR"));
    fs::read(path).map(Into::into)
}

#[test]
fn bank_transfers() -> anyhow::Result<()> {
    let (mut suite, accounts) = TestBuilder::new_with_vm(WasmVm::new(WASM_CACHE_CAPACITY))
        .add_account("owner", Coins::new())?
        .add_account("sender", Coins::one(DENOM, NonZero::new(300_000_u128)))?
        .add_account("receiver", Coins::new())?
        .set_owner("owner")?
        .set_fee_denom(DENOM)
        .set_fee_rate(Udec128::from_str(FEE_RATE)?)
        .build()?;

    // Check that sender has been given 300,000 ugrug.
    // Sender needs to have sufficient tokens to cover gas fee and the transfers.
    suite
        .query_balance(&accounts["sender"], DENOM)
        .should_succeed_and_equal(Uint256::from(300_000_u128));
    suite
        .query_balance(&accounts["receiver"], DENOM)
        .should_succeed_and_equal(Uint256::ZERO);

    // Sender sends 70 ugrug to the receiver across multiple messages
    let outcome = suite.send_messages_with_gas(&accounts["sender"], 2_500_000, vec![
        Message::Transfer {
            to: accounts["receiver"].address.clone(),
            coins: Coins::one(DENOM, NonZero::new(10_u128)),
        },
        Message::Transfer {
            to: accounts["receiver"].address.clone(),
            coins: Coins::one(DENOM, NonZero::new(15_u128)),
        },
        Message::Transfer {
            to: accounts["receiver"].address.clone(),
            coins: Coins::one(DENOM, NonZero::new(20_u128)),
        },
        Message::Transfer {
            to: accounts["receiver"].address.clone(),
            coins: Coins::one(DENOM, NonZero::new(25_u128)),
        },
    ])?;

    outcome.result.should_succeed();

    // Sender remaining balance should be 300k - 70 - withhold + (withhold - charge).
    // = 300k - 70 - charge
    let fee = Uint256::from(outcome.gas_used).checked_mul_dec_ceil(Udec128::from_str(FEE_RATE)?)?;
    let sender_balance_after = Uint256::from(300_000_u128 - 70) - fee;

    // Check balances again
    suite
        .query_balance(&accounts["sender"], DENOM)
        .should_succeed_and_equal(sender_balance_after);
    suite
        .query_balance(&accounts["receiver"], DENOM)
        .should_succeed_and_equal(Uint256::from(70_u128));

    let info = suite.query_info().should_succeed();

    // List all holders of the denom
    suite
        .query_wasm_smart::<_, BTreeMap<Addr, Uint256>>(
            info.config.bank,
            &grug_bank::QueryMsg::Holders {
                denom: DENOM.to_string(),
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and_equal(BTreeMap::from([
            (accounts["owner"].address.clone(), fee),
            (accounts["sender"].address.clone(), sender_balance_after),
            (accounts["receiver"].address.clone(), Uint256::from(70_u128)),
        ]));

    Ok(())
}

#[test]
fn gas_limit_too_low() -> anyhow::Result<()> {
    let (mut suite, accounts) = TestBuilder::new_with_vm(WasmVm::new(WASM_CACHE_CAPACITY))
        .add_account("owner", Coins::new())?
        .add_account("sender", Coins::one(DENOM, NonZero::new(200_000_u128)))?
        .add_account("receiver", Coins::new())?
        .set_owner("owner")?
        .set_fee_rate(Udec128::from_str(FEE_RATE)?)
        .build()?;

    // Make a bank transfer with a small gas limit; should fail.
    // Bank transfers should take around ~1M gas.
    //
    // We can't easily tell whether gas will run out during the Wasm execution
    // (in which case, the error would be a `VmError::GasDepletion`) or during
    // a host function call (in which case, a `VmError::OutOfGas`). We can only
    // say that the error has to be one of the two. Therefore, we simply ensure
    // the error message contains the word "gas".
    let outcome = suite.send_message_with_gas(&accounts["sender"], 100_000, Message::Transfer {
        to: accounts["receiver"].address.clone(),
        coins: Coins::one(DENOM, NonZero::new(10_u128)),
    })?;

    outcome.result.should_fail();

    // The transfer should have failed, but gas fee already spent is still charged.
    let fee = Uint256::from(outcome.gas_used).checked_mul_dec_ceil(Udec128::from_str(FEE_RATE)?)?;
    let sender_balance_after = Uint256::from(200_000_u128) - fee;

    // Tx is went out of gas.
    // Balances should remain the same
    suite
        .query_balance(&accounts["sender"], DENOM)
        .should_succeed_and_equal(sender_balance_after);
    suite
        .query_balance(&accounts["receiver"], DENOM)
        .should_succeed_and_equal(Uint256::ZERO);

    Ok(())
}

#[test]
fn infinite_loop() -> anyhow::Result<()> {
    let (mut suite, accounts) = TestBuilder::new_with_vm(WasmVm::new(WASM_CACHE_CAPACITY))
        .add_account("owner", Coins::new())?
        .add_account("sender", Coins::one(DENOM, NonZero::new(32_100_000_u128)))?
        .set_owner("owner")?
        .set_fee_rate(Udec128::from_str(FEE_RATE)?)
        .build()?;

    let (_, tester) = suite.upload_and_instantiate_with_gas(
        &accounts["sender"],
        320_000_000,
        read_wasm_file("grug_tester.wasm")?,
        "tester",
        &grug_tester::InstantiateMsg {},
        Coins::new(),
    )?;

    suite
        .send_message_with_gas(&accounts["sender"], 1_000_000, Message::Execute {
            contract: tester,
            msg: to_json_value(&grug_tester::ExecuteMsg::InfiniteLoop {})?,
            funds: Coins::new(),
        })?
        .result
        .should_fail_with_error("out of gas");

    Ok(())
}

#[test]
fn immutable_state() -> anyhow::Result<()> {
    let (mut suite, accounts) = TestBuilder::new_with_vm(WasmVm::new(WASM_CACHE_CAPACITY))
        .add_account("owner", Coins::new())?
        .add_account("sender", Coins::one(DENOM, NonZero::new(32_100_000_u128)))?
        .set_owner("owner")?
        .set_fee_rate(Udec128::from_str(FEE_RATE)?)
        .build()?;

    // Deploy the tester contract
    let (_, tester) = suite.upload_and_instantiate_with_gas(
        &accounts["sender"],
        // Currently, deploying a contract consumes an exceedingly high amount
        // of gas because of the need to allocate hundreds ok kB of contract
        // bytecode into Wasm memory and have the contract deserialize it...
        320_000_000,
        read_wasm_file("grug_tester.wasm")?,
        "tester",
        &grug_tester::InstantiateMsg {},
        Coins::new(),
    )?;

    // Query the tester contract.
    //
    // During the query, the contract attempts to write to the state by directly
    // calling the `db_write` import.
    //
    // This tests how the VM handles state mutability while serving the `Query`
    // ABCI request.
    suite
        .query_wasm_smart::<_, Empty>(tester.clone(), &grug_tester::QueryMsg::ForceWrite {
            key: "larry".to_string(),
            value: "engineer".to_string(),
        })
        .should_fail_with_error(VmError::ReadOnly);

    // Execute the tester contract.
    //
    // During the execution, the contract makes a query to itself and the query
    // tries to write to the storage.
    //
    // This tests how the VM handles state mutability while serving the
    // `FinalizeBlock` ABCI request.
    suite
        .send_message_with_gas(&accounts["sender"], 1_000_000, Message::Execute {
            contract: tester,
            msg: to_json_value(&grug_tester::ExecuteMsg::ForceWriteOnQuery {
                key: "larry".to_string(),
                value: "engineer".to_string(),
            })?,
            funds: Coins::new(),
        })?
        .result
        .should_fail_with_error(VmError::ReadOnly);

    Ok(())
}

const MSG: &[u8] = b"finger but hole";

const WRONG_MSG: &[u8] = b"precious item ahead";

fn secp256k1() -> (TesterQueryMsg, fn(&[u8]) -> [u8; 32]) {
    use k256::ecdsa::{signature::DigestSigner, Signature, SigningKey, VerifyingKey};

    let sk = SigningKey::random(&mut OsRng);
    let vk = VerifyingKey::from(&sk);
    let msg_hash = Identity256::from(sha2_256(MSG));
    let sig: Signature = sk.sign_digest(msg_hash.clone());

    (
        TesterQueryMsg::CryptoVerify {
            ty: CryptoVerifyType::Secp256k1,
            pk: vk.to_sec1_bytes().to_vec(),
            sig: sig.to_bytes().to_vec(),
            msg_hash: msg_hash.to_vec(),
        },
        sha2_256,
    )
}

fn secp256r1() -> (TesterQueryMsg, fn(&[u8]) -> [u8; 32]) {
    use p256::ecdsa::{signature::DigestSigner, Signature, SigningKey, VerifyingKey};

    let sk = SigningKey::random(&mut OsRng);
    let vk = VerifyingKey::from(&sk);
    let msg_hash = Identity256::from(sha2_256(MSG));
    let sig: Signature = sk.sign_digest(msg_hash.clone());

    (
        TesterQueryMsg::CryptoVerify {
            ty: CryptoVerifyType::Secp256r1,
            pk: vk.to_sec1_bytes().to_vec(),
            sig: sig.to_bytes().to_vec(),
            msg_hash: msg_hash.to_vec(),
        },
        sha2_256,
    )
}

fn ed25519() -> (TesterQueryMsg, fn(&[u8]) -> [u8; 64]) {
    use ed25519_dalek::{DigestSigner, SigningKey, VerifyingKey};

    let sk = SigningKey::generate(&mut OsRng);
    let vk = VerifyingKey::from(&sk);
    let msg_hash = Identity512::from(sha2_512(MSG));
    let sig = sk.sign_digest(msg_hash.clone());

    (
        TesterQueryMsg::CryptoVerify {
            ty: CryptoVerifyType::Ed25519,
            pk: vk.as_bytes().to_vec(),
            sig: sig.to_bytes().to_vec(),
            msg_hash: msg_hash.to_vec(),
        },
        sha2_512,
    )
}

#[test_case(secp256k1; "wasm_secp256k1")]
#[test_case(secp256r1; "wasm_secp256r1")]
#[test_case(ed25519; "wasm_ed25519")]
fn export_crypto_verify<const N: usize>(
    clos: fn() -> (TesterQueryMsg, fn(&[u8]) -> [u8; N]),
) -> anyhow::Result<()> {
    let (mut suite, accounts) = TestBuilder::new_with_vm(WasmVm::new(WASM_CACHE_CAPACITY))
        .add_account("owner", Coins::new())?
        .add_account("sender", Coins::one(DENOM, NonZero::new(32_100_000_u128)))?
        .set_owner("owner")?
        .set_fee_rate(Udec128::from_str(FEE_RATE)?)
        .set_tracing_level(None)
        .build()?;

    // Deploy the tester contract
    let (_, tester) = suite.upload_and_instantiate_with_gas(
        &accounts["sender"],
        // Currently, deploying a contract consumes an exceedingly high amount
        // of gas because of the need to allocate hundreds ok kB of contract
        // bytecode into Wasm memory and have the contract deserialize it...
        320_000_000,
        read_wasm_file("grug_tester.wasm")?,
        "tester",
        &grug_tester::InstantiateMsg {},
        Coins::new(),
    )?;

    let (mut query_msg, hash_fn) = clos();

    // Ok
    {
        suite
            .query_wasm_smart::<_, ()>(tester.clone(), &query_msg)
            .should_succeed();
    }

    // Err, verify sign failed
    {
        // Replace the message with a wrong one
        if let TesterQueryMsg::CryptoVerify { msg_hash, .. } = &mut query_msg {
            *msg_hash = hash_fn(WRONG_MSG).to_vec();
        };
        suite
            .query_wasm_smart::<_, ()>(tester.clone(), &query_msg)
            .should_fail_with_error("verify sign failed");
    }

    // Err, wrong msg format
    {
        // Replace the message with a wrong one
        if let TesterQueryMsg::CryptoVerify { msg_hash, .. } = &mut query_msg {
            *msg_hash = WRONG_MSG.to_vec();
        };
        suite
            .query_wasm_smart::<_, ()>(tester, &query_msg)
            .should_fail_with_error("invalid msg format");
    }

    Ok(())
}

#[test]
fn wasm_secp256k1_pubkey_recover() -> anyhow::Result<()> {
    use k256::ecdsa::{SigningKey, VerifyingKey};

    let (mut suite, accounts) = TestBuilder::new_with_vm(WasmVm::new(WASM_CACHE_CAPACITY))
        .add_account("owner", Coins::new())?
        .add_account("sender", Coins::one(DENOM, NonZero::new(32_100_000_u128)))?
        .set_owner("owner")?
        .set_fee_rate(Udec128::from_str(FEE_RATE)?)
        .set_tracing_level(None)
        .build()?;

    // Deploy the tester contract
    let (_, tester) = suite.upload_and_instantiate_with_gas(
        &accounts["sender"],
        // Currently, deploying a contract consumes an exceedingly high amount
        // of gas because of the need to allocate hundreds ok kB of contract
        // bytecode into Wasm memory and have the contract deserialize it...
        320_000_000,
        read_wasm_file("grug_tester.wasm")?,
        "tester",
        &grug_tester::InstantiateMsg {},
        Coins::new(),
    )?;

    // Generate a valid signature
    let sk = SigningKey::random(&mut OsRng);
    let vk = VerifyingKey::from(&sk);
    let msg_hash = Identity256::from(sha2_256(MSG));
    let (sig, recovery_id) = sk.sign_digest_recoverable(msg_hash.clone()).unwrap();

    let mut query_msg = TesterQueryMsg::RecoverSepc256k1 {
        sig: sig.to_vec(),
        msg_hash: msg_hash.to_vec(),
        recovery_id: recovery_id.to_byte(),
        compressed: true,
    };

    // Ok
    {
        let pk = suite
            .query_wasm_smart::<_, Vec<u8>>(tester.clone(), &query_msg)
            .should_succeed();

        assert_eq!(pk.to_vec(), vk.to_sec1_bytes().to_vec());
    }

    // Different msg, succeed but pk is different
    {
        if let TesterQueryMsg::RecoverSepc256k1 { msg_hash, .. } = &mut query_msg {
            *msg_hash = sha2_256(WRONG_MSG).to_vec();
        };
        let pk = suite
            .query_wasm_smart::<_, Vec<u8>>(tester.clone(), &query_msg)
            .should_succeed();

        assert_ne!(pk.to_vec(), vk.to_sec1_bytes().to_vec());
    }

    // Wrong msg format
    {
        if let TesterQueryMsg::RecoverSepc256k1 { msg_hash, .. } = &mut query_msg {
            *msg_hash = WRONG_MSG.to_vec();
        };

        suite
            .query_wasm_smart::<_, Vec<u8>>(tester.clone(), &query_msg)
            .should_fail_with_error("invalid msg format");
    }

    Ok(())
}

#[test]
fn wasm_ed25519_batch_verify() -> anyhow::Result<()> {
    use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

    fn ed25519_sign(msg: &str) -> (Vec<u8>, [u8; 64], [u8; 32]) {
        let sk = SigningKey::generate(&mut OsRng);
        let vk = VerifyingKey::from(&sk);
        let sig = sk.sign(msg.as_bytes());
        (msg.as_bytes().to_vec(), sig.to_bytes(), vk.to_bytes())
    }

    let (mut suite, accounts) = TestBuilder::new_with_vm(WasmVm::new(WASM_CACHE_CAPACITY))
        .add_account("owner", Coins::new())?
        .add_account("sender", Coins::one(DENOM, NonZero::new(32_100_000_u128)))?
        .set_owner("owner")?
        .set_fee_rate(Udec128::from_str(FEE_RATE)?)
        .set_tracing_level(None)
        .build()?;

    // Deploy the tester contract
    let (_, tester) = suite.upload_and_instantiate_with_gas(
        &accounts["sender"],
        // Currently, deploying a contract consumes an exceedingly high amount
        // of gas because of the need to allocate hundreds ok kB of contract
        // bytecode into Wasm memory and have the contract deserialize it...
        320_000_000,
        read_wasm_file("grug_tester.wasm")?,
        "tester",
        &grug_tester::InstantiateMsg {},
        Coins::new(),
    )?;

    let (prehash_msg1, sig1, vk1) = ed25519_sign("Jake");
    let (prehash_msg2, sig2, vk2) = ed25519_sign("Larry");
    let (prehash_msg3, sig3, vk3) = ed25519_sign("Rhaki");

    let mut query_msg = TesterQueryMsg::Ed25519BatchVerify {
        prehash_msgs: vec![prehash_msg1, prehash_msg2, prehash_msg3],
        sigs: vec![sig1.to_vec(), sig2.to_vec(), sig3.to_vec()],
        pks: vec![vk1.to_vec(), vk2.to_vec(), vk3.to_vec()],
    };

    // Ok
    {
        suite
            .query_wasm_smart::<_, ()>(tester.clone(), &query_msg)
            .should_succeed();
    }

    // Revert sign
    {
        if let TesterQueryMsg::Ed25519BatchVerify { sigs, .. } = &mut query_msg {
            sigs.reverse();
        };
        suite
            .query_wasm_smart::<_, ()>(tester.clone(), &query_msg)
            .should_fail_with_error("verify sign failed");
    }

    Ok(())
}
