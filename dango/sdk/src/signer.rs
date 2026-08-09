use {
    crate::Secret,
    anyhow::anyhow,
    dango_primitives::{
        Addr, Addressable, Defined, JsonSerExt, MaybeDefined, Message, NonEmpty, QueryClient,
        QueryClientExt, Signer, StdError, StdResult, Tx, Undefined, UnsignedTx,
    },
    dango_types::{
        account,
        account_factory::{self, UserIndex, UserIndexOrName},
        auth::{Credential, Metadata, Nonce, SignDoc, StandardCredential},
        config::AppConfig,
        signer::SequencedSigner,
    },
};

pub const DEFAULT_DERIVATION_PATH: &str = "m/44'/60'/0'/0/0";

/// Utility for signing transactions in the format by Dango's single-signature
/// accounts.
#[derive(Debug)]
pub struct SingleSigner<S, I = Defined<UserIndex>, N = Defined<Nonce>>
where
    S: Secret,
    I: MaybeDefined<UserIndex>,
    N: MaybeDefined<Nonce>,
{
    pub address: Addr,
    pub secret: S,
    pub user_index: I,
    pub nonce: N,
}

impl<S, I, N> SingleSigner<S, I, N>
where
    S: Secret,
    I: MaybeDefined<UserIndex>,
    N: MaybeDefined<Nonce>,
{
    pub async fn query_user_index<C>(&self, client: &C) -> anyhow::Result<UserIndex>
    where
        C: QueryClient,
        anyhow::Error: From<C::Error>,
    {
        let account_factory = client
            .query_app_config::<dango_types::config::AppConfig>()
            .await?
            .addresses
            .account_factory;

        Ok(client
            .query_wasm_smart(
                account_factory,
                account_factory::QueryAccountRequest {
                    address: self.address,
                },
            )
            .await?
            .owner)
    }

    pub async fn query_next_nonce<C>(&self, client: &C) -> anyhow::Result<Nonce>
    where
        C: QueryClient,
        anyhow::Error: From<C::Error>,
    {
        // If the account hasn't sent any transaction yet, use 0 as nonce.
        // Otherwise, use the latest seen nonce + 1.
        let nonce = client
            .query_wasm_smart(self.address, account::QuerySeenNoncesRequest {})
            .await?
            .last()
            .map(|newest_nonce| newest_nonce + 1)
            .unwrap_or(0);

        Ok(nonce)
    }
}

impl<S> SingleSigner<S, Undefined<UserIndex>, Undefined<Nonce>>
where
    S: Secret,
{
    /// Create a new `SingleSigner` with the given secret key.
    pub fn new(address: Addr, secret: S) -> Self {
        Self {
            address,
            secret,
            user_index: Undefined::new(),
            nonce: Undefined::new(),
        }
    }
}

impl<S> SingleSigner<S, Defined<UserIndex>, Undefined<Nonce>>
where
    S: Secret,
{
    /// Create a new `SingleSigner` with the given secret key, using the first
    /// user index and account associated with this key.
    pub async fn new_first_address_available<C>(
        client: &C,
        secret: S,
        cfg: Option<&AppConfig>,
    ) -> anyhow::Result<Self>
    where
        C: QueryClient,
        anyhow::Error: From<C::Error>,
    {
        let factory_addr = match cfg {
            Some(cfg) => cfg.addresses.account_factory,
            None => {
                client
                    .query_app_config::<AppConfig>()
                    .await?
                    .addresses
                    .account_factory
            },
        };

        let key_hash = secret.key_hash();

        let user_index = client
            .query_wasm_smart(
                factory_addr,
                account_factory::QueryForgotUsernameRequest {
                    key_hash,
                    start_after: None,
                    limit: Some(1),
                },
            )
            .await?
            .first()
            .ok_or_else(|| anyhow!("no user index found for key hash {key_hash}"))?
            .index;

        let address = client
            .query_wasm_smart(
                factory_addr,
                account_factory::QueryUserRequest(UserIndexOrName::Index(user_index)),
            )
            .await?
            .master_account();

        Ok(SingleSigner {
            address,
            secret,
            user_index: Defined::new(user_index),
            nonce: Undefined::new(),
        })
    }
}

