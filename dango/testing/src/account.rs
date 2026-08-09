use {
    crate::{TestSuite, create_signature},
    dango_app::{AppError, Db, Indexer, ProposalPreparer, Vm},
    dango_primitives::{
        Addr, Addressable, Coins, Defined, Duration, Hash256, HashExt, Json, JsonSerExt,
        MaybeDefined, Message, NonEmpty, QuerierExt, QuerierWrapper, QueryClient, QueryClientExt,
        ResultExt, SignData, Signer, StdError, StdResult, Tx, Undefined, UnsignedTx, btree_map,
    },
    dango_types::{
        DangoQuerier, account,
        account_factory::{
            self, NewUserSalt, QueryCodeHashRequest, QueryNextAccountIndexRequest,
            RegisterUserData, Salt, UserIndex,
        },
        auth::{Credential, Key, Metadata, Nonce, SignDoc, Signature, StandardCredential},
        signer::SequencedSigner,
    },
    k256::{ecdsa::SigningKey, elliptic_curve::Generate},
    sha2::Sha256,
    std::{array, collections::BTreeMap},
};

/// Accounts available for testing purposes.
#[derive(Debug, Clone)]
pub struct TestAccounts {
    pub owner: TestAccount,
    pub user1: TestAccount,
    pub user2: TestAccount,
    pub user3: TestAccount,
    pub user4: TestAccount,
    pub user5: TestAccount,
    pub user6: TestAccount,
    pub user7: TestAccount,
    pub user8: TestAccount,
    pub user9: TestAccount,
}

impl TestAccounts {
    /// Iterate over all user accounts (the owner excluded).
    pub fn users(&self) -> array::IntoIter<&TestAccount, 9> {
        [
            &self.user1,
            &self.user2,
            &self.user3,
            &self.user4,
            &self.user5,
            &self.user6,
            &self.user7,
            &self.user8,
            &self.user9,
        ]
        .into_iter()
    }

    /// Iterate over all user accounts as mutable (the owner excluded).
    pub fn users_mut(&mut self) -> array::IntoIter<&mut TestAccount, 9> {
        [
            &mut self.user1,
            &mut self.user2,
            &mut self.user3,
            &mut self.user4,
            &mut self.user5,
            &mut self.user6,
            &mut self.user7,
            &mut self.user8,
            &mut self.user9,
        ]
        .into_iter()
    }
}

// ------------------------------- test account --------------------------------

#[derive(Debug, Clone)]
pub struct TestAccount<I = Defined<UserIndex>, A = Defined<Addr>>
where
    I: MaybeDefined<UserIndex>,
    A: MaybeDefined<Addr>,
{
    pub user_index: I,
    pub address: A,
    pub nonce: Nonce,
    keys: BTreeMap<Hash256, (SigningKey, Key)>,
    sign_with: Hash256,
}

impl TestAccount<Undefined<UserIndex>, Undefined<Addr>> {
    pub fn new_key_pair() -> (SigningKey, Key) {
        let sk = SigningKey::generate();
        let pk = sk
            .verifying_key()
            .to_sec1_point(true)
            .to_bytes()
            .to_vec()
            .try_into()
            .unwrap();

        (sk, Key::Secp256k1(pk))
    }

    pub fn new_random() -> Self {
        let sk = SigningKey::generate();

        Self::new(sk)
    }

    pub fn new_from_private_key(sk_bytes: [u8; 32]) -> Self {
        let sk = SigningKey::from_bytes(&sk_bytes.into()).unwrap();

        Self::new(sk)
    }

    pub fn new(sk: SigningKey) -> Self {
        let pk = sk
            .verifying_key()
            .to_sec1_point(true)
            .to_bytes()
            .to_vec()
            .try_into()
            .unwrap();
        let key = Key::Secp256k1(pk);
        let key_hash = pk.sha2_256();

        Self {
            user_index: Undefined::new(),
            address: Undefined::new(),
            nonce: 0,
            keys: btree_map! { key_hash => (sk, key) },
            sign_with: key_hash,
        }
    }
}

