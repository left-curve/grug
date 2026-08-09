#[cfg(feature = "metrics")]
use metrics::counter;
#[cfg(feature = "tracing")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "metrics")]
use std::time::Instant;

use {
    crate::{
        Context, EventCache, EventCacheWriter,
        active_model::Models,
        entity, error,
        pubsub::{MemoryPubSub, PostgresPubSub, PubSubType},
    },
    dango_primitives::{
        BlockAndBlockOutcomeWithHttpDetails, Config, Defined, Json, MaybeDefined, Storage,
        Undefined,
    },
    itertools::Itertools,
    sea_orm::{
        ColumnTrait, ConnectionTrait, Database, DatabaseConnection, EntityTrait, QueryFilter,
        TransactionTrait,
    },
    std::sync::Arc,
};

// ------------------------------- IndexerBuilder ------------------------------

pub struct IndexerBuilder<DB = Undefined<String>> {
    db_url: DB,
    db_max_connections: u32,
    pubsub: PubSubType,
    event_cache_window: usize,
}

impl Default for IndexerBuilder {
    fn default() -> Self {
        Self {
            db_url: Undefined::default(),
            db_max_connections: 10,
            pubsub: PubSubType::Memory,
            event_cache_window: 100,
        }
    }
}

impl IndexerBuilder<Defined<String>> {
    pub fn with_database_max_connections(self, db_max_connections: u32) -> Self {
        IndexerBuilder {
            db_url: self.db_url,
            db_max_connections,
            pubsub: self.pubsub,
            event_cache_window: self.event_cache_window,
        }
    }
}

impl IndexerBuilder<Undefined<String>> {
    pub fn with_database_url<URL>(self, db_url: URL) -> IndexerBuilder<Defined<String>>
    where
        URL: ToString,
    {
        IndexerBuilder {
            db_url: Defined::new(db_url.to_string()),
            db_max_connections: self.db_max_connections,
            pubsub: self.pubsub,
            event_cache_window: self.event_cache_window,
        }
    }

    pub fn with_memory_database(self) -> IndexerBuilder<Defined<String>> {
        self.with_database_url("sqlite::memory:")
            .with_database_max_connections(1)
    }
}

impl<DB> IndexerBuilder<DB> {
    pub fn with_sqlx_pubsub(self) -> IndexerBuilder<DB> {
        IndexerBuilder {
            db_url: self.db_url,
            db_max_connections: self.db_max_connections,
            pubsub: PubSubType::Postgres,
            event_cache_window: self.event_cache_window,
        }
    }
}

impl<DB> IndexerBuilder<DB>
where
    DB: MaybeDefined<String>,
{
    pub fn with_event_cache_window(self, event_cache_window: usize) -> Self {
        Self {
            db_url: self.db_url,
            db_max_connections: self.db_max_connections,
            pubsub: self.pubsub,
            event_cache_window,
        }
    }

    pub async fn build_context(self) -> error::Result<Context> {
        let db = match self.db_url.maybe_into_inner() {
            Some(url) => Context::connect_db_with_url(&url, self.db_max_connections).await,
            None => Context::connect_db().await,
        }?;

        let mut context = Context {
            db: db.clone(),
            // This gets overwritten in the next match
            pubsub: Arc::new(MemoryPubSub::new(100)),
            perps_trade_pubsub: Arc::new(MemoryPubSub::new(100)),
            event_cache: EventCache::new(self.event_cache_window),
            perps_trade_cache: Default::default(),
        };

        match self.pubsub {
            PubSubType::Postgres => {
                if let DatabaseConnection::SqlxPostgresPoolConnection(_) = &context.db {
                    let pool: &sqlx::PgPool = context.db.get_postgres_connection_pool();

                    context.pubsub = Arc::new(PostgresPubSub::new(pool.clone(), "blocks").await?);
                }
            },
            PubSubType::Memory => {},
        }

        Ok(context)
    }

    pub async fn build(self) -> error::Result<Indexer> {
        let db = match self.db_url.maybe_into_inner() {
            Some(url) => Context::connect_db_with_url(&url, self.db_max_connections).await,
            None => Context::connect_db().await,
        }?;

        // Generate unique ID
        #[cfg(feature = "tracing")]
        let id = INDEXER_COUNTER.fetch_add(1, Ordering::SeqCst);

        let mut context = Context {
            db: db.clone(),
            // This gets overwritten in the next match
            pubsub: Arc::new(MemoryPubSub::new(100)),
            perps_trade_pubsub: Arc::new(MemoryPubSub::new(100)),
            event_cache: EventCache::new(self.event_cache_window),
            perps_trade_cache: Default::default(),
        };

        match self.pubsub {
            PubSubType::Postgres => {
                if let DatabaseConnection::SqlxPostgresPoolConnection(_) = &context.db {
                    let pool: &sqlx::PgPool = context.db.get_postgres_connection_pool();

                    context.pubsub = Arc::new(PostgresPubSub::new(pool.clone(), "blocks").await?);
                }
            },
            PubSubType::Memory => {},
        }

        Ok(Indexer {
            context,
            indexing: false,
            #[cfg(feature = "tracing")]
            id,
        })
    }
}

