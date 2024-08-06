use {
    crate::{tracing::setup_tracing_subscriber, TestAccount, TestAccounts, TestSuite, TestVm},
    anyhow::{anyhow, ensure},
    grug_account::PublicKey,
    grug_app::AppError,
    grug_types::{
        hash256, Addr, Binary, BlockInfo, Coins, Config, Defined, Duration, GenesisState,
        MaybeDefined, Message, Permission, Permissions, Timestamp, Udec128, Undefined,
        GENESIS_BLOCK_HASH, GENESIS_BLOCK_HEIGHT, GENESIS_SENDER,
    },
    grug_vm_rust::RustVm,
    serde::Serialize,
    std::{
        collections::BTreeMap,
        str::FromStr,
        time::{SystemTime, UNIX_EPOCH},
    },
    tracing::Level,
};

const DEFAULT_TRACING_LEVEL: Level = Level::DEBUG;
const DEFAULT_CHAIN_ID: &str = "dev-1";
const DEFAULT_BLOCK_TIME: Duration = Duration::from_millis(250);
const DEFAULT_BANK_SALT: &[u8] = b"bank";
const DEFAULT_TAXMAN_SALT: &[u8] = b"taxman";
const DEFAULT_FEE_DENOM: &str = "ugrug";
const DEFAULT_FEE_RATE: &str = "0";

// If the user wishes to use a custom code for account, bank, or taxman, they
// must provide both the binary code, as well as a function for creating the
// instantiate message.
struct CodeOption<B> {
    code: Binary,
    msg_builder: B,
}

pub struct TestBuilder<
    VM = RustVm,
    M1 = grug_account::InstantiateMsg,
    M2 = grug_bank::InstantiateMsg,
    M3 = grug_taxman::InstantiateMsg,
    TA = Undefined<TestAccounts>,
> {
    vm: VM,
    // Basic configs
    tracing_level: Option<Level>,
    chain_id: Option<String>,
    genesis_time: Option<Timestamp>,
    block_time: Option<Duration>,
    owner: Option<Addr>,
    // Accounts
    account_opt: CodeOption<Box<dyn Fn(Binary) -> M1>>,
    accounts: TA,
    // Bank
    bank_opt: CodeOption<Box<dyn FnOnce(BTreeMap<Addr, Coins>) -> M2>>,
    balances: BTreeMap<Addr, Coins>,
    // Taxman
    taxman_opt: CodeOption<Box<dyn FnOnce(String, Udec128) -> M3>>,
    fee_denom: Option<String>,
    fee_rate: Option<Udec128>,
}

// Clippy incorrectly thinks we can derive `Default` here, which we can't.
#[allow(clippy::new_without_default)]
impl TestBuilder<RustVm> {
    pub fn new() -> Self {
        Self::new_with_vm(RustVm::new())
    }
}

impl<VM> TestBuilder<VM>
where
    VM: TestVm,
{
    pub fn new_with_vm(vm: VM) -> Self {
        Self {
            account_opt: CodeOption {
                code: VM::default_account_code(),
                msg_builder: Box::new(|pk| grug_account::InstantiateMsg {
                    public_key: PublicKey::Secp256k1(pk),
                }),
            },
            bank_opt: CodeOption {
                code: VM::default_bank_code(),
                msg_builder: Box::new(|initial_balances| grug_bank::InstantiateMsg {
                    initial_balances,
                }),
            },
            taxman_opt: CodeOption {
                code: VM::default_taxman_code(),
                msg_builder: Box::new(|fee_denom, fee_rate| grug_taxman::InstantiateMsg {
                    config: grug_taxman::Config {
                        fee_denom,
                        fee_rate,
                    },
                }),
            },
            vm,
            tracing_level: Some(DEFAULT_TRACING_LEVEL),
            chain_id: None,
            genesis_time: None,
            block_time: None,
            owner: None,
            accounts: Undefined::default(),
            balances: BTreeMap::new(),
            fee_denom: None,
            fee_rate: None,
        }
    }
}