impl<A> TestAccount<Undefined<UserIndex>, A>
where
    A: MaybeDefined<Addr>,
{
    pub fn set_user_index(self, user_index: UserIndex) -> TestAccount<Defined<UserIndex>, A> {
        TestAccount {
            user_index: Defined::new(user_index),
            address: self.address,
            nonce: self.nonce,
            keys: self.keys,
            sign_with: self.sign_with,
        }
    }
}

impl TestAccount<Undefined<UserIndex>, Defined<Addr>> {
    pub fn query_user_index(
        self,
        querier: QuerierWrapper<'_>,
    ) -> TestAccount<Defined<UserIndex>, Defined<Addr>> {
        let account_factory = querier.query_account_factory().unwrap();
        let user_index = querier
            .query_wasm_smart(
                account_factory,
                account_factory::QueryAccountRequest {
                    address: self.address.into_inner(),
                },
            )
            .unwrap()
            .owner;

        self.set_user_index(user_index)
    }
}

impl<I> TestAccount<I, Undefined<Addr>>
where
    I: MaybeDefined<UserIndex>,
{
    pub fn set_address(self, address: Addr) -> TestAccount<I, Defined<Addr>> {
        TestAccount {
            user_index: self.user_index,
            address: Defined::new(address),
            nonce: self.nonce,
            keys: self.keys,
            sign_with: self.sign_with,
        }
    }

    pub fn predict_address(
        self,
        factory: Addr,
        seed: u32,
        code_hash: Hash256,
        new_user_salt: bool,
    ) -> TestAccount<I, Defined<Addr>> {
        let (_, key) = &self.keys[&self.sign_with];
        let salt = if new_user_salt {
            NewUserSalt {
                key: *key,
                key_hash: self.sign_with,
                seed,
            }
            .to_bytes()
        } else {
            todo!("implement address prediction for not new users");
        };

        let address = Addr::derive(factory, code_hash, &salt);

        self.set_address(address)
    }
}

impl<I, A> TestAccount<I, A>
where
    I: MaybeDefined<UserIndex>,
    A: MaybeDefined<Addr>,
{
    pub fn first_sk(&self) -> &SigningKey {
        &self.keys.iter().next().unwrap().1.0
    }

    pub fn first_key(&self) -> Key {
        self.keys.iter().next().unwrap().1.1
    }

    pub fn first_key_hash(&self) -> Hash256 {
        *self.keys.keys().next().unwrap()
    }

    pub fn keys(&self) -> &BTreeMap<Hash256, (SigningKey, Key)> {
        &self.keys
    }

    pub fn sign_with(&self) -> Hash256 {
        self.sign_with
    }

    pub fn set_nonce(mut self, nonce: Nonce) -> Self {
        self.nonce = nonce;
        self
    }

    // TODO: currently only support sign data that use SHA256 hasher.
    pub fn sign_arbitrary<D>(&self, data: D) -> StdResult<Signature>
    where
        D: SignData<Hasher = Sha256>,
        StdError: From<D::Error>,
    {
        let bytes = data.to_sign_data()?;
        let standard_credential = self.create_standard_credential(bytes.into());

        Ok(standard_credential.signature)
    }

    /// Note: This function expects the _hashed_ sign data.
    pub fn create_standard_credential(&self, sign_data: [u8; 32]) -> StandardCredential {
        let sk = &self.keys.get(&self.sign_with).unwrap().0;
        let signature = create_signature(sk, sign_data);

        StandardCredential {
            key_hash: self.sign_with,
            signature: Signature::Secp256k1(signature),
        }
    }
}

impl<A> TestAccount<Defined<UserIndex>, A>
where
    A: MaybeDefined<Addr>,
{
    pub fn user_index(&self) -> UserIndex {
        self.user_index.into_inner()
    }

    pub fn metadata(&self, chain_id: &str, nonce: u32, expiry: Option<Duration>) -> Metadata {
        Metadata {
            user_index: self.user_index.into_inner(),
            chain_id: chain_id.to_string(),
            expiry,
            nonce,
        }
    }

    pub fn sign_transaction_with_nonce(
        &self,
        sender: Addr,
        msgs: NonEmpty<Vec<Message>>,
        chain_id: &str,
        gas_limit: u64,
        nonce: u32,
        expiry: Option<Duration>,
    ) -> StdResult<(Metadata, Credential)> {
        let data = self.metadata(chain_id, nonce, expiry);

        let sign_doc = SignDoc {
            sender,
            gas_limit,
            messages: msgs.clone(),
            data: data.clone(),
        };

        let sign_data = sign_doc.to_sign_data()?;
        let standard_credential = self.create_standard_credential(sign_data.into());

        Ok((data, Credential::Standard(standard_credential)))
    }
}

