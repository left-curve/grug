use {
    crate::{
        query_memo::QueryMemo,
        traits::{ConsensusClient, QueryApp},
    },
    dango_indexer_sql::{
        EventCacheReader, entity::perps_trade::PerpsTrade, pubsub::PubSub,
        write::perps_trades::PerpsTradeCache,
    },
    dango_primitives::{Addr, JsonDeExt, Query},
    dango_types::config::{AppAddresses, AppConfig},
    sea_orm::{ConnectOptions, Database, DatabaseConnection},
    std::sync::Arc,
    tokio::sync::{OnceCell, RwLock},
};

/// The chain-query slice of [`FullContext`] — holds just the chain query app.
/// `FullContext` embeds one as its `base` field, and it is also injected as its
/// own `web::Data` so the `CoreQuery` resolvers and the `/query` / `/simulate`
/// handlers can extract it without the full context.
#[derive(Clone)]
pub struct MinimalContext {
    pub dango_app: Arc<dyn QueryApp + Send + Sync>,
    /// Addresses of the Dango contracts, resolved from the chain's app config
    /// on first use (see [`Self::app_addresses`]). Behind an `Arc` so that all
    /// clones of the context — one per actix worker — share the one cache.
    app_addresses: Arc<OnceCell<AppAddresses>>,
}

impl MinimalContext {
    pub fn new(dango_app: Arc<dyn QueryApp + Send + Sync>) -> Self {
        Self {
            dango_app,
            app_addresses: Arc::new(OnceCell::new()),
        }
    }

    /// The addresses of the Dango contracts, per the chain's app config.
    ///
    /// Resolved lazily on first call rather than at server construction, so
    /// that serving does not depend on chain state being queryable at startup
    /// (on a fresh chain, the app config exists only once the genesis state is
    /// committed). A failed resolution is returned as an error and retried on
    /// the next call; a success is cached for the life of the process — the
    /// contract addresses only change with a chain upgrade, which entails a
    /// node restart anyway.
    pub async fn app_addresses(&self) -> anyhow::Result<&AppAddresses> {
        self.app_addresses
            .get_or_try_init(|| async {
                let (response, _) = self.dango_app.query_app(Query::app_config()).await?;

                let app_config: AppConfig = response.into_app_config().deserialize_json()?;

                Ok::<_, anyhow::Error>(app_config.addresses)
            })
            .await
    }

    /// The address of the perps contract, per the chain's app config.
    pub async fn perps_address(&self) -> anyhow::Result<Addr> {
        Ok(self.app_addresses().await?.perps)
    }

    /// The address of the account factory contract, per the chain's app config.
    pub async fn account_factory_address(&self) -> anyhow::Result<Addr> {
        Ok(self.app_addresses().await?.account_factory)
    }
}

#[derive(Clone)]
pub struct FullContext {
    pub sql_context: dango_indexer_sql::Context,
    pub indexer_cache_context: dango_indexer_cache::Context,
    pub clickhouse_context: dango_indexer_clickhouse::context::Context,
    /// Reader handle to the validator-side realtime stream backing the
    /// `perps_events` subscription.
    pub stream_context: dango_indexer_stream::Context,
    pub base: MinimalContext,
    pub db: DatabaseConnection,
    pub pubsub: Arc<dyn PubSub<u64> + Send + Sync>,
    pub perps_trade_pubsub: Arc<dyn PubSub<PerpsTrade> + Send + Sync>,
    pub perps_trade_cache: Arc<RwLock<PerpsTradeCache>>,
    pub consensus_client: Arc<dyn ConsensusClient + Send + Sync>,
    pub event_cache: EventCacheReader,
    pub static_files_path: Option<String>,
    /// Per-block memo for the `/ws` `query` subscription: identical queries
    /// triggered by the same block are executed once and shared.
    pub query_memo: Arc<QueryMemo>,
}

impl FullContext {
    pub fn new(
        indexer_cache_context: dango_indexer_cache::Context,
        ctx: dango_indexer_sql::Context,
        clickhouse_context: dango_indexer_clickhouse::context::Context,
        stream_context: dango_indexer_stream::Context,
        dango_app: Arc<dyn QueryApp + Send + Sync>,
        consensus_client: Arc<dyn ConsensusClient + Send + Sync>,
        static_files_path: Option<String>,
    ) -> Self {
        Self {
            indexer_cache_context,
            clickhouse_context,
            stream_context,
            db: ctx.db.clone(),
            pubsub: ctx.pubsub.clone(),
            perps_trade_pubsub: ctx.perps_trade_pubsub.clone(),
            perps_trade_cache: ctx.perps_trade_cache.clone(),
            event_cache: ctx.event_cache.as_reader(),
            sql_context: ctx,
            base: MinimalContext::new(dango_app),
            consensus_client,
            static_files_path,
            query_memo: Arc::new(QueryMemo::new()),
        }
    }

    pub fn dango_app(&self) -> &Arc<dyn QueryApp + Send + Sync> {
        &self.base.dango_app
    }

    /// Preload the perps trade cache from existing DB data so that new
    /// GraphQL subscribers immediately receive recent trades.
    pub async fn start_perps_trade_cache(
        &self,
    ) -> Result<(), dango_indexer_sql::error::IndexerError> {
        let mut cache = self.perps_trade_cache.write().await;
        cache.preload(&self.db).await
    }
}

impl FullContext {
    pub async fn connect_db() -> Result<DatabaseConnection, sea_orm::DbErr> {
        let database_url = "sqlite::memory:";

        Self::connect_db_with_url(database_url).await
    }

    pub async fn connect_db_with_url(
        database_url: &str,
    ) -> Result<DatabaseConnection, sea_orm::DbErr> {
        let mut opt = ConnectOptions::new(database_url.to_owned());

        // Default number of workers is the number of logical CPUs, which is what actix is using
        // TODO: add this as a configuration flag
        let num_workers = num_cpus::get();

        opt.max_connections(num_workers as u32)
        //.min_connections(5)
        //.connect_timeout(Duration::from_secs(settings.timeout))
        //.idle_timeout(Duration::from_secs(8))
        //.max_lifetime(Duration::from_secs(20))
        .sqlx_logging(false);

        match Database::connect(opt).await {
            Ok(db) => {
                #[cfg(feature = "tracing")]
                tracing::info!(database_url, "Connected to database");

                Ok(db)
            },
            Err(error) => {
                #[cfg(feature = "tracing")]
                tracing::error!(database_url, %error, "Failed to connect to database");

                Err(error)
            },
        }
    }
}