impl<S, N> SingleSigner<S, Undefined<UserIndex>, N>
where
    S: Secret,
    N: MaybeDefined<Nonce>,
{
    pub fn with_user_index(self, user_index: UserIndex) -> SingleSigner<S, Defined<UserIndex>, N> {
        SingleSigner {
            address: self.address,
            secret: self.secret,
            user_index: Defined::new(user_index),
            nonce: self.nonce,
        }
    }

    pub async fn with_query_user_index<C>(
        self,
        client: &C,
    ) -> anyhow::Result<SingleSigner<S, Defined<UserIndex>, N>>
    where
        C: QueryClient,
        anyhow::Error: From<C::Error>,
    {
        let user_index = self.query_user_index(client).await?;
        Ok(self.with_user_index(user_index))
    }
}

impl<S, I> SingleSigner<S, I, Undefined<Nonce>>
where
    S: Secret,
    I: MaybeDefined<UserIndex>,
{
    pub fn with_nonce(self, nonce: Nonce) -> SingleSigner<S, I, Defined<Nonce>> {
        SingleSigner {
            address: self.address,
            secret: self.secret,
            user_index: self.user_index,
            nonce: Defined::new(nonce),
        }
    }

    /// Fetch the next nonce and return a `SingleSigner` with the nonce set.
    pub async fn with_query_nonce<C>(
        self,
        client: &C,
    ) -> anyhow::Result<SingleSigner<S, I, Defined<Nonce>>>
    where
        C: QueryClient,
        anyhow::Error: From<C::Error>,
    {
        let nonce = self.query_next_nonce(client).await?;
        Ok(self.with_nonce(nonce))
    }
}

impl<S, I, N> Addressable for SingleSigner<S, I, N>
where
    S: Secret,
    I: MaybeDefined<UserIndex>,
    N: MaybeDefined<Nonce>,
{
    fn address(&self) -> Addr {
        self.address
    }
}

impl<S, N> SingleSigner<S, Defined<UserIndex>, N>
where
    S: Secret,
    N: MaybeDefined<Nonce>,
{
    pub fn user_index(&self) -> UserIndex {
        self.user_index.into_inner()
    }
}

impl<S, I> SingleSigner<S, I, Defined<Nonce>>
where
    S: Secret,
    I: MaybeDefined<UserIndex>,
{
    pub fn nonce(&self) -> Nonce {
        self.nonce.into_inner()
    }
}

impl<S> Signer for SingleSigner<S>
where
    S: Secret,
{
    fn unsigned_transaction(
        &self,
        msgs: NonEmpty<Vec<Message>>,
        chain_id: &str,
    ) -> StdResult<UnsignedTx> {
        Ok(UnsignedTx {
            sender: self.address,
            msgs,
            data: Metadata {
                chain_id: chain_id.to_string(),
                user_index: self.user_index(),
                nonce: self.nonce(),
                expiry: None, // TODO
            }
            .to_json_value()?,
        })
    }

    fn sign_transaction(
        &mut self,
        msgs: NonEmpty<Vec<Message>>, // TODO: the method should take a `LengthBounded`
        chain_id: &str,
        gas_limit: u64,
    ) -> StdResult<Tx> {
        let nonce = self.nonce();
        *self.nonce.inner_mut() += 1;

        let metadata = Metadata {
            user_index: self.user_index(),
            chain_id: chain_id.to_string(),
            nonce,
            expiry: None, // TODO
        };

        let sign_doc = SignDoc {
            gas_limit,
            sender: self.address,
            messages: msgs.clone(),
            data: metadata.clone(),
        };

        let credential = Credential::Standard(StandardCredential {
            key_hash: self.secret.key_hash(),
            signature: self
                .secret
                .sign_transaction(sign_doc)
                .map_err(|err| StdError::host(err.to_string()))?, // TODO: better handle this error
        });

        Ok(Tx {
            sender: self.address,
            gas_limit,
            msgs,
            data: metadata.to_json_value()?,
            credential: credential.to_json_value()?,
        })
    }
}

