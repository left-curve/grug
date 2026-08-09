use {
    crate::{
        BalanceTracker, InstantiateOutcome, MakeBlockOutcome, UploadAndInstantiateOutcome,
        UploadOutcome,
    },
    dango_app::{
        App, AppError, AppResult, CONTRACT_NAMESPACE, Db, Indexer, NaiveProposalPreparer,
        NullIndexer, ProposalPreparer, StorageProvider, UpgradeHandler, Vm,
    },
    dango_backtrace::Backtraceable,
    dango_db_memory::MemDb,
    dango_genesis::Contracts,
    dango_indexer_hooked::HookedIndexer,
    dango_indexer_httpd::traits::QueryApp,
    dango_math::Uint128,
    dango_primitives::{
        Addr, Addressable, Binary, Block, BlockInfo, CheckTxOutcome, Coins, Config, Denom,
        Duration, GenesisState, Hash256, HashExt, JsonDeExt, JsonSerExt, Message, NonEmpty,
        Querier, QuerierExt, QuerierWrapper, Query, QueryResponse, QueryStatusResponse, Signer,
        StdError, StdResult, Tx, TxOutcome, UnsignedTx,
    },
    dango_pyth_client::PythClientCache,
    dango_vm_rust::RustVm,
    serde::ser::Serialize,
    std::{collections::BTreeMap, fmt::Debug},
};

type PythProposalPreparer = dango_proposal_preparer::ProposalPreparer<PythClientCache>;

pub type TestSuiteNaive = TestSuite<MemDb, RustVm, NaiveProposalPreparer, NullIndexer>;

pub type TestSuiteWithIndexer = TestSuite<MemDb, RustVm, PythProposalPreparer, HookedIndexer>;

pub type TestSuiteNaiveWithIndexer = TestSuite<MemDb, RustVm, NaiveProposalPreparer, HookedIndexer>;

pub struct TestSuite<DB = MemDb, VM = RustVm, PP = PythProposalPreparer, ID = NullIndexer>
where
    DB: Db,
    VM: Vm,
    PP: ProposalPreparer,
    ID: Indexer,
{
    pub app: App<DB, VM, PP, ID>,
    pub contracts: Contracts,
    /// The chain ID can be queries from the `app`, but we internally track it in
    /// the test suite, so we don't need to query it every time we need it.
    pub chain_id: String,
    /// Interally track the last finalized block.
    pub block: BlockInfo,
    /// Each time we make a new block, we set the new block's time as the
    /// previous block's time plus this value.
    pub block_time: Duration,
    /// Transaction gas limit to use if user doesn't specify one.
    pub default_gas_limit: u64,
    pub(crate) balances: BTreeMap<Addr, Coins>,
}

impl TestSuite {
    /// Create a new test suite.
    pub fn new(
        contracts: Contracts,
        chain_id: String,
        block_time: Duration,
        default_gas_limit: u64,
        genesis_block: BlockInfo,
        genesis_state: GenesisState,
    ) -> Self {
        Self::new_with_vm(
            RustVm::new(),
            contracts,
            chain_id,
            block_time,
            default_gas_limit,
            genesis_block,
            genesis_state,
        )
    }
}

impl<VM> TestSuite<MemDb, VM, PythProposalPreparer, NullIndexer>
where
    VM: Vm + Clone + Send + Sync + 'static,
    AppError: From<VM::Error>,
{
    /// Create a new test suite with `MemDb`, `PythProposalPreparer`, and the
    /// given VM.
    pub fn new_with_vm(
        vm: VM,
        contracts: Contracts,
        chain_id: String,
        block_time: Duration,
        default_gas_limit: u64,
        genesis_block: BlockInfo,
        genesis_state: GenesisState,
    ) -> Self {
        Self::new_with_db_vm_indexer_and_pp(
            MemDb::new(),
            vm,
            PythProposalPreparer::new_with_cache(),
            NullIndexer,
            None,
            contracts,
            chain_id,
            block_time,
            default_gas_limit,
            genesis_block,
            genesis_state,
        )
    }
}

impl<PP> TestSuite<MemDb, RustVm, PP, NullIndexer>
where
    PP: ProposalPreparer,
    AppError: From<PP::Error>,
{
    /// Create a new test suite with `MemDb`, `RustVm`, and the given proposal
    /// preparer.
    pub fn new_with_pp(
        pp: PP,
        contracts: Contracts,
        chain_id: String,
        block_time: Duration,
        default_gas_limit: u64,
        genesis_block: BlockInfo,
        genesis_state: GenesisState,
    ) -> Self {
        Self::new_with_db_vm_indexer_and_pp(
            MemDb::new(),
            RustVm::new(),
            pp,
            NullIndexer,
            None,
            contracts,
            chain_id,
            block_time,
            default_gas_limit,
            genesis_block,
            genesis_state,
        )
    }
}