impl<VM, M1, M2, M3, TA> TestBuilder<VM, M1, M2, M3, TA>
where
    M1: Serialize,
    M2: Serialize,
    M3: Serialize,
    TA: MaybeDefined<Inner = TestAccounts>,
    VM: TestVm + Clone,
    AppError: From<VM::Error>,
{
    // Setting this to `None` means no tracing.
    pub fn set_tracing_level(mut self, level: Option<Level>) -> Self {
        self.tracing_level = level;
        self
    }

    pub fn set_chain_id<T>(mut self, chain_id: T) -> Self
    where
        T: ToString,
    {
        self.chain_id = Some(chain_id.to_string());
        self
    }

    pub fn set_genesis_time(mut self, genesis_time: Timestamp) -> Self {
        self.genesis_time = Some(genesis_time);
        self
    }

    pub fn set_block_time(mut self, block_time: Duration) -> Self {
        self.block_time = Some(block_time);
        self
    }

    pub fn set_fee_denom<T>(mut self, fee_denom: T) -> Self
    where
        T: ToString,
    {
        self.fee_denom = Some(fee_denom.to_string());
        self
    }

    pub fn set_fee_rate(mut self, fee_rate: Udec128) -> Self {
        self.fee_rate = Some(fee_rate);
        self
    }

    /// Use a custom code for the bank instead the default implementation
    /// provided by the Grug test suite.
    ///
    /// Must provide a builder function that generates the bank's instantiate
    /// message given the initial account balances.
    ///
    /// E.g.
    ///
    /// ```rust
    /// use grug_testing::TestBuilder;
    /// use grug_vm_rust::ContractBuilder;
    /// use grug_types::Coins;
    ///
    /// let code = ContractBuilder::new(Box::new(grug_bank::instantiate))
    ///     .with_bank_execute(Box::new(grug_bank::bank_execute))
    ///     .with_bank_query(Box::new(grug_bank::bank_query))
    ///     .build();
    ///
    /// let (suite, accounts) = TestBuilder::new()
    ///     .add_account("owner", Coins::new())
    ///     .unwrap()
    ///     .set_bank_code(
    ///         code,
    ///         |initial_balances| grug_bank::InstantiateMsg { initial_balances },
    ///     )
    ///     .build()
    ///     .unwrap();
    /// ```
    pub fn set_bank_code<T, F, M2A>(
        self,
        code: T,
        msg_builder: F,
    ) -> TestBuilder<VM, M1, M2A, M3, TA>
    where
        T: Into<Binary>,
        F: FnOnce(BTreeMap<Addr, Coins>) -> M2A + 'static,
    {
        TestBuilder {
            vm: self.vm,
            tracing_level: self.tracing_level,
            chain_id: self.chain_id,
            genesis_time: self.genesis_time,
            block_time: self.block_time,
            owner: self.owner,
            account_opt: self.account_opt,
            accounts: self.accounts,
            bank_opt: CodeOption {
                code: code.into(),
                msg_builder: Box::new(msg_builder),
            },
            balances: self.balances,
            taxman_opt: self.taxman_opt,
            fee_denom: self.fee_denom,
            fee_rate: self.fee_rate,
        }
    }

    /// Use a custom code for the taxman instead the default implementation
    /// provided by the Grug test suite.
    ///
    /// Must provide a builder function that generates the taxman's instantiate
    /// message given the fee denom and fee rate.
    ///
    /// E.g.
    ///
    /// ```rust
    /// use grug_testing::TestBuilder;
    /// use grug_vm_rust::ContractBuilder;
    /// use grug_types::Coins;
    ///
    /// let code = ContractBuilder::new(Box::new(grug_taxman::instantiate))
    ///     .with_withhold_fee(Box::new(grug_taxman::withhold_fee))
    ///     .with_finalize_fee(Box::new(grug_taxman::finalize_fee))
    ///     .build();
    ///
    /// let (suite, accounts) = TestBuilder::new()
    ///     .add_account("owner", Coins::new())
    ///     .unwrap()
    ///     .set_taxman_code(
    ///         code,
    ///         |fee_denom, fee_rate| grug_taxman::InstantiateMsg {
    ///             config: grug_taxman::Config { fee_denom, fee_rate },
    ///         },
    ///     )
    ///     .build()
    ///     .unwrap();
    /// ```
    pub fn set_taxman_code<T, F, M3A>(
        self,
        code: T,
        msg_builder: F,
    ) -> TestBuilder<VM, M1, M2, M3A, TA>
    where
        T: Into<Binary>,
        F: FnOnce(String, Udec128) -> M3A + 'static,
    {
        TestBuilder {
            vm: self.vm,
            tracing_level: self.tracing_level,
            chain_id: self.chain_id,
            genesis_time: self.genesis_time,
            block_time: self.block_time,
            owner: self.owner,
            account_opt: self.account_opt,
            accounts: self.accounts,
            bank_opt: self.bank_opt,
            balances: self.balances,
            taxman_opt: CodeOption {
                code: code.into(),
                msg_builder: Box::new(msg_builder),
            },
            fee_denom: self.fee_denom,
            fee_rate: self.fee_rate,
        }
    }

    pub fn add_account<C>(
        mut self,
        name: &'static str,
        balances: C,
    ) -> anyhow::Result<TestBuilder<VM, M1, M2, M3, Defined<TestAccounts>>>
    where
        C: TryInto<Coins>,
        anyhow::Error: From<C::Error>,
    {
        let mut accounts = self.accounts.maybe_inner().unwrap_or_default();
        ensure!(
            !accounts.contains_key(name),
            "account with name {name} already exists"
        );

        // Generate a random new account
        let account = TestAccount::new_random(&hash256(&self.account_opt.code), name.as_bytes());

        // Save account and balances
        let balances = balances.try_into()?;
        if !balances.is_empty() {
            self.balances.insert(account.address.clone(), balances);
        }
        accounts.insert(name, account);

        Ok(TestBuilder {
            vm: self.vm,
            tracing_level: self.tracing_level,
            chain_id: self.chain_id,
            genesis_time: self.genesis_time,
            block_time: self.block_time,
            owner: self.owner,
            account_opt: self.account_opt,
            accounts: Defined::new(accounts),
            bank_opt: self.bank_opt,
            balances: self.balances,
            taxman_opt: self.taxman_opt,
            fee_denom: self.fee_denom,
            fee_rate: self.fee_rate,
        })
    }
}

