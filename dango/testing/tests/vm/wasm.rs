use {
    super::{WASM_CACHE_CAPACITY, read_wasm_file},
    dango_app::{AppError, NaiveProposalPreparer, NullIndexer},
    dango_backtrace::Backtraceable,
    dango_crypto::sha2_256,
    dango_db_memory::MemDb,
    dango_genesis::{GenesisCodes, GenesisOption},
    dango_primitives::{
        Addr, Binary, Coins, GenericResult, InnerMut, Message, QuerierExt, QueryRequest, ResultExt,
        VerificationError,
    },
    dango_tester::{
        QueryRecoverSecp256k1Request, QueryVerifySecp256k1Request, QueryVerifySecp256r1Request,
    },
    dango_testing::{Preset, TestAccounts, TestSuite, setup_suite_with_db_and_vm},
    dango_vm_hybrid::HybridVm,
    dango_vm_rust::RustVm,
    dango_vm_wasm::VmError,
    k256::elliptic_curve::Generate,
    serde::{Serialize, de::DeserializeOwned},
    std::fmt::Debug,
    test_case::test_case,
};

async fn setup_test() -> (
    TestSuite<MemDb, HybridVm, NaiveProposalPreparer>,
    TestAccounts,
    Addr,
) {
    let codes = RustVm::genesis_codes();
    let vm = HybridVm::new(WASM_CACHE_CAPACITY, codes.all_code_hashes());

    let (mut suite, mut accounts, ..) = setup_suite_with_db_and_vm(
        MemDb::new(),
        vm,
        NaiveProposalPreparer,
        NullIndexer,
        codes,
        Default::default(),
        GenesisOption::preset_test(),
    );

    let tester = suite
        .upload_and_instantiate_with_gas(
            &mut accounts.owner,
            320_000_000,
            read_wasm_file("dango_tester.wasm"),
            &dango_tester::InstantiateMsg {},
            "tester",
            Some("tester"),
            None,
            Coins::new(),
        )
        .await
        .should_succeed()
        .address;

    (suite, accounts, tester)
}

// --------------------------- vm correctness tests ----------------------------

#[tokio::test]
async fn infinite_loop() {
    let (mut suite, mut accounts, tester) = setup_test().await;

    suite
        .send_message_with_gas(
            &mut accounts.owner,
            1_000_000,
            Message::execute(
                tester,
                &dango_tester::ExecuteMsg::InfiniteLoop {},
                Coins::new(),
            )
            .unwrap(),
        )
        .await
        .should_fail_with_error("out of gas");
}

#[tokio::test]
async fn immutable_state() {
    let (mut suite, mut accounts, tester) = setup_test().await;

    // Query the tester contract.
    //
    // During the query, the contract attempts to write to the state by directly
    // calling the `db_write` import.
    //
    // This tests how the VM handles state mutability while serving the `Query`
    // ABCI request.
    suite
        .query_wasm_smart(
            tester,
            dango_tester::QueryForceWriteRequest {
                key: "larry".to_string(),
                value: "engineer".to_string(),
            },
        )
        .should_fail_with_error(VmError::immutable_state());

    // Execute the tester contract.
    //
    // During the execution, the contract makes a query to itself and the query
    // tries to write to the storage.
    //
    // This tests how the VM handles state mutability while serving the
    // `FinalizeBlock` ABCI request.
    suite
        .send_message_with_gas(
            &mut accounts.owner,
            2_000_000,
            Message::execute(
                tester,
                &dango_tester::ExecuteMsg::ForceWriteOnQuery {
                    key: "larry".to_string(),
                    value: "engineer".to_string(),
                },
                Coins::new(),
            )
            .unwrap(),
        )
        .await
        .should_fail_with_error(VmError::immutable_state());
}