impl<DB, VM, PP, ID> TestSuite<DB, VM, PP, ID>
where
    DB: Db,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    /// Create a new test suite with the given DB and VM.
    pub fn new_with_db_vm_indexer_and_pp(
        db: DB,
        vm: VM,
        pp: PP,
        mut id: ID,
        upgrade_handler: Option<UpgradeHandler<VM>>,
        contracts: Contracts,
        chain_id: String,
        block_time: Duration,
        default_gas_limit: u64,
        genesis_block: BlockInfo,
        genesis_state: GenesisState,
    ) -> Self {
        // This is doing the same order as in Dango.
        // 1. Calling `start` on the indexer

        let previous_block_height = if let 0 | 1 = genesis_block.height {
            None
        } else {
            Some(genesis_block.height - 1)
        };

        let state_storage = db
            .state_storage(previous_block_height)
            .unwrap_or_else(|err| {
                panic!(
                    "Fatal error while getting the state storage: {}",
                    err.to_string()
                );
            });

        // Since start is now async, we need to block on it
        // Use futures::executor::block_on which works in both sync and async contexts
        futures::executor::block_on(async { id.start(&state_storage).await }).unwrap_or_else(
            |err| {
                panic!("fatal error while running indexer start: {err}");
            },
        );

        // 2. Creating the app instance
        // Use `u64::MAX` as query gas limit so that there's practically no limit.
        let app = App::new(db, vm, pp, id, u64::MAX, upgrade_handler, "0.0.0"); // TODO: allow customizing the cargo version

        app.do_init_chain(chain_id.clone(), genesis_block, genesis_state)
            .unwrap_or_else(|err| {
                panic!("fatal error while initializing chain: {err}");
            });

        Self::new_with_app(
            app,
            contracts,
            chain_id,
            genesis_block,
            block_time,
            default_gas_limit,
        )
    }

    /// Create a new test suite with the given already initialized app instance.
    pub fn new_with_app(
        app: App<DB, VM, PP, ID>,
        contracts: Contracts,
        chain_id: String,
        last_finalized_block: BlockInfo,
        block_time: Duration,
        default_gas_limit: u64,
    ) -> Self {
        Self {
            app,
            contracts,
            chain_id,
            block: last_finalized_block,
            block_time,
            default_gas_limit,
            balances: Default::default(),
        }
    }

    /// Simulate the gas cost and event outputs of an unsigned transaction.
    pub fn simulate_tx(&self, unsigned_tx: UnsignedTx) -> TxOutcome {
        self.app.do_simulate(unsigned_tx).unwrap_or_else(|err| {
            panic!("fatal error while simulating tx: {err}");
        })
    }

    /// Perform ABCI `CheckTx` call of a transaction.
    pub fn check_tx(&self, tx: Tx) -> CheckTxOutcome {
        self.app
            .do_check_tx(tx)
            .unwrap_or_else(|err| panic!("fatal error while checking tx: {err}"))
    }

    /// Increase the chain's time by the given duration.
    pub async fn increase_time(&mut self, duration: Duration) {
        let old_block_time = self.block_time;
        self.block_time = duration;
        self.make_empty_block().await;
        self.block_time = old_block_time;
    }

    /// Make a new block without any transaction. Panic if any error happens.
    pub async fn make_empty_block(&mut self) -> MakeBlockOutcome {
        self.make_block(vec![]).await
    }

    /// Make a new block without any transaction.
    pub async fn try_make_empty_block(&mut self) -> AppResult<MakeBlockOutcome> {
        self.try_make_block(vec![]).await
    }

    /// Make a new block with the given transactions. Panic if any error happens.
    pub async fn make_block(&mut self, txs: Vec<Tx>) -> MakeBlockOutcome {
        self.try_make_block(txs).await.unwrap_or_else(|err| {
            panic!("fatal error while making block: {err}");
        })
    }

    /// Make a new block with the given transactions.
    pub async fn try_make_block(&mut self, txs: Vec<Tx>) -> AppResult<MakeBlockOutcome> {
        // Advance block height and time
        let mut new_block = self.block;
        new_block.height += 1;
        new_block.timestamp = self.block.timestamp + self.block_time;

        // Prepare proposal
        let raw_txs = txs
            .into_iter()
            .map(|tx| tx.to_json_vec().unwrap().into())
            .collect();
        let txs = self
            .app
            .do_prepare_proposal(raw_txs, usize::MAX)
            .into_iter()
            .map(|raw_tx| (raw_tx.deserialize_json().unwrap(), raw_tx.sha2_256()))
            .collect::<Vec<_>>();

        let block = Block {
            info: new_block,
            txs: txs.clone(),
        };

        // Call ABCI `FinalizeBlock` then `Commit`. The `App` methods are async
        // so they can `.await` the indexer trait directly.
        let block_outcome = self.app.do_finalize_block(block).await?; // TODO: drop uncommitted changes if errors
        self.app.do_commit().await?;

        self.block = new_block;

        Ok(MakeBlockOutcome { txs, block_outcome })
    }

    /// Execute a single transaction.
    pub async fn send_transaction(&mut self, tx: Tx) -> TxOutcome {
        let mut block_outcome = self.make_block(vec![tx]).await;

        block_outcome.block_outcome.tx_outcomes.pop().unwrap()
    }

    /// Sign a transaction with the default gas limit.
    pub fn sign_transaction(
        &self,
        signer: &mut (dyn Signer + Send + Sync),
        msgs: NonEmpty<Vec<Message>>,
    ) -> Tx {
        self.sign_transaction_with_gas(signer, self.default_gas_limit, msgs)
    }

    /// Sign a transaction with the given gas limit.
    pub fn sign_transaction_with_gas(
        &self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        msgs: NonEmpty<Vec<Message>>,
    ) -> Tx {
        signer
            .sign_transaction(msgs, &self.chain_id, gas_limit)
            .unwrap_or_else(|err| {
                panic!("fatal error while signing tx: {err}");
            })
    }

    /// Execute a single message.
    pub async fn send_message(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        msg: Message,
    ) -> TxOutcome {
        self.send_message_with_gas(signer, self.default_gas_limit, msg)
            .await
    }

    /// Execute a single message under the given gas limit.
    pub async fn send_message_with_gas(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        msg: Message,
    ) -> TxOutcome {
        self.send_messages_with_gas(signer, gas_limit, NonEmpty::new_unchecked(vec![msg]))
            .await
    }

    /// Execute one or more messages.
    pub async fn send_messages(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        msgs: NonEmpty<Vec<Message>>,
    ) -> TxOutcome {
        self.send_messages_with_gas(signer, self.default_gas_limit, msgs)
            .await
    }

    /// Execute one or more messages under the given gas limit.
    pub async fn send_messages_with_gas(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        msgs: NonEmpty<Vec<Message>>,
    ) -> TxOutcome {
        self.send_transaction(self.sign_transaction_with_gas(signer, gas_limit, msgs))
            .await
    }

    /// Update the chain's config.
    pub async fn configure<T>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        new_cfg: Option<Config>,
        new_app_cfg: Option<T>,
    ) -> TxOutcome
    where
        T: Serialize,
    {
        self.configure_with_gas(signer, self.default_gas_limit, new_cfg, new_app_cfg)
            .await
    }

    /// Update the chain's config under the given gas limit.
    pub async fn configure_with_gas<T>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        new_cfg: Option<Config>,
        new_app_cfg: Option<T>,
    ) -> TxOutcome
    where
        T: Serialize,
    {
        self.send_message_with_gas(
            signer,
            gas_limit,
            Message::configure(new_cfg, new_app_cfg).unwrap(),
        )
        .await
    }

    /// Schedule a chain upgrade.
    pub async fn upgrade<T, U, V>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        height: u64,
        cargo_version: T,
        git_tag: Option<U>,
        url: Option<V>,
    ) -> TxOutcome
    where
        T: Into<String>,
        U: Into<String>,
        V: Into<String>,
    {
        self.upgrade_with_gas(
            signer,
            self.default_gas_limit,
            height,
            cargo_version,
            git_tag,
            url,
        )
        .await
    }

    /// Schedule a chain upgrade under the given gas limit.
    pub async fn upgrade_with_gas<T, U, V>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        height: u64,
        cargo_version: T,
        git_tag: Option<U>,
        url: Option<V>,
    ) -> TxOutcome
    where
        T: Into<String>,
        U: Into<String>,
        V: Into<String>,
    {
        self.send_message_with_gas(
            signer,
            gas_limit,
            Message::upgrade(height, cargo_version, git_tag, url),
        )
        .await
    }

    /// Make a transfer of tokens.
    pub async fn transfer<C>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        to: Addr,
        coins: C,
    ) -> TxOutcome
    where
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        self.transfer_with_gas(signer, self.default_gas_limit, to, coins)
            .await
    }

    /// Make a transfer of tokens under the given gas limit.
    pub async fn transfer_with_gas<C>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        to: Addr,
        coins: C,
    ) -> TxOutcome
    where
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        self.send_message_with_gas(signer, gas_limit, Message::transfer(to, coins).unwrap())
            .await
    }

    /// Make a batched transfer of tokens to multiple recipients.
    pub async fn batch_transfer(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        transfers: BTreeMap<Addr, Coins>,
    ) -> TxOutcome {
        self.batch_transfer_with_gas(signer, self.default_gas_limit, transfers)
            .await
    }

    /// Make a batched transfer of tokens to multiple recipients, under the
    /// given gas limit.
    pub async fn batch_transfer_with_gas(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        transfers: BTreeMap<Addr, Coins>,
    ) -> TxOutcome {
        self.send_message_with_gas(
            signer,
            gas_limit,
            Message::batch_transfer(transfers).unwrap(),
        )
        .await
    }

    /// Upload a code. Return the code's hash.
    pub async fn upload<B>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        code: B,
    ) -> UploadOutcome
    where
        B: Into<Binary>,
    {
        self.upload_with_gas(signer, self.default_gas_limit, code)
            .await
    }

    /// Upload a code under the given gas limit. Return the code's hash.
    pub async fn upload_with_gas<B>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        code: B,
    ) -> UploadOutcome
    where
        B: Into<Binary>,
    {
        let code = code.into();
        let code_hash = code.sha2_256();

        let outcome = self
            .send_message_with_gas(signer, gas_limit, Message::upload(code))
            .await;

        UploadOutcome { code_hash, outcome }
    }

    /// Instantiate a contract. Return the contract's address.
    pub async fn instantiate<M, S, C>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        code_hash: Hash256,
        msg: &M,
        salt: S,
        label: Option<&str>,
        admin: Option<Addr>,
        funds: C,
    ) -> InstantiateOutcome
    where
        M: Serialize,
        S: Into<Binary>,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        self.instantiate_with_gas(
            signer,
            self.default_gas_limit,
            code_hash,
            msg,
            salt,
            label,
            admin,
            funds,
        )
        .await
    }

    /// Instantiate a contract under the given gas limit. Return the contract's
    /// address.
    pub async fn instantiate_with_gas<M, S, C>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        code_hash: Hash256,
        msg: &M,
        salt: S,
        label: Option<&str>,
        admin: Option<Addr>,
        funds: C,
    ) -> InstantiateOutcome
    where
        M: Serialize,
        S: Into<Binary>,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        let salt = salt.into();
        let address = Addr::derive(signer.address(), code_hash, &salt);

        let outcome = self
            .send_message_with_gas(
                signer,
                gas_limit,
                Message::instantiate(code_hash, msg, salt, label, admin, funds).unwrap(),
            )
            .await;

        InstantiateOutcome { address, outcome }
    }

    /// Upload a code and instantiate a contract with it in one go. Return the
    /// code hash as well as the contract's address.
    pub async fn upload_and_instantiate<M, B, S, L, C>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        code: B,
        msg: &M,
        salt: S,
        label: Option<L>,
        admin: Option<Addr>,
        funds: C,
    ) -> UploadAndInstantiateOutcome
    where
        M: Serialize,
        B: Into<Binary>,
        S: Into<Binary>,
        L: Into<String>,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        self.upload_and_instantiate_with_gas(
            signer,
            self.default_gas_limit,
            code,
            msg,
            salt,
            label,
            admin,
            funds,
        )
        .await
    }

    /// Upload a code and instantiate a contract with it in one go under the
    /// given gas limit. Return the code hash as well as the contract's address.
    pub async fn upload_and_instantiate_with_gas<M, B, S, L, C>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        code: B,
        msg: &M,
        salt: S,
        label: Option<L>,
        admin: Option<Addr>,
        funds: C,
    ) -> UploadAndInstantiateOutcome
    where
        M: Serialize,
        B: Into<Binary>,
        S: Into<Binary>,
        L: Into<String>,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        let code = code.into();
        let code_hash = code.sha2_256();
        let salt = salt.into();
        let address = Addr::derive(signer.address(), code_hash, &salt);

        let outcome = self
            .send_messages_with_gas(
                signer,
                gas_limit,
                NonEmpty::new_unchecked(vec![
                    Message::upload(code),
                    Message::instantiate(code_hash, msg, salt, label, admin, funds).unwrap(),
                ]),
            )
            .await;

        UploadAndInstantiateOutcome {
            address,
            code_hash,
            outcome,
        }
    }

    /// Execute a contrat.
    pub async fn execute<M, C>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        contract: Addr,
        msg: &M,
        funds: C,
    ) -> TxOutcome
    where
        M: Serialize,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        self.execute_with_gas(signer, self.default_gas_limit, contract, msg, funds)
            .await
    }

    /// Execute a contrat under the given gas limit.
    pub async fn execute_with_gas<M, C>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        contract: Addr,
        msg: &M,
        funds: C,
    ) -> TxOutcome
    where
        M: Serialize,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        self.send_message_with_gas(
            signer,
            gas_limit,
            Message::execute(contract, msg, funds).unwrap(),
        )
        .await
    }

    /// Migrate a contract to a new code hash.
    pub async fn migrate<M>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        contract: Addr,
        new_code_hash: Hash256,
        msg: &M,
    ) -> TxOutcome
    where
        M: Serialize,
    {
        self.migrate_with_gas(signer, self.default_gas_limit, contract, new_code_hash, msg)
            .await
    }

    /// Migrate a contract to a new code hash, under the given gas limit.
    pub async fn migrate_with_gas<M>(
        &mut self,
        signer: &mut (dyn Signer + Send + Sync),
        gas_limit: u64,
        contract: Addr,
        new_code_hash: Hash256,
        msg: &M,
    ) -> TxOutcome
    where
        M: Serialize,
    {
        self.send_message_with_gas(
            signer,
            gas_limit,
            Message::migrate(contract, new_code_hash, msg).unwrap(),
        )
        .await
    }

    /// Return a `QuerierWrapper` object.
    pub fn querier(&self) -> QuerierWrapper<'_> {
        QuerierWrapper::new(self)
    }

    /// Return a `Storage` object representing the storage of a given contract.
    pub fn contract_storage(&self, address: Addr) -> StorageProvider
    where
        <DB as Db>::Error: Debug,
    {
        let storage = self.app.db.state_storage(None).unwrap();
        StorageProvider::new(Box::new(storage), &[CONTRACT_NAMESPACE, &address])
    }
}