impl<VM, M1, M2, M3> TestBuilder<VM, M1, M2, M3, Undefined<TestAccounts>>
where
    M1: Serialize,
    M2: Serialize,
    M3: Serialize,
    VM: TestVm + Clone,
    AppError: From<VM::Error>,
{
    /// Use a custom code for the account instead the default implementation
    /// provided by the Grug test suite.
    ///
    /// Must provide a builder function that generates an account's instantiate
    /// message given an Secp256k1 public key.
    ///
    /// **Note:** `set_account_code` can only be called before any `add_account`
    /// call, otherwise the derived addresses won't be correct.
    ///
    /// E.g.
    ///
    /// ```rust
    /// use grug_testing::TestBuilder;
    /// use grug_vm_rust::ContractBuilder;
    /// use grug_types::Coins;
    ///
    /// let code = ContractBuilder::new(Box::new(grug_account::instantiate))
    ///     .with_authenticate(Box::new(grug_account::authenticate))
    ///     .build();
    ///
    /// let (suite, accounts) = TestBuilder::new()
    ///     .set_account_code(
    ///         code,
    ///         |pk| grug_account::InstantiateMsg {
    ///             public_key: grug_account::PublicKey::Secp256k1(pk),
    ///         },
    ///     )
    ///     .unwrap()
    ///     .add_account("owner", Coins::new())
    ///     .unwrap()
    ///     .build()
    ///     .unwrap();
    /// ```
    pub fn set_account_code<T, F, M1A>(
        self,
        code: T,
        msg_builder: F,
    ) -> anyhow::Result<TestBuilder<VM, M1A, M2, M3, Undefined<TestAccounts>>>
    where
        T: Into<Binary>,
        F: Fn(Binary) -> M1A + 'static,
    {
        Ok(TestBuilder {
            vm: self.vm,
            tracing_level: self.tracing_level,
            chain_id: self.chain_id,
            genesis_time: self.genesis_time,
            block_time: self.block_time,
            owner: self.owner,
            account_opt: CodeOption {
                code: code.into(),
                msg_builder: Box::new(msg_builder),
            },
            accounts: self.accounts,
            bank_opt: self.bank_opt,
            balances: self.balances,
            taxman_opt: self.taxman_opt,
            fee_denom: self.fee_denom,
            fee_rate: self.fee_rate,
        })
    }
}