#[async_trait::async_trait]
impl<S> SequencedSigner for SingleSigner<S, Defined<Nonce>>
where
    S: Secret + Send + Sync,
{
    async fn query_nonce<C>(&self, client: &C) -> anyhow::Result<Nonce>
    where
        C: QueryClient,
        anyhow::Error: From<C::Error>,
    {
        self.query_next_nonce(client).await
    }

    async fn update_nonce<C>(&mut self, client: &C) -> anyhow::Result<()>
    where
        C: QueryClient,
        anyhow::Error: From<C::Error>,
    {
        let nonce = self.query_next_nonce(client).await?;

        self.nonce = Defined::new(nonce);

        Ok(())
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{Eip712, Secp256k1},
        dango_account_factory::USERS,
        dango_auth::{account::STATUS, authenticate_tx},
        dango_primitives::{
            AuthMode, Coins, MockContext, MockQuerier, MockStorage, ResultExt, btree_map,
        },
        dango_types::{
            account_factory::{User, Username},
            auth::AccountStatus,
            config::{AppAddresses, AppConfig},
        },
    };

    #[test]
    fn sign_secp256k1_transaction_works() {
        let user_index = 123;
        let address = Addr::mock(0);
        let nonce = 0;
        let account_factory = Addr::mock(1);

        let mut signer = SingleSigner::new(address, Secp256k1::new_random())
            .with_nonce(nonce)
            .with_user_index(user_index);

        let tx = signer
            .sign_transaction(
                NonEmpty::new_unchecked(vec![
                    Message::transfer(Addr::mock(2), Coins::one("uatom", 100).unwrap()).unwrap(),
                    Message::transfer(Addr::mock(3), Coins::one("uosmo", 500).unwrap()).unwrap(),
                ]),
                "dango-1",
                100_000_000,
            )
            .unwrap();

        let mut mock_storage = MockStorage::new();

        STATUS
            .save(&mut mock_storage, &AccountStatus::Active)
            .unwrap();

        let mock_querier = MockQuerier::new()
            .with_raw_contract_storage(account_factory, |storage| {
                let user = User {
                    index: user_index,
                    name: Username::default_for_index(user_index),
                    accounts: btree_map! { 0u32 => address },
                    keys: btree_map! { signer.secret.key_hash() => signer.secret.key() },
                };
                USERS.save(storage, user_index, &user).unwrap();
            })
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory,
                    // the other addresses don't matter
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap();

        let mut mock_ctx = MockContext::default()
            .with_chain_id("dango-1")
            .with_storage(mock_storage)
            .with_querier(mock_querier)
            .with_mode(AuthMode::Finalize);

        authenticate_tx(mock_ctx.as_auth(), tx, None).should_succeed();
    }

    #[test]
    fn sign_eip712_transaction_works() {
        let user_index = 234;
        let address = Addr::mock(0);
        let nonce = 0;
        let account_factory = Addr::mock(1);

        let mut signer = SingleSigner::new(address, Eip712::new_random())
            .with_nonce(nonce)
            .with_user_index(user_index);

        let tx = signer
            .sign_transaction(
                NonEmpty::new_unchecked(vec![
                    Message::transfer(Addr::mock(2), Coins::one("uatom", 100).unwrap()).unwrap(),
                    Message::transfer(Addr::mock(3), Coins::one("uosmo", 500).unwrap()).unwrap(),
                ]),
                "dango-1",
                100_000_000,
            )
            .unwrap();

        let mut mock_storage = MockStorage::new();

        STATUS
            .save(&mut mock_storage, &AccountStatus::Active)
            .unwrap();

        let mock_querier = MockQuerier::new()
            .with_raw_contract_storage(account_factory, |storage| {
                let user = User {
                    index: user_index,
                    name: Username::default_for_index(user_index),
                    accounts: btree_map! { 0u32 => address },
                    keys: btree_map! { signer.secret.key_hash() => signer.secret.key() },
                };
                USERS.save(storage, user_index, &user).unwrap();
            })
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory,
                    // the other addresses don't matter
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap();

        let mut mock_ctx = MockContext::default()
            .with_chain_id("dango-1")
            .with_storage(mock_storage)
            .with_querier(mock_querier)
            .with_mode(AuthMode::Finalize);

        authenticate_tx(mock_ctx.as_auth(), tx, None).should_succeed();
    }
}
