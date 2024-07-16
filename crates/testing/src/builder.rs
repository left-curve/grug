use {
    crate::{setup_tracing_subscriber, TestAccount, TestAccounts, TestSuite, TestVm},
    anyhow::ensure,
    grug_account::PublicKey,
    grug_app::AppError,
    grug_types::{
        hash, Addr, Binary, BlockInfo, Coins, Config, GenesisState, Hash, Message, NumberConst,
        Permission, Permissions, Timestamp, Uint64, GENESIS_BLOCK_HASH, GENESIS_SENDER,
    },
    grug_vm_rust::RustVm,
    std::{
        collections::{BTreeMap, BTreeSet},
        time::{Duration, SystemTime, UNIX_EPOCH},
    },
    tracing::Level,
};

pub const DEFAULT_TRACING_LEVEL: Level = Level::DEBUG;
pub const DEFAULT_CHAIN_ID: &str = "dev-1";
pub const DEFAULT_BLOCK_TIME: Duration = Duration::from_millis(250);
pub const DEFAULT_BANK_SALT: &[u8] = b"bank";

pub struct TestBuilder<VM: TestVm = RustVm> {
    vm: VM,
    tracing_level: Option<Level>,
    chain_id: Option<String>,
    genesis_time: Option<SystemTime>,
    block_time: Option<Duration>,
    // TODO: let user customize the codes and instantiate messages of bank and account
    account_code: Binary,
    account_code_hash: Hash,
    accounts: TestAccounts,
    bank_code: Binary,
    bank_code_hash: Hash,
    balances: BTreeMap<Addr, Coins>,
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
    VM: TestVm + Clone,
    AppError: From<VM::Error>,
{
    pub fn new_with_vm(vm: VM) -> Self {
        let account_code = VM::default_account_code();
        let account_code_hash = hash(&account_code);

        let bank_code = VM::default_bank_code();
        let bank_code_hash = hash(&bank_code);

        Self {
            vm,
            tracing_level: None,
            chain_id: None,
            genesis_time: None,
            block_time: None,
            account_code,
            account_code_hash,
            accounts: TestAccounts::new(),
            bank_code,
            bank_code_hash,
            balances: BTreeMap::new(),
        }
    }

    pub fn set_tracing_level(mut self, level: Level) -> Self {
        self.tracing_level = Some(level);
        self
    }

    pub fn set_chain_id(mut self, chain_id: impl ToString) -> Self {
        self.chain_id = Some(chain_id.to_string());
        self
    }

    pub fn set_genesis_time(mut self, genesis_time: SystemTime) -> Self {
        self.genesis_time = Some(genesis_time);
        self
    }

    pub fn add_account(mut self, name: &'static str, balances: Coins) -> anyhow::Result<Self> {
        ensure!(
            !self.accounts.contains_key(name),
            "account with name {name} already exists"
        );

        // Generate a random new account
        let account = TestAccount::new_random(&self.account_code_hash, name.as_bytes());

        // Save account and balances
        if !balances.is_empty() {
            self.balances.insert(account.address.clone(), balances);
        }
        self.accounts.insert(name, account);

        Ok(self)
    }

    pub fn build(self) -> anyhow::Result<(TestSuite<VM>, TestAccounts)> {
        let tracing_level = self.tracing_level.unwrap_or(DEFAULT_TRACING_LEVEL);
        setup_tracing_subscriber(tracing_level);

        let block_time = self.block_time.unwrap_or(DEFAULT_BLOCK_TIME);

        let chain_id = self
            .chain_id
            .unwrap_or_else(|| DEFAULT_CHAIN_ID.to_string());

        // Use the current system time as genesis time, if unspecified.
        let genesis_time = self
            .genesis_time
            .unwrap_or_else(SystemTime::now)
            .duration_since(UNIX_EPOCH)?
            .as_nanos();

        let genesis_block = BlockInfo {
            height: Uint64::ZERO,
            timestamp: Timestamp::from_nanos(genesis_time),
            hash: GENESIS_BLOCK_HASH,
        };

        // Upload account and bank codes, instantiate bank contract.
        let mut msgs = vec![
            Message::upload(self.account_code),
            Message::upload(self.bank_code),
            Message::instantiate(
                self.bank_code_hash.clone(),
                &grug_bank::InstantiateMsg {
                    initial_balances: self.balances,
                },
                DEFAULT_BANK_SALT,
                Coins::new_empty(),
                None,
            )?,
        ];

        // Instantiate accounts
        for (name, account) in &self.accounts {
            msgs.push(Message::instantiate(
                self.account_code_hash.clone(),
                &grug_account::InstantiateMsg {
                    public_key: PublicKey::Secp256k1(account.pk.clone()),
                },
                name.to_string(),
                Coins::new_empty(),
                Some(account.address.clone()),
            )?);
        }

        // Create the app config
        let bank = Addr::compute(&GENESIS_SENDER, &self.bank_code_hash, DEFAULT_BANK_SALT);
        let config = Config {
            // TODO: allow user to set owner
            owner: None,
            bank,
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
            pinned_hashes: BTreeSet::new(),
        };

        let genesis_state = GenesisState { config, msgs };
        let suite = TestSuite::create(self.vm, chain_id, block_time, genesis_block, genesis_state)?;

        Ok((suite, self.accounts))
    }
}