impl<A> TestAccount<Undefined<UserIndex>, A>
where
    A: MaybeDefined<Addr>,
{
    /// Register the user
    pub async fn register_user<DB, VM, PP, ID>(
        &self,
        test_suite: &mut TestSuite<DB, VM, PP, ID>,
        factory: Addr,
        funds: Coins,
    ) where
        PP: ProposalPreparer,
        DB: Db,
        VM: Vm + Clone + Send + Sync + 'static,
        ID: Indexer,
        AppError: From<PP::Error> + From<DB::Error> + From<VM::Error>,
    {
        let chain_id = test_suite.chain_id.clone();

        test_suite
            .execute(
                &mut Factory::new(factory),
                factory,
                &account_factory::ExecuteMsg::RegisterUser {
                    seed: 0,
                    key: self.first_key(),
                    key_hash: self.first_key_hash(),
                    signature: self
                        .sign_arbitrary(RegisterUserData {
                            chain_id,
                            key: self.first_key(),
                            key_hash: self.first_key_hash(),
                            seed: 0,
                            referrer: None,
                        })
                        .unwrap(),
                    referrer: None,
                },
                funds,
            )
            .await
            .should_succeed();
    }
}

impl<A> TestAccount<Defined<UserIndex>, A>
where
    A: MaybeDefined<Addr>,
    Self: Signer + Send + Sync,
{
    /// Register a new account with the user index and key of this account and returns a new
    /// `TestAccount` with the new account's address.
    pub async fn register_new_account<DB, VM, PP, ID>(
        &mut self,
        test_suite: &mut TestSuite<DB, VM, PP, ID>,
        factory: Addr,
        funds: Coins,
    ) -> StdResult<TestAccount>
    where
        PP: ProposalPreparer,
        DB: Db,
        VM: Vm + Clone + Send + Sync + 'static,
        ID: Indexer,
        AppError: From<PP::Error> + From<DB::Error> + From<VM::Error>,
    {
        // Derive the new accounts address.
        let index = test_suite
            .query_wasm_smart(factory, QueryNextAccountIndexRequest {})
            .unwrap();

        let code_hash = test_suite
            .query_wasm_smart(factory, QueryCodeHashRequest {})
            .should_succeed();

        let address = Addr::derive(factory, code_hash, Salt { index }.into_bytes().as_slice());

        // Create a new account
        test_suite
            .execute(
                &mut *self,
                factory,
                &account_factory::ExecuteMsg::RegisterAccount {},
                funds,
            )
            .await
            .should_succeed();

        Ok(TestAccount {
            user_index: self.user_index,
            nonce: 0,
            address: Defined::new(address),
            keys: self.keys.clone(),
            sign_with: self.sign_with,
        })
    }
}

impl<I> Addressable for TestAccount<I, Defined<Addr>>
where
    I: MaybeDefined<UserIndex>,
{
    fn address(&self) -> Addr {
        *self.address.inner()
    }
}

impl Signer for TestAccount {
    fn unsigned_transaction(
        &self,
        msgs: NonEmpty<Vec<Message>>,
        chain_id: &str,
    ) -> StdResult<UnsignedTx> {
        Ok(UnsignedTx {
            sender: self.address(),
            msgs,
            data: self.metadata(chain_id, self.nonce, None).to_json_value()?,
        })
    }