impl IndexerBuilder<Defined<String>> {
    /// Create a unique test database and return a guard for manual cleanup.
    ///
    /// This is useful for tests that need an isolated database. The returned guard
    /// has a `cleanup()` method that should be called when the test is done.
    /// The guard's Drop does NOT automatically clean up to avoid race conditions.
    pub async fn with_test_database(self) -> (Self, TestDatabaseGuard) {
        let base_url = self.db_url.inner().clone();

        // Only handle Postgres URLs
        let is_postgres =
            base_url.starts_with("postgres://") || base_url.starts_with("postgresql://");
        if !is_postgres {
            // Return unchanged for non-Postgres databases
            return (
                self,
                TestDatabaseGuard {
                    server_prefix: String::new(),
                    db_name: String::new(),
                },
            );
        }

        // Generate a unique database name
        let unique_suffix = uuid::Uuid::new_v4();
        let test_db_name = format!("grug_test_{}", unique_suffix.simple());

        // Everything before the final '/'
        let slash_pos = base_url.rfind('/').unwrap_or(base_url.len());
        let server_prefix = base_url[..slash_pos].to_string();

        // Create the new test database
        let parent_url = format!("{server_prefix}/postgres");
        let create_sql = format!("CREATE DATABASE \"{test_db_name}\"");
        let created = if let Ok(conn) = Database::connect(parent_url.clone()).await {
            conn.execute_unprepared(&create_sql).await.is_ok()
        } else {
            false
        };
        if !created {
            panic!(
                "Failed to create test database `{test_db_name}`; could not connect to parent database"
            );
        }

        // Build a new URL pointing to the newly created database
        let new_url = format!("{server_prefix}/{test_db_name}");

        let guard = TestDatabaseGuard {
            server_prefix,
            db_name: test_db_name,
        };

        let builder = IndexerBuilder {
            db_url: Defined::new(new_url),
            db_max_connections: self.db_max_connections,
            pubsub: self.pubsub,
            event_cache_window: self.event_cache_window,
        };

        (builder, guard)
    }
}

/// Guard for test database cleanup.
///
/// Automatically cleans up the database when dropped.
#[derive(Debug)]
pub struct TestDatabaseGuard {
    server_prefix: String,
    db_name: String,
}

impl TestDatabaseGuard {
    /// Get the test database name
    pub fn db_name(&self) -> &str {
        &self.db_name
    }
}

impl Drop for TestDatabaseGuard {
    fn drop(&mut self) {
        if self.db_name.is_empty() {
            return;
        }

        let server_prefix = std::mem::take(&mut self.server_prefix);
        let db_name = std::mem::take(&mut self.db_name);

        #[cfg(feature = "tracing")]
        tracing::debug!(db_name = %db_name, "TestDatabaseGuard::drop - cleaning up test database");

        // Spawn a separate thread with its own runtime to avoid any context issues
        let handle = std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(_) => return,
            };

            rt.block_on(async move {
                let parent = format!("{}/postgres", server_prefix);
                if let Ok(conn) = Database::connect(&parent).await {
                    let drop_sql = format!("DROP DATABASE \"{}\" WITH (FORCE)", db_name);
                    if conn.execute_unprepared(&drop_sql).await.is_err() {
                        let _ = conn
                            .execute_unprepared(&format!("DROP DATABASE \"{}\"", db_name))
                            .await;
                    }
                }
            });
        });

        // Block until cleanup completes
        let _ = handle.join();
    }
}