#[tokio::test]
async fn query_stack_overflow() {
    let (suite, _, tester) = setup_test().await;

    // The contract attempts to call with `QueryMsg::StackOverflow` to itself in
    // a loop. Should raise the "exceeded max query depth" error.
    suite
        .query_wasm_smart(tester, dango_tester::QueryStackOverflowRequest {})
        .should_fail_with_error(VmError::exceed_max_query_depth());
}

#[tokio::test]
async fn message_stack_overflow() {
    let (mut suite, mut accounts, tester) = setup_test().await;

    // The contract attempts to return a Response with `Execute::StackOverflow`
    // to itself in a loop. Should raise the "exceeded max message depth" error.
    suite
        .send_message_with_gas(
            &mut accounts.owner,
            10_000_000,
            Message::execute(
                tester,
                &dango_tester::ExecuteMsg::StackOverflow {},
                Coins::new(),
            )
            .unwrap(),
        )
        .await
        .should_fail_with_error(AppError::exceed_max_message_depth());
}

// ------------------------------- crypto tests --------------------------------

const MSG: &[u8] = b"finger but hole";
const WRONG_MSG: &[u8] = b"precious item ahead";

fn generate_secp256r1_verify_request() -> QueryVerifySecp256r1Request {
    use p256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::hazmat::PrehashSigner};

    let sk = SigningKey::generate();
    let vk = VerifyingKey::from(&sk);
    let msg_hash = sha2_256(MSG);
    let sig: Signature = sk.sign_prehash(&msg_hash).unwrap();

    QueryVerifySecp256r1Request {
        pk: vk.to_sec1_bytes().to_vec().into(),
        sig: sig.to_bytes().to_vec().into(),
        msg_hash: msg_hash.into(),
    }
}

fn generate_secp256k1_verify_request() -> QueryVerifySecp256k1Request {
    use k256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::hazmat::PrehashSigner};

    let sk = SigningKey::generate();
    let vk = VerifyingKey::from(&sk);
    let msg_hash = sha2_256(MSG);
    let sig: Signature = sk.sign_prehash(&msg_hash).unwrap();

    QueryVerifySecp256k1Request {
        pk: vk.to_sec1_bytes().to_vec().into(),
        sig: sig.to_bytes().to_vec().into(),
        msg_hash: msg_hash.into(),
    }
}