// TSB where at least one account is set
impl<VM, M1, M2, M3> TestBuilder<VM, M1, M2, M3, Defined<TestAccounts>>
where
    M1: Serialize,
    M2: Serialize,
    M3: Serialize,
    VM: TestVm + Clone,
    AppError: From<VM::Error>,
{
    /// **Note:** `set_owner` can only called if `add_account` has been already
    /// called at least once.
    pub fn set_owner(mut self, name: &'static str) -> anyhow::Result<Self> {
        let owner =
            self.accounts.inner().get(name).ok_or_else(|| {
                anyhow!("failed to set owner: can't find account with name `{name}`")
            })?;

        self.owner = Some(owner.address.clone());

        Ok(self)
    }

    pub fn build(self) -> anyhow::Result<(TestSuite<VM>, TestAccounts)> {
        if let Some(tracing_level) = self.tracing_level {
            setup_tracing_subscriber(tracing_level);
        }

        let block_time = self.block_time.unwrap_or(DEFAULT_BLOCK_TIME);

        let chain_id = self
            .chain_id
            .unwrap_or_else(|| DEFAULT_CHAIN_ID.to_string());

        let fee_denom = self
            .fee_denom
            .unwrap_or_else(|| DEFAULT_FEE_DENOM.to_string());

        let fee_rate = self
            .fee_rate
            .unwrap_or_else(|| Udec128::from_str(DEFAULT_FEE_RATE).unwrap());

        // Use the current system time as genesis time, if unspecified.
        let genesis_time = match self.genesis_time {
            Some(time) => time,
            None => Timestamp::from_nanos(SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()),
        };

        let genesis_block = BlockInfo {
            hash: GENESIS_BLOCK_HASH,
            height: GENESIS_BLOCK_HEIGHT,
            timestamp: genesis_time,
        };

        // Upload account, bank, and taxman codes,
        // instantiate bank and taxman contracts.
        let mut msgs = vec![
            Message::upload(self.account_opt.code.clone()),
            Message::upload(self.bank_opt.code.clone()),
            Message::upload(self.taxman_opt.code.clone()),
            Message::instantiate(
                hash256(&self.bank_opt.code),
                &(self.bank_opt.msg_builder)(self.balances),
                DEFAULT_BANK_SALT,
                Coins::new(),
                None,
            )?,
            Message::instantiate(
                hash256(&self.taxman_opt.code),
                &(self.taxman_opt.msg_builder)(fee_denom, fee_rate),
                DEFAULT_TAXMAN_SALT,
                Coins::new(),
                None,
            )?,
        ];

        // Instantiate accounts
        for (name, account) in self.accounts.inner() {
            msgs.push(Message::instantiate(
                hash256(&self.account_opt.code),
                &(self.account_opt.msg_builder)(account.pk.clone()),
                name.to_string(),
                Coins::new(),
                Some(account.address.clone()),
            )?);
        }

        // Predict bank contract address
        let bank = Addr::compute(
            &GENESIS_SENDER,
            &hash256(&self.bank_opt.code),
            DEFAULT_BANK_SALT,
        );

        // Prefict taxman contract address
        let taxman = Addr::compute(
            &GENESIS_SENDER,
            &hash256(&self.taxman_opt.code),
            DEFAULT_TAXMAN_SALT,
        );

        // Create the app config
        let config = Config {
            owner: self.owner,
            bank,
            taxman,
            cronjobs: BTreeMap::new(),
            permissions: Permissions {
                upload: Permission::Everybody,
                instantiate: Permission::Everybody,
            },
        };

        let genesis_state = GenesisState { config, msgs };
        let suite = TestSuite::create(self.vm, chain_id, block_time, genesis_block, genesis_state)?;

        Ok((suite, self.accounts.into_inner()))
    }
}