impl<DB, VM, PP, ID> Querier for TestSuite<DB, VM, PP, ID>
where
    DB: Db,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    fn query_chain(&self, req: Query) -> StdResult<QueryResponse> {
        self.app
            .do_query_app(req)
            .map_err(|err| StdError::Host(err.into_generic_backtraced_error()))
    }
}

impl<DB, VM, PP, ID> TestSuite<DB, VM, PP, ID>
where
    DB: Db,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
    Self: Querier,
{
    pub fn balances(&mut self) -> BalanceTracker<'_, DB, VM, PP, ID> {
        BalanceTracker { suite: self }
    }

    pub fn query_status(&self) -> StdResult<QueryStatusResponse> {
        <Self as QuerierExt>::query_status(self)
    }

    pub fn query_balance<D>(&self, address: &dyn Addressable, denom: D) -> StdResult<Uint128>
    where
        D: TryInto<Denom>,
        StdError: From<D::Error>,
    {
        let address = address.address();
        let denom = denom.try_into()?;
        <Self as QuerierExt>::query_balance(self, address, denom)
    }

    pub fn query_balances(&self, account: &dyn Addressable) -> StdResult<Coins> {
        let address = account.address();
        <Self as QuerierExt>::query_balances(self, address, None, Some(u32::MAX))
    }

    pub fn query_supply<D>(&self, denom: D) -> StdResult<Uint128>
    where
        D: TryInto<Denom>,
        StdError: From<D::Error>,
    {
        let denom = denom.try_into()?;
        <Self as QuerierExt>::query_supply(self, denom)
    }

    pub fn query_supplies(
        &self,
        start_after: Option<Denom>,
        limit: Option<u32>,
    ) -> StdResult<Coins> {
        <Self as QuerierExt>::query_supplies(self, start_after, limit)
    }
}

// ---- impl QueryApp for TestSuite ----

#[async_trait::async_trait]
impl<DB, VM, PP, ID> QueryApp for TestSuite<DB, VM, PP, ID>
where
    DB: Db + Send + Sync + 'static,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer + Send + Sync + 'static,
    ID: Indexer + Send + Sync + 'static,
    App<DB, VM, PP, ID>: QueryApp,
{
    async fn query_app(&self, raw_req: Query) -> AppResult<(QueryResponse, u64)> {
        self.app.query_app(raw_req).await
    }

    async fn simulate(&self, unsigned_tx: UnsignedTx) -> AppResult<TxOutcome> {
        self.app.simulate(unsigned_tx).await
    }

    async fn chain_id(&self) -> AppResult<String> {
        self.app.chain_id().await
    }

    async fn last_finalized_block(&self) -> AppResult<BlockInfo> {
        self.app.last_finalized_block().await
    }
}