// ----------------------------- NonBlockingIndexer ----------------------------

// Add a global counter for unique IDs
#[cfg(feature = "tracing")]
static INDEXER_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Because I'm using `.spawn` in this implementation, I ran into lifetime issues where I need the
/// data to live as long as the spawned task.
///
/// I also have potential issues where the task spawned in `pre_indexing` to create a DB
/// transaction (in the sync implmentation of this trait) could be theorically executed after the
/// task spawned in `index_block` and `index_transaction` meaning I'd have to check in these
/// functions if the transaction exists or not.
///
/// Decided to do different and prepare the data in memory in `blocks` to inject all data in a single Tokio
/// spawned task
#[derive(Clone)]
pub struct Indexer {
    pub context: Context,
    // NOTE: this could be Arc<AtomicBool> because if this Indexer is cloned all instances should
    // be stopping when the program is stopped, but then it adds a lot of boilerplate. So far, code
    // as I understand it doesn't clone `App` in a way it'd raise concern.
    pub indexing: bool,
    // Add unique ID field, used for debugging and tracing
    #[cfg(feature = "tracing")]
    id: u64,
}

// ------------------------------- DB Related ----------------------------------
/// Maximum number of items to insert in a single `insert_many` operation.
/// This to avoid the following psql error:
/// PgConnection::run(): too many arguments for query
/// See discussion here:
/// https://www.postgresql.org/message-id/13394.1533697144%40sss.pgh.pa.us
pub const MAX_ROWS_INSERT: usize = 2048;

impl Indexer {
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    async fn save_block(
        db: DatabaseConnection,
        event_cache: EventCacheWriter,
        block: BlockAndBlockOutcomeWithHttpDetails,
    ) -> error::Result<()> {
        #[cfg(feature = "metrics")]
        let start = Instant::now();

        #[cfg(feature = "tracing")]
        tracing::info!(
            block_height = block.block.info.height,
            "Store block in SQL database"
        );

        let models = Models::build(&block)?;

        let db = db.begin().await?;

        // I check if the block already exists, if so it means we can skip the
        // whole block, transactions, messages and events since those are created
        // within a single DB transaction.
        // This scenario could happen if the process has crashed after block was
        // indexed but before the tmp_file was removed.
        let existing_block = entity::blocks::Entity::find()
            .filter(entity::blocks::Column::BlockHeight.eq(block.block.info.height))
            .one(&db)
            .await
            .inspect_err(|_e| {
                #[cfg(feature = "tracing")]
                tracing::error!(err = %_e, "Failed to check if block exists");
            })?;

        if existing_block.is_some() {
            return Ok(());
        }

        entity::blocks::Entity::insert(models.block)
            .exec_without_returning(&db)
            .await
            .inspect_err(|_e| {
                #[cfg(feature = "tracing")]
                tracing::error!(err = %_e, "Failed to insert block");

                #[cfg(feature = "metrics")]
                counter!("indexer.database.errors.total").increment(1);
            })?;

        #[cfg(feature = "metrics")]
        {
            use metrics::counter;

            counter!("indexer.blocks.total").increment(1);
            counter!("indexer.transactions.total").increment(models.transactions.len() as u64);
            counter!("indexer.messages.total").increment(models.messages.len() as u64);
            counter!("indexer.events.total").increment(models.events.len() as u64);
        }

        if !models.transactions.is_empty() {
            #[cfg(feature = "tracing")]
            let transactions_len = models.transactions.len();

            for transactions in models
                .transactions
                .into_iter()
                .chunks(MAX_ROWS_INSERT)
                .into_iter()
                .map(|c| c.collect())
                .collect::<Vec<Vec<_>>>()
            {
                entity::transactions::Entity::insert_many(transactions)
                .exec_without_returning(&db)
                .await.inspect_err(|_e| {
                    #[cfg(feature = "tracing")]
                    tracing::error!(err = %_e, transactions_len=transactions_len, "Failed to insert transactions");

                    #[cfg(feature = "metrics")]
                    counter!("indexer.database.errors.total").increment(1);
                })?;
            }
        }

        if !models.messages.is_empty() {
            #[cfg(feature = "tracing")]
            let messages_len = models.messages.len();

            for messages in models
                .messages
                .into_iter()
                .chunks(MAX_ROWS_INSERT)
                .into_iter()
                .map(|c| c.collect())
                .collect::<Vec<Vec<_>>>()
            {
                entity::messages::Entity::insert_many(messages)
                .exec_without_returning(&db)
                .await.inspect_err(|_e| {
                    #[cfg(feature = "tracing")]
                    tracing::error!(err = %_e, messages_len=messages_len, "Failed to insert messages");

                    #[cfg(feature = "metrics")]
                    counter!("indexer.database.errors.total").increment(1);
                })?;
            }
        }

        if !models.events.is_empty() {
            #[cfg(feature = "tracing")]
            let events_len = models.events.len();

            for events in models
                .events
                .into_iter()
                .chunks(MAX_ROWS_INSERT)
                .into_iter()
                .map(|c| c.collect())
                .collect::<Vec<Vec<_>>>()
            {
                entity::events::Entity::insert_many(events)
                .exec_without_returning(&db)
                .await
                .inspect_err(|_e| {
                    #[cfg(feature = "tracing")]
                    tracing::error!(err = %_e, events_len=events_len, "Failed to insert events");

                    #[cfg(feature = "metrics")]
                    counter!("indexer.database.errors.total").increment(1);
                })?;
            }
        }

        db.commit().await?;

        event_cache
            .save_events(block.block.info.height, models.events_by_address)
            .await;

        #[cfg(feature = "metrics")]
        metrics::histogram!("indexer.block_save.duration").record(start.elapsed().as_secs_f64());

        Ok(())
    }
}

