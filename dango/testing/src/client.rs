use {
    crate::{MakeBlockOutcome, suite::TestSuite},
    anyhow::anyhow,
    async_trait::async_trait,
    dango_app::{AppError, Db, Indexer, NaiveProposalPreparer, NullIndexer, ProposalPreparer, Vm},
    dango_db_memory::MemDb,
    dango_primitives::{
        Block, BlockClient, BlockInfo, BlockOutcome, BroadcastClient, BroadcastTxOutcome, Hash256,
        Query, QueryClient, QueryResponse, SearchTxClient, SearchTxOutcome, Timestamp, Tx,
        TxOutcome, UnsignedTx,
    },
    dango_vm_rust::RustVm,
    std::{collections::BTreeMap, ops::DerefMut, sync::Arc, thread, time::Duration},
    tokio::{
        runtime::Runtime,
        sync::{Mutex, RwLock, RwLockReadGuard},
    },
};

pub struct MockClient<DB = MemDb, VM = RustVm, PP = NaiveProposalPreparer, ID = NullIndexer>
where
    DB: Db,
    VM: Vm,
    PP: ProposalPreparer,
    ID: Indexer,
{
    suite: Arc<RwLock<TestSuite<DB, VM, PP, ID>>>,
    blocks: Arc<Mutex<BTreeMap<u64, (Block, BlockOutcome)>>>,
    txs: Arc<Mutex<BTreeMap<Hash256, SearchTxOutcome>>>,
    block_mode: BlockModeCache,
}

impl<DB, VM, PP, ID> MockClient<DB, VM, PP, ID>
where
    DB: Db + Send + Sync + 'static,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer + Send + Sync + 'static,
    ID: Indexer + Send + Sync + 'static,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    pub fn new(suite: TestSuite<DB, VM, PP, ID>, block_mode: BlockCreation) -> Self {
        let suite = Arc::new(RwLock::new(suite));
        Self::new_shared(suite, block_mode)
    }

    pub fn new_shared(
        suite: Arc<RwLock<TestSuite<DB, VM, PP, ID>>>,
        block_mode: BlockCreation,
    ) -> Self {
        let blocks = Arc::new(Mutex::new(BTreeMap::new()));
        let txs = Arc::new(Mutex::new(BTreeMap::new()));

        let block_mode = match block_mode {
            BlockCreation::Timed => {
                let buffer = Arc::new(Mutex::new(vec![]));

                let th_buffer = buffer.clone();
                let th_suite = suite.clone();
                let th_blocks = blocks.clone();
                let th_txs = txs.clone();

                thread::spawn(move || {
                    let rt = Runtime::new().unwrap();

                    rt.block_on(async move {
                        loop {
                            let sleep = th_suite.read().await.block_time.into_nanos();
                            tokio::time::sleep(Duration::from_nanos(sleep as u64)).await;

                            let txs = {
                                let mut guard = th_buffer.lock().await;
                                std::mem::take(&mut *guard)
                            };

                            next_block(
                                th_txs.lock().await.deref_mut(),
                                th_blocks.lock().await.deref_mut(),
                                th_suite.write().await.deref_mut(),
                                txs,
                            )
                            .await
                            .unwrap();
                        }
                    })
                });

                BlockModeCache::Timed { buffer }
            },
            BlockCreation::OnBroadcast => BlockModeCache::OnBroadcast,
        };

        Self {
            suite,
            blocks,
            txs,
            block_mode,
        }
    }

    pub async fn set_timestamp(&self, timestamp: Timestamp) {
        self.suite.write().await.block.timestamp = timestamp;
    }

    pub async fn set_block_time(&self, block_time: dango_primitives::Duration) {
        self.suite.write().await.block_time = block_time;
    }

    pub async fn chain_id(&self) -> String {
        self.suite.read().await.chain_id.clone()
    }

    pub async fn suite(&self) -> RwLockReadGuard<'_, TestSuite<DB, VM, PP, ID>> {
        self.suite.read().await
    }

    pub async fn make_empty_block(&self) -> anyhow::Result<()> {
        next_block(
            self.txs.lock().await.deref_mut(),
            self.blocks.lock().await.deref_mut(),
            self.suite.write().await.deref_mut(),
            vec![],
        )
        .await
    }
}

#[async_trait]
impl<DB, VM, PP, ID> QueryClient for MockClient<DB, VM, PP, ID>
where
    DB: Db + Send + Sync,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer + Send + Sync,
    ID: Indexer + Send + Sync,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    type Error = anyhow::Error;
    type Proof = dango_primitives::Proof;

    async fn query_app(&self, query: Query) -> Result<QueryResponse, Self::Error> {
        Ok(self.suite.read().await.app.do_query_app(query)?)
    }

    async fn simulate(&self, tx: UnsignedTx) -> Result<TxOutcome, Self::Error> {
        Ok(self.suite.read().await.app.do_simulate(tx)?)
    }
}

