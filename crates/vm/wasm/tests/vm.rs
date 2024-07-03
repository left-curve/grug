use {
    anyhow::ensure,
    grug_account::{make_sign_bytes, PublicKey, StateResponse},
    grug_app::{App, AppError, AppResult},
    grug_crypto::{sha2_256, Identity256},
    grug_db_memory::MemDb,
    grug_types::{
        from_json_value, to_json_value, Addr, Binary, BlockInfo, Coin, Coins, Config, Empty, Event,
        GenesisState, Hash, Message, NumberConst, Permission, Permissions, QueryRequest,
        QueryResponse, Timestamp, Tx, Uint64, GENESIS_SENDER,
    },
    grug_vm_wasm::WasmVm,
    k256::ecdsa::{signature::DigestSigner, Signature, SigningKey},
    rand::rngs::OsRng,
    serde::{de::DeserializeOwned, ser::Serialize},
    std::{
        collections::{BTreeMap, BTreeSet},
        fs, io, vec,
    },
    tracing::subscriber::SetGlobalDefaultError,
};

const MOCK_CHAIN_ID: &str = "grug-1";
const MOCK_DENOM: &str = "ugrug";
const MOCK_BANK_SALT: &[u8] = b"bank";
const MOCK_SENDER_SALT: &[u8] = b"sender";
const MOCK_RECEIVER_SALT: &[u8] = b"receiver";

fn read_wasm_file(filename: &str) -> io::Result<Vec<u8>> {
    let path = format!("{}/testdata/{filename}", env!("CARGO_MANIFEST_DIR"));
    fs::read(path)
}

fn setup_tracing() -> Result<(), SetGlobalDefaultError> {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
}

struct TestAccount {
    address: Addr,
    sk: SigningKey,
}

struct TestSuite {
    app: App<MemDb, WasmVm>,
    block: BlockInfo,
}

impl TestSuite {
    /// Initialize the chain with the default genesis state for these tests.
    /// This includes two user accounts and the bank contract.
    fn default_setup() -> anyhow::Result<(Self, TestAccount, TestAccount)> {
        let app = App::new(MemDb::new(), WasmVm::new(100), None);
        let block = BlockInfo {
            height: Uint64::ZERO,
            timestamp: Timestamp::from_nanos(0),
            hash: Hash::ZERO,
        };

        // Generate private keys for the accounts
        let sender_sk = SigningKey::random(&mut OsRng);
        let sender_pk: Binary = sender_sk
            .verifying_key()
            .to_encoded_point(true)
            .to_bytes()
            .to_vec()
            .into();
        let receiver_sk = SigningKey::random(&mut OsRng);
        let receiver_pk: Binary = receiver_sk
            .verifying_key()
            .to_encoded_point(true)
            .to_bytes()
            .to_vec()
            .into();

        // Load account contract byte code, and predict account addresses
        let account_code = read_wasm_file("grug_account.wasm")?;
        let account_code_hash = Hash::from_slice(sha2_256(&account_code));
        let sender = Addr::compute(&GENESIS_SENDER, &account_code_hash, MOCK_SENDER_SALT);
        let receiver = Addr::compute(&GENESIS_SENDER, &account_code_hash, MOCK_RECEIVER_SALT);

        // Load bank contract byte code, and predict its address.
        let bank_code = read_wasm_file("grug_bank.wasm")?;
        let bank_code_hash = Hash::from_slice(sha2_256(&bank_code));
        let bank = Addr::compute(&GENESIS_SENDER, &bank_code_hash, MOCK_BANK_SALT);

        // Genesis the chain. This deploys the bank contract and gives the "sender"
        // account 100 ugrug.
        app.do_init_chain(MOCK_CHAIN_ID.to_string(), block.clone(), GenesisState {
            config: Config {
                owner: None,
                bank: bank.clone(),
                begin_blockers: vec![],
                end_blockers: vec![],
                permissions: Permissions {
                    upload: Permission::Everybody,
                    instantiate: Permission::Everybody,
                    create_client: Permission::Everybody,
                    create_connection: Permission::Everybody,
                    create_channel: Permission::Everybody,
                },
                allowed_clients: BTreeSet::new(),
            },
            msgs: vec![
                Message::Upload {
                    code: account_code.into(),
                },
                Message::Upload {
                    code: bank_code.into(),
                },
                Message::Instantiate {
                    code_hash: account_code_hash.clone(),
                    msg: to_json_value(&grug_account::InstantiateMsg {
                        public_key: PublicKey::Secp256k1(sender_pk.to_vec().into()),
                    })?,
                    salt: MOCK_SENDER_SALT.to_vec().into(),
                    funds: Coins::new_empty(),
                    admin: Some(sender.clone()),
                },
                Message::Instantiate {
                    code_hash: account_code_hash.clone(),
                    msg: to_json_value(&grug_account::InstantiateMsg {
                        public_key: PublicKey::Secp256k1(receiver_pk.to_vec().into()),
                    })?,
                    salt: MOCK_RECEIVER_SALT.to_vec().into(),
                    funds: Coins::new_empty(),
                    admin: Some(receiver.clone()),
                },
                Message::Instantiate {
                    code_hash: bank_code_hash,
                    msg: to_json_value(&grug_bank::InstantiateMsg {
                        initial_balances: BTreeMap::from([(
                            sender.clone(),
                            Coins::new_one(MOCK_DENOM, 100_u128),
                        )]),
                    })?,
                    salt: MOCK_BANK_SALT.to_vec().into(),
                    funds: Coins::new_empty(),
                    admin: None,
                },
            ],
        })?;

        let sender = TestAccount {
            address: sender,
            sk: sender_sk,
        };

        let receiver = TestAccount {
            address: receiver,
            sk: receiver_sk,
        };

        Ok((Self { app, block }, sender, receiver))
    }