    fn sign_transaction(
        &mut self,
        msgs: NonEmpty<Vec<Message>>,
        chain_id: &str,
        gas_limit: u64,
    ) -> StdResult<Tx> {
        let (data, credential) = self.sign_transaction_with_nonce(
            self.address(),
            msgs.clone(),
            chain_id,
            gas_limit,
            self.nonce,
            None,
        )?;

        // Increment the internally tracked nonce.
        self.nonce += 1;

        Ok(Tx {
            sender: self.address(),
            gas_limit,
            msgs,
            data: data.to_json_value()?,
            credential: credential.to_json_value()?,
        })
    }
}

#[async_trait::async_trait]
impl SequencedSigner for TestAccount {
    async fn query_nonce<C>(&self, client: &C) -> anyhow::Result<Nonce>
    where
        C: QueryClient,
        anyhow::Error: From<C::Error>,
    {
        // If the account hasn't sent any transaction yet, use 0 as nonce.
        // Otherwise, use the latest seen nonce + 1.
        let nonce = client
            .query_wasm_smart(
                self.address.into_inner(),
                account::QuerySeenNoncesRequest {},
            )
            .await?
            .last()
            .map(|newest_nonce| newest_nonce + 1)
            .unwrap_or(0);

        Ok(nonce)
    }

    async fn update_nonce<C>(&mut self, client: &C) -> anyhow::Result<()>
    where
        C: QueryClient,
        anyhow::Error: From<C::Error>,
    {
        self.nonce = self.query_nonce(client).await?;

        Ok(())
    }
}

// ---------------------------------- factory ----------------------------------

pub struct Factory {
    address: Addr,
}

impl Factory {
    pub fn new(address: Addr) -> Self {
        Self { address }
    }
}

impl Addressable for Factory {
    fn address(&self) -> Addr {
        self.address
    }
}

impl Signer for Factory {
    fn unsigned_transaction(
        &self,
        msgs: NonEmpty<Vec<Message>>,
        _chain_id: &str,
    ) -> StdResult<UnsignedTx> {
        Ok(UnsignedTx {
            sender: self.address(),
            msgs,
            data: Json::null(),
        })
    }

    fn sign_transaction(
        &mut self,
        msgs: NonEmpty<Vec<Message>>,
        _chain_id: &str,
        gas_limit: u64,
    ) -> StdResult<Tx> {
        Ok(Tx {
            sender: self.address,
            gas_limit,
            msgs,
            data: Json::null(),
            credential: Json::null(),
        })
    }
}

// ----------------------------------- multi -----------------------------------

pub struct Multi<'a> {
    address: Addr,
    signer: Option<&'a TestAccount>,
    nonce: u32,
}

impl Multi<'_> {
    pub fn new(address: Addr) -> Self {
        Self {
            address,
            signer: None,
            nonce: 0,
        }
    }
}

impl<'a> Multi<'a> {
    pub fn with_signer(&mut self, signer: &'a TestAccount) -> &mut Self {
        self.signer = Some(signer);
        self
    }

    pub fn with_nonce(&mut self, nonce: u32) -> &mut Self {
        self.nonce = nonce;
        self
    }
}

impl Addressable for Multi<'_> {
    fn address(&self) -> Addr {
        self.address
    }
}

impl Signer for Multi<'_> {
    fn unsigned_transaction(
        &self,
        msgs: NonEmpty<Vec<Message>>,
        chain_id: &str,
    ) -> StdResult<UnsignedTx> {
        self.signer
            .expect("[Multi]: signer not set") // TODO: use typed state pattern to avoid runtime error
            .unsigned_transaction(msgs, chain_id)
    }

    fn sign_transaction(
        &mut self,
        msgs: NonEmpty<Vec<Message>>,
        chain_id: &str,
        gas_limit: u64,
    ) -> StdResult<Tx> {
        let (data, credential) = self
            .signer
            .expect("[Multi]: signer not set")
            .sign_transaction_with_nonce(
                self.address(),
                msgs.clone(),
                chain_id,
                gas_limit,
                self.nonce,
                None,
            )?;

        // Increment the internally tracked nonce.
        self.nonce += 1;

        Ok(Tx {
            sender: self.address,
            gas_limit,
            msgs,
            data: data.to_json_value()?,
            credential: credential.to_json_value()?,
        })
    }
}