// ----- Secp256r1 -----
#[test_case(
    generate_secp256r1_verify_request,
    |req| req,
    GenericResult::Ok(());
    "valid secp256r1 signature"
)]
#[test_case(
    generate_secp256r1_verify_request,
    |mut req| {
        req.pk.inner_mut().pop();
        req
    },
    GenericResult::Err(VerificationError::incorrect_length().into_generic_backtraced_error());
    "invalid secp256r1: incorrect pk length"
)]
#[test_case(
    generate_secp256r1_verify_request,
    |mut req| {
        req.sig.inner_mut().pop();
        req
    },
    GenericResult::Err(VerificationError::incorrect_length().into_generic_backtraced_error());
    "invalid secp256r1: incorrect signature length"
)]
#[test_case(
    generate_secp256r1_verify_request,
    |mut req| {
        req.msg_hash.inner_mut().pop();
        req
    },
    GenericResult::Err(VerificationError::incorrect_length().into_generic_backtraced_error());
    "invalid secp256r1: incorrect msg hash length"
)]
#[test_case(
    generate_secp256r1_verify_request,
    |mut req| {
        let sk = p256::ecdsa::SigningKey::generate();
        let vk = p256::ecdsa::VerifyingKey::from(&sk);
        req.pk = vk.to_sec1_bytes().to_vec().into();
        req
    },
    GenericResult::Err(VerificationError::unauthentic().into_generic_backtraced_error());
    "invalid secp256r1: different pk"
)]
#[test_case(
    generate_secp256r1_verify_request,
    |mut req| {
        req.msg_hash = sha2_256(WRONG_MSG).to_vec().into();
        req
    },
    GenericResult::Err(VerificationError::unauthentic().into_generic_backtraced_error());
    "invalid secp256r1: different msg"
)]
// ----- Secp256k1 -----
#[test_case(
    generate_secp256k1_verify_request,
    |req| req,
    GenericResult::Ok(());
    "valid secp256k1 signature"
)]
#[test_case(
    generate_secp256k1_verify_request,
    |mut req| {
        req.pk.inner_mut().push(0);
        req
    },
    GenericResult::Err(VerificationError::incorrect_length().into_generic_backtraced_error());
    "invalid secp256k1: incorrect pk length"
)]
#[test_case(
    generate_secp256k1_verify_request,
    |mut req| {
        req.sig.inner_mut().push(0);
        req
    },
    GenericResult::Err(VerificationError::incorrect_length().into_generic_backtraced_error());
    "invalid secp256k1: incorrect signature length"
)]
#[test_case(
    generate_secp256k1_verify_request,
    |mut req| {
        req.msg_hash.inner_mut().push(0);
        req
    },
    GenericResult::Err(VerificationError::incorrect_length().into_generic_backtraced_error());
    "invalid secp256k1: incorrect msg hash length"
)]
#[test_case(
    generate_secp256k1_verify_request,
    |mut req| {
        let sk = k256::ecdsa::SigningKey::generate();
        let vk = k256::ecdsa::VerifyingKey::from(&sk);
        req.pk = vk.to_sec1_bytes().to_vec().into();
        req
    },
    GenericResult::Err(VerificationError::unauthentic().into_generic_backtraced_error());
    "invalid secp256k1: different pk"
)]
#[test_case(
    generate_secp256k1_verify_request,
    |mut req| {
        req.msg_hash = sha2_256(WRONG_MSG).to_vec().into();
        req
    },
    GenericResult::Err(VerificationError::unauthentic().into_generic_backtraced_error());
    "invalid secp256k1: different msg"
)]
#[tokio::test]
async fn verifying_signature<G, M, R>(
    generate_request: G,
    malleate: M,
    expect: GenericResult<R::Response>,
) where
    G: FnOnce() -> R,
    M: FnOnce(R) -> R,
    R: QueryRequest,
    R::Message: Serialize,
    R::Response: DeserializeOwned + Debug + PartialEq,
{
    let (suite, _, tester) = setup_test().await;

    // Generate and malleate query request
    let req = generate_request();
    let req = malleate(req);

    suite.query_wasm_smart(tester, req).should_match(expect);
}

#[tokio::test]
async fn recovering_secp256k1_pubkey() {
    let (suite, _, tester) = setup_test().await;

    // Generate a valid signature
    let (vk, req) = {
        use k256::ecdsa::{SigningKey, VerifyingKey};

        let sk = SigningKey::generate();
        let vk = VerifyingKey::from(&sk);
        let msg_hash = sha2_256(MSG);
        let (sig, recovery_id) = sk.sign_prehash_recoverable(&msg_hash);

        (
            vk,
            QueryRecoverSecp256k1Request {
                sig: sig.to_vec().into(),
                msg_hash: msg_hash.to_vec().into(),
                recovery_id: recovery_id.to_byte(),
                compressed: true,
            },
        )
    };

    // Ok
    {
        suite
            .query_wasm_smart(tester, req.clone())
            .should_succeed_and_equal(Binary::from_inner(vk.to_sec1_bytes().to_vec()));
    }

    // Attempt to recover with a different msg. Should succeed but pk is different.
    {
        let mut false_req = req.clone();
        false_req.msg_hash = sha2_256(WRONG_MSG).into();

        suite
            .query_wasm_smart(tester, false_req)
            .should_succeed_but_not_equal(Binary::from_inner(vk.to_sec1_bytes().to_vec()));
    }

    // Attempt to recover with an invalid recovery ID. Should error.
    {
        let mut false_req = req;
        false_req.recovery_id = 123;

        suite
            .query_wasm_smart(tester, false_req)
            .should_fail_with_error(VerificationError::invalid_recovery_id());
    }
}