    fn query(&self, req: QueryRequest) -> AppResult<QueryResponse> {
        self.app.do_query_app(req, self.block.height.into(), false)
    }

    fn query_wasm_smart<M: Serialize, R: DeserializeOwned>(
        &self,
        contract: Addr,
        msg: &M,
    ) -> anyhow::Result<R> {
        let msg_raw = to_json_value(&msg)?;
        let res_raw = self
            .query(QueryRequest::WasmSmart {
                contract,
                msg: msg_raw,
            })?
            .as_wasm_smart()
            .data;
        Ok(from_json_value(res_raw)?)
    }

    fn query_account_sequence(&self, address: Addr) -> anyhow::Result<u32> {
        self.query_wasm_smart(address, &grug_account::QueryMsg::State {})
            .map(|res: StateResponse| res.sequence)
    }

    fn send_messages(
        &mut self,
        signer: &TestAccount,
        gas_limit: u64,
        msgs: Vec<Message>,
    ) -> anyhow::Result<AppExecuteResponse> {
        // Sign the transaction
        let sequence = self.query_account_sequence(signer.address.clone())?;
        let sign_bytes =
            make_sign_bytes(sha2_256, &msgs, &signer.address, MOCK_CHAIN_ID, sequence)?;
        let signature: Signature = signer.sk.sign_digest(Identity256::from(sign_bytes));
        let tx = Tx {
            sender: signer.address.clone(),
            msgs,
            credential: signature.to_vec().into(),
            gas_limit,
        };

        // Increment block height and block time
        self.block.height += Uint64::ONE;
        self.block.timestamp = self.block.timestamp.plus_nanos(1);

        // Finalize block + commit
        let (_, _, results) = self
            .app
            .do_finalize_block(self.block.clone(), vec![(Hash::ZERO, tx.clone())])?;

        self.app.do_commit()?;

        // Check if tx was successful
        Ok(AppExecuteResponse::new(vec![tx], results))
    }

    fn assert_balance(&self, address: &Addr, denom: &str, expect: u128) -> anyhow::Result<()> {
        let actual = self
            .query(QueryRequest::Balance {
                address: address.clone(),
                denom: denom.to_string(),
            })?
            .as_balance()
            .amount
            .number();

        ensure!(actual == expect);

        Ok(())
    }
}

#[derive(Debug)]
struct AppExecuteResponse {
    txs: Vec<Tx>,
    response: Vec<AppResult<Vec<Event>>>,
}

impl AppExecuteResponse {
    fn new(txs: Vec<Tx>, response: Vec<AppResult<Vec<Event>>>) -> Self {
        if txs.len() != response.len() {
            panic!("txs and response must have the same length");
        }
        Self { txs, response }
    }