impl Indexer {
    pub async fn last_indexed_block_height(&self) -> dango_app::IndexerResult<Option<u64>> {
        let last_indexed_block_height =
            entity::blocks::Entity::find_last_block_height(&self.context.db)
                .await
                .map_err(|e| dango_app::IndexerError::hook(e.to_string()))?;

        Ok(last_indexed_block_height.map(|h| h as u64))
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn start(&mut self, _storage: &dyn Storage) -> dango_app::IndexerResult<()> {
        #[cfg(feature = "metrics")]
        crate::metrics::init_indexer_metrics();

        self.context
            .migrate_db()
            .await
            .map_err(|e| dango_app::IndexerError::database(e.to_string()))?;

        self.indexing = true;

        Ok(())
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn shutdown(&mut self) -> dango_app::IndexerResult<()> {
        // Avoid running this twice when called manually and from `Drop`
        if !self.indexing {
            return Ok(());
        }

        self.indexing = false;

        Ok(())
    }

    /// No-op kept for symmetry with the other indexers' `wait_for_finish`.
    /// The SQL writer commits inline at the end of `post_indexing`, so there
    /// is no background work to drain here.
    pub async fn wait_for_finish(&self) -> dango_app::IndexerResult<()> {
        Ok(())
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn post_indexing(
        &self,
        block_height: u64,
        _cfg: Config,
        app_cfg: Json,
        block: &BlockAndBlockOutcomeWithHttpDetails,
    ) -> dango_app::IndexerResult<()> {
        if !self.indexing {
            return Err(dango_app::IndexerError::not_running());
        }

        #[cfg(feature = "tracing")]
        tracing::debug!(block_height, "`post_indexing` called");

        #[cfg(feature = "tracing")]
        let id = self.id;

        #[cfg(feature = "tracing")]
        tracing::debug!(
            block_height,
            indexer_id = id,
            "`post_indexing` async work started"
        );

        // 1. Dango-side writes (blocks, transactions, messages, events).
        //    Preserve the existing "log and bail early on failure" behavior so a
        //    save_block error does not propagate up and abort the rest of the
        //    indexer pipeline. The dango-side writes below depend on the rows
        //    save_block inserts, so we skip them if save_block failed.
        #[allow(clippy::map_identity)]
        if let Err(_err) = Self::save_block(
            self.context.db.clone(),
            self.context.event_cache.clone(),
            block.clone(),
        )
        .await
        {
            #[cfg(feature = "tracing")]
            tracing::error!(
                err = %_err,
                indexer_id = id,
                block_height,
                "Can't save to db in `post_indexing`"
            );

            #[cfg(feature = "metrics")]
            metrics::counter!("indexer.errors.save.total").increment(1);

            return Ok(());
        }

        #[cfg(feature = "metrics")]
        metrics::counter!("indexer.blocks.processed.total").increment(1);

        // 2. Dango-side writes (transfers, accounts, perps_events).
        //    Run in parallel; each reads from the rows just committed above.
        let (transfers_result, accounts_result, perps_result) = tokio::join!(
            crate::write::transfers::save_transfers(&self.context, block_height),
            crate::write::accounts::save_accounts(&self.context, block, app_cfg.clone()),
            crate::write::perps_events::save_perps_events(&self.context, block, app_cfg),
        );

        // Dango-side write failures are logged and counted but do not abort
        // the function before the pubsub publish below. The grug-side rows
        // committed in step 1 are still valid; subscribers should see "block N
        // is indexed at the grug level" notifications even if a dango-side
        // write transiently failed. (Without this, a null `app_cfg` — possible
        // in grug-only test harnesses that don't write APP_CONFIG to storage —
        // would short-circuit every block's notification, hanging GraphQL
        // subscribers indefinitely.)
        if let Err(_err) = transfers_result {
            #[cfg(feature = "tracing")]
            tracing::error!(
                err = %_err,
                indexer_id = id,
                block_height,
                "save_transfers failed",
            );

            #[cfg(feature = "metrics")]
            metrics::counter!("indexer.dango.hooks.transfers.errors.total").increment(1);
        }
        if let Err(_err) = accounts_result {
            #[cfg(feature = "tracing")]
            tracing::error!(
                err = %_err,
                indexer_id = id,
                block_height,
                "save_accounts failed",
            );

            #[cfg(feature = "metrics")]
            metrics::counter!("indexer.dango.hooks.accounts.errors.total").increment(1);
        }
        if let Err(_err) = perps_result {
            #[cfg(feature = "tracing")]
            tracing::error!(
                err = %_err,
                indexer_id = id,
                block_height,
                "save_perps_events failed",
            );

            #[cfg(feature = "metrics")]
            metrics::counter!("indexer.dango.hooks.perps_events.errors.total").increment(1);
        }

        // 3. Single pubsub publish, after grug-side and dango-side writes have
        //    been attempted. Fires whether dango-side writes succeeded or not.
        if let Err(_err) = self.context.pubsub.publish(block_height).await {
            #[cfg(feature = "tracing")]
            tracing::error!(
                err = %_err,
                indexer_id = id,
                block_height,
                "Can't publish block minted in `post_indexing`"
            );

            #[cfg(feature = "metrics")]
            metrics::counter!("indexer.errors.pubsub.total").increment(1);

            return Ok(());
        }

        #[cfg(feature = "metrics")]
        metrics::counter!("indexer.pubsub.published.total").increment(1);

        #[cfg(feature = "tracing")]
        tracing::debug!(block_height, indexer_id = id, "`post_indexing` finished");

        Ok(())
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        // If the DatabaseTransactions are left open (not committed) its `Drop` implementation
        // expects a Tokio context. We must call `commit` manually on it within our Tokio
        // context.
        // Since shutdown is now async, we can't call it from Drop in an async context.
        // Just mark as not indexing - the actual cleanup will happen when the async context
        // completes.
        self.indexing = false;
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {super::*, dango_primitives::MockStorage};

    /// This is when used from Dango, which is async. In such case the indexer does not have its
    /// own Tokio runtime and use the main handler. Making sure `start` can be called in an async
    /// context.
    #[tokio::test(flavor = "multi_thread")]
    async fn should_start() -> anyhow::Result<()> {
        let mut indexer: Indexer = IndexerBuilder::default()
            .with_memory_database()
            .build()
            .await?;
        let storage = MockStorage::new();

        assert!(!indexer.indexing);
        indexer.start(&storage).await.expect("can't start indexer");
        assert!(indexer.indexing);

        indexer.shutdown().await.expect("can't shutdown Indexer");
        assert!(!indexer.indexing);

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_without_hooks() -> anyhow::Result<()> {
        let mut indexer: Indexer = IndexerBuilder::default()
            .with_memory_database()
            .build()
            .await?;
        let storage = MockStorage::new();

        assert!(!indexer.indexing);
        indexer.start(&storage).await.expect("can't start Indexer");
        assert!(indexer.indexing);

        indexer.shutdown().await.expect("can't shutdown Indexer");
        assert!(!indexer.indexing);

        Ok(())
    }
}