#[async_trait]
impl<DB, VM, PP, ID> BlockClient for MockClient<DB, VM, PP, ID>
where
    DB: Db + Send + Sync,
    VM: Vm + Send + Sync,
    PP: ProposalPreparer + Send + Sync,
    ID: Indexer + Send + Sync,
{
    type Error = anyhow::Error;

    async fn query_block(&self, height: Option<u64>) -> Result<Block, Self::Error> {
        let maybe_block = match height {
            Some(height) => self
                .blocks
                .lock()
                .await
                .get(&height)
                .map(|(block, _)| block.clone()),
            None => self
                .blocks
                .lock()
                .await
                .last_key_value()
                .map(|(_, (block, _))| block.clone()),
        };

        maybe_block.ok_or_else(|| anyhow!("block not found: {height:?}"))
    }

    async fn query_block_outcome(&self, height: Option<u64>) -> Result<BlockOutcome, Self::Error> {
        let maybe_block = match height {
            Some(height) => self
                .blocks
                .lock()
                .await
                .get(&height)
                .map(|(_, block)| block.clone()),
            None => self
                .blocks
                .lock()
                .await
                .last_key_value()
                .map(|(_, (_, block))| block.clone()),
        };

        maybe_block.ok_or_else(|| anyhow!("block not found: {height:?}"))
    }
}

#[async_trait]
impl<DB, VM, PP, ID> SearchTxClient for MockClient<DB, VM, PP, ID>
where
    DB: Db + Send + Sync,
    VM: Vm + Send + Sync,
    PP: ProposalPreparer + Send + Sync,
    ID: Indexer + Send + Sync,
{
    type Error = anyhow::Error;

    async fn search_tx(&self, hash: Hash256) -> Result<SearchTxOutcome, Self::Error> {
        self.txs
            .lock()
            .await
            .get(&hash)
            .cloned()
            .ok_or_else(|| anyhow!("tx not found: {hash}"))
    }
}

#[async_trait]
impl<DB, VM, PP, ID> BroadcastClient for MockClient<DB, VM, PP, ID>
where
    DB: Db + Send + Sync,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer + Send + Sync,
    ID: Indexer + Send + Sync,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    type Error = anyhow::Error;

    async fn broadcast_tx(&self, tx: Tx) -> Result<BroadcastTxOutcome, Self::Error> {
        let tx_hash = tx.tx_hash()?;

        let check_tx = self.suite.read().await.app.do_check_tx(tx.clone())?;
        if check_tx.result.is_err() {
            return Ok(BroadcastTxOutcome { tx_hash, check_tx });
        };

        match &self.block_mode {
            BlockModeCache::Timed { buffer, .. } => {
                buffer.lock().await.push(tx);
            },
            BlockModeCache::OnBroadcast => {
                next_block(
                    self.txs.lock().await.deref_mut(),
                    self.blocks.lock().await.deref_mut(),
                    self.suite.write().await.deref_mut(),
                    vec![tx],
                )
                .await?;
            },
        }

        Ok(BroadcastTxOutcome { tx_hash, check_tx })
    }
}

async fn index(
    index_txs: &mut BTreeMap<Hash256, SearchTxOutcome>,
    index_blocks: &mut BTreeMap<u64, (Block, BlockOutcome)>,
    outcome: MakeBlockOutcome,
    block_info: BlockInfo,
) -> anyhow::Result<()> {
    for (index, ((tx, hash), outcome)) in outcome
        .txs
        .iter()
        .zip(outcome.block_outcome.tx_outcomes.iter())
        .enumerate()
    {
        index_txs.insert(
            *hash,
            SearchTxOutcome {
                hash: *hash,
                height: block_info.height,
                index: index as u32,
                tx: tx.clone(),
                outcome: outcome.clone(),
            },
        );
    }

    let block = Block {
        info: block_info,
        txs: outcome.txs,
    };

    index_blocks.insert(block_info.height, (block, outcome.block_outcome));

    Ok(())
}

async fn next_block<DB, VM, PP, ID>(
    index_txs: &mut BTreeMap<Hash256, SearchTxOutcome>,
    index_blocks: &mut BTreeMap<u64, (Block, BlockOutcome)>,
    suite: &mut TestSuite<DB, VM, PP, ID>,
    tx: Vec<Tx>,
) -> anyhow::Result<()>
where
    DB: Db,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    let outcome = suite.make_block(tx).await;
    index(index_txs, index_blocks, outcome, suite.block).await
}

enum BlockModeCache {
    Timed { buffer: Arc<Mutex<Vec<Tx>>> },
    OnBroadcast,
}

/// Settings for the block creation.
pub enum BlockCreation {
    /// The block is created at a fixed interval (based on `suite.block.timestamp`).
    Timed,
    /// The block is created when a transaction is broadcasted.
    OnBroadcast,
}