    fn no_errors(self) -> AppResult<Vec<(Tx, Vec<Event>)>> {
        self.txs
            .into_iter()
            .zip(self.response)
            .map(|(tx, res)| res.map(|res| (tx, res)))
            .collect()
    }

    fn errors(self) -> Vec<(usize, Tx, AppError)> {
        self.txs.into_iter().zip(self.response).enumerate().fold(
            vec![],
            |mut buff, (i, (tx, response))| match response {
                Ok(_) => buff,
                Err(err) => {
                    buff.push((i, tx, err));
                    buff
                },
            },
        )
    }
}

#[test]
fn wasm_vm_works() -> anyhow::Result<()> {
    setup_tracing()?;

    let (mut suite, sender, receiver) = TestSuite::default_setup()?;

    // Check that sender has been given 100 ugrug.
    suite.assert_balance(&sender.address, MOCK_DENOM, 100)?;

    // Sender sends 25 ugrug to the receiver.
    suite
        .send_messages(&sender, 300_000, vec![Message::Transfer {
            to: receiver.address.clone(),
            coins: vec![Coin::new(MOCK_DENOM, 25_u128)].try_into().unwrap(),
        }])?
        .no_errors()?;

    // Check balances again.
    suite.assert_balance(&sender.address, MOCK_DENOM, 75)?;
    suite.assert_balance(&receiver.address, MOCK_DENOM, 25)?;

    suite
        .send_messages(&sender, 900_000, vec![
            Message::Transfer {
                to: receiver.address.clone(),
                coins: vec![Coin::new(MOCK_DENOM, 10_u128)].try_into().unwrap(),
            },
            Message::Transfer {
                to: receiver.address.clone(),
                coins: vec![Coin::new(MOCK_DENOM, 15_u128)].try_into().unwrap(),
            },
            Message::Transfer {
                to: receiver.address.clone(),
                coins: vec![Coin::new(MOCK_DENOM, 20_u128)].try_into().unwrap(),
            },
        ])?
        .no_errors()?;

    // Check balances again.
    suite.assert_balance(&sender.address, MOCK_DENOM, 30)?;
    suite.assert_balance(&receiver.address, MOCK_DENOM, 70)?;

    // Out of gas
    suite.send_messages(&sender, 100_000, vec![Message::Transfer {
        to: receiver.address.clone(),
        coins: vec![Coin::new(MOCK_DENOM, 10_u128)].try_into().unwrap(),
    }])?;

    // Tx is went out of gas.
    // Balances should remain the same
    suite.assert_balance(&sender.address, MOCK_DENOM, 30)?;
    suite.assert_balance(&receiver.address, MOCK_DENOM, 70)?;

    Ok(())
}

#[test]
fn immutable_state() -> anyhow::Result<()> {
    setup_tracing()?;

    let (mut suite, sender, _) = TestSuite::default_setup()?;

    // Load the immutable state contract byte code
    let tester_code = read_wasm_file("grug_tester_immutable_state.wasm")?;
    let tester_code_hash = Hash::from_slice(sha2_256(&tester_code));
    let salt = b"tester/immutable_state".to_vec();
    let tester = Addr::compute(&sender.address, &tester_code_hash, &salt);

    // Upload
    suite
        .send_messages(&sender, 80_000_000, vec![Message::Upload {
            code: tester_code.into(),
        }])?
        .no_errors()?;

    // Instantiate
    suite
        .send_messages(&sender, 1_000_000, vec![Message::Instantiate {
            code_hash: tester_code_hash,
            msg: to_json_value(&Empty {})?,
            salt: salt.into(),
            funds: Coins::default(),
            admin: None,
        }])?
        .no_errors()?;

    // Execute the contract.
    // During the execution the contract make a query to itself
    // and the query try to write the storage.
    let result = suite
        .send_messages(&sender, 1_000_000, vec![Message::Execute {
            contract: tester,
            msg: to_json_value(&Empty {})?,
            funds: Coins::default(),
        }])?
        .errors();

    let err = &result.first().unwrap().2;

    assert!(err
        .to_string()
        .contains("db state changed detected on readonly instance"));

    Ok(())
}
