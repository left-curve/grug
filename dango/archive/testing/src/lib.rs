//! End-to-end test harness for the archive.
//!
//! Helpers that stand up a full environment — a Postgres engine (a throwaway
//! schema on the `DATABASE_URL` server, default
//! `postgres://postgres@localhost/grug_test`), a temp RocksDB block store, and
//! a mock Dango node feeding the `App` — so the integration tests in `tests/`
//! can drive the real pipeline end to end.
//!
//! Two entry points: [`Env::setup`] stands up everything at once (node + db +
//! indexer); [`PendingEnv`] stops after the node + db so a test can drive the
//! chain to an arbitrary height *before* starting the indexer — reproducing a
//! production cold start where the live tail is at the tip and the fetcher must
//! backfill all the history below.

use {
    anyhow::Context,
    dango_archive_app::{App, PgChCommitter},
    dango_archive_block_source::{
        BlockFetcher, BlockSource, BlockStore, HttpdClient, RemoteBlockSource,
        RemoteBlockSourceConfig, RocksdbBlockStore, SentinelBlockFetcher, SentinelFetcherConfig,
    },
    dango_archive_httpd::HttpdConfig,
    dango_archive_projection::{ActivityConfig, ActivityProjection, Committer, Projection},
    dango_genesis::{Contracts, GenesisOption},
    dango_primitives::{
        Addr, BroadcastClient, Coins, Hash256, MOCK_CHAIN_ID, Message, NonEmpty, Signer,
    },
    dango_sdk::HttpClient,
    dango_testing::{
        BlockCreation, Preset, TestAccount, TestAccounts, TestOption, mock_httpd_get_socket_addr,
        mock_httpd_run_with_callback, mock_httpd_wait_for_server_ready,
    },
    dango_types::constants::usdc,
    sea_orm::{
        ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    },
    std::{sync::Arc, time::Duration},
    tempfile::{Builder, TempDir},
};

// ---- Postgres test database ----

/// A Postgres test database scoped to a throwaway schema.
///
/// Uses the Postgres server at `DATABASE_URL` (default
/// `postgres://postgres@localhost/grug_test`); each [`TestDb`] gets a fresh
/// `CREATE SCHEMA`d namespace so parallel tests never collide.
///
/// Cleanup is automatic on drop: the schema is `DROP`ped so nothing leaks.
pub struct TestDb {
    /// Connection pinned to this test's schema via `search_path`. Share its
    /// clone with the committer and the read API.
    pub conn: DatabaseConnection,
    schema: String,
    base_url: String,
}

impl TestDb {
    /// Stand up a fresh, schema-isolated test database.
    pub async fn setup() -> anyhow::Result<Self> {
        let base_url = base_engine().await?;
        let schema = format!("hist_e2e_{}", uuid::Uuid::new_v4().simple());

        // Create the schema on a plain admin connection (no `search_path`), so it
        // exists before the scoped pool's connections run `SET search_path`.
        let admin = Database::connect(&base_url)
            .await
            .context("connecting to the test Postgres")?;
        admin
            .execute_unprepared(&format!("CREATE SCHEMA IF NOT EXISTS \"{schema}\""))
            .await
            .context("creating the throwaway schema")?;
        admin.close().await.ok();

        // The working pool, every connection pinned to the schema.
        let mut opt = ConnectOptions::new(&base_url);
        opt.set_schema_search_path(schema.clone())
            .max_connections(5)
            .sqlx_logging(false);
        let conn = Database::connect(opt)
            .await
            .context("connecting with the scoped search_path")?;

        Ok(Self {
            conn,
            schema,
            base_url,
        })
    }

    /// Apply the committer's own migrations plus every projection's, into this
    /// test's schema — exactly what `App::run` does at boot.
    pub async fn migrate(&self, projections: &[Arc<dyn Projection>]) -> anyhow::Result<()> {
        PgChCommitter::new(self.conn.clone(), None)
            .migrate(projections)
            .await
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // Drop the schema so it does not leak — on a fresh thread + runtime,
        // since `Drop` is sync and may run inside the test's own runtime.
        let base_url = self.base_url.clone();
        let schema = self.schema.clone();
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Runtime::new() else {
                return;
            };
            rt.block_on(async {
                if let Ok(db) = Database::connect(&base_url).await {
                    let _ = db
                        .execute_unprepared(&format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE"))
                        .await;
                }
            });
        })
        .join();
    }
}

/// Resolve the external Postgres: `DATABASE_URL` if set, else a local default
/// (`postgres://postgres@localhost/grug_test`, the same default as
/// `dango-testing`). The database is created if missing; each test carves its
/// own schema inside it.
async fn base_engine() -> anyhow::Result<String> {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost/grug_test".to_string());
    // CI points `DATABASE_URL` at a `dango_test` database the bare Postgres
    // container never creates, and the local default `grug_test` likewise may
    // not exist yet; make sure it exists before we carve schemas inside it.
    ensure_database_exists(&url).await?;
    Ok(url)
}

/// Ensure the database named in an external `DATABASE_URL` exists, creating it
/// through the server's `postgres` maintenance database if not.
///
/// CI points `DATABASE_URL` at a `dango_test` database that the bare Postgres
/// container never creates — the in-process indexer harness likewise carves its
/// own test databases. Postgres has no `CREATE DATABASE IF NOT EXISTS`, so check
/// `pg_database` first and tolerate a concurrent creator: the `db`, `e2e`, and
/// `backfill` test binaries run as separate processes and can reach here at once.
async fn ensure_database_exists(url: &str) -> anyhow::Result<()> {
    let slash = url
        .rfind('/')
        .context("DATABASE_URL has no `/<database>`")?;
    let server_prefix = &url[..slash];
    // Drop any `?query` suffix from the database name.
    let db_name = url[slash + 1..].split('?').next().unwrap_or_default();
    if db_name.is_empty() {
        return Ok(());
    }

    let admin = Database::connect(format!("{server_prefix}/postgres"))
        .await
        .context("connecting to the `postgres` maintenance database")?;
    let exists = admin
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT 1 AS one FROM pg_database WHERE datname = '{db_name}'"),
        ))
        .await
        .context("checking whether the test database exists")?
        .is_some();
    if !exists {
        // A sibling process may win the race and create it first; our `CREATE`
        // then fails with "already exists", which is exactly the state we want.
        let _ = admin
            .execute_unprepared(&format!("CREATE DATABASE \"{db_name}\""))
            .await;
    }
    admin.close().await.ok();
    Ok(())
}

// ---- Full end-to-end environment ----

/// A spawned `App::run` task, aborted when the env drops so a test never leaks
/// the indexer's background loops past its own lifetime.
struct AbortOnDrop(tokio::task::JoinHandle<anyhow::Result<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Stage one of the harness: a running mock node + a test database, with the
/// indexer **not** yet started. A test can drive the chain to any height first
/// (via [`broadcast_transfer`]) and only then [`start_indexer`](Self::start_indexer),
/// so the `RemoteBlockSource` connects mid-history — the live tail sees only new
/// blocks while the fetcher backfills everything below, as in production.
pub struct PendingEnv {
    /// Broadcast transactions to the mock node with this.
    pub client: HttpClient,
    /// The genesis-funded accounts (`owner`, `user1`..`user9`) to sign with.
    pub accounts: TestAccounts,
    /// The deployment's system contract addresses (e.g. `contracts.bank`).
    pub contracts: Contracts,
    /// The mock node's bound port.
    pub node_port: u16,
    db: TestDb,
}

impl PendingEnv {
    /// Mock node (real chain + httpd) + schema-isolated Postgres; no indexer.
    pub async fn setup() -> anyhow::Result<Self> {
        let db = TestDb::setup().await?;
        let (client, accounts, contracts, node_port) = spawn_mock_node().await?;
        Ok(Self {
            client,
            accounts,
            contracts,
            node_port,
            db,
        })
    }

    /// Start the indexer over a production `RemoteBlockSource` (temp RocksDB
    /// store + sentinel fetcher + live tail), pointed at the mock node's
    /// **current** tip. The live tail (`since=None`) streams only blocks newer
    /// than that tip, so everything below it is backfilled by the fetcher.
    /// Migrations run before this returns, so the read schema is queryable
    /// immediately.
    pub async fn start_indexer(self) -> anyhow::Result<Env> {
        let Self {
            client,
            accounts,
            contracts,
            node_port,
            db,
        } = self;

        // The remote block source pointed at the mock node: temp RocksDB store,
        // one httpd client shared by the live tail and the sentinel fetcher
        // (`HttpdClient` is `Clone`, so the fetcher carries no URL of its own).
        let block_store_dir = Builder::new()
            .prefix("hist_e2e_blocks")
            .tempdir()
            .context("creating the temp block store dir")?;
        let store: Arc<dyn BlockStore> = Arc::new(
            RocksdbBlockStore::open(block_store_dir.path())
                .context("opening the temp block store")?,
        );
        let live = HttpdClient::new(format!("http://127.0.0.1:{node_port}"))
            .context("building the node httpd client")?;
        let fetcher: Arc<dyn BlockFetcher> = Arc::new(SentinelBlockFetcher::new(
            live.clone(),
            SentinelFetcherConfig::default(),
        ));
        let source: Arc<dyn BlockSource> = Arc::new(RemoteBlockSource::new(
            store,
            live,
            fetcher,
            RemoteBlockSourceConfig::default(),
        ));

        // The committer + projections — ClickHouse deferred (`None`).
        let committer: Arc<dyn Committer> = Arc::new(PgChCommitter::new(db.conn.clone(), None));
        // The `/events/perps` shortcut anchors on whatever address is injected.
        // The tests' window produces no perps activity (it stays below the
        // first perps cron), so anchor the shortcut on the **bank** instead:
        // every `/events/contract?contract={bank}` assertion can then be
        // replayed on `/events/perps` verbatim — same code path, real fixtures.
        let projections: Vec<Arc<dyn Projection>> =
            vec![Arc::new(ActivityProjection::new(ActivityConfig {
                perps_contract: Some(contracts.bank),
                ..Default::default()
            }))];

        // Migrate up front so the tables are queryable immediately; `App::run`
        // re-migrates idempotently at boot. Doing it here also surfaces a
        // migration failure to the caller instead of the background task.
        db.migrate(&projections)
            .await
            .context("migrating the test schema")?;

        // Serve the REST read API on a free port over the same Postgres + block
        // source the App ingests into, so reads see exactly what it writes.
        // `App::run` assembles the httpd from the projections' own routes.
        let httpd_port = mock_httpd_get_socket_addr();
        let read_bind = format!("127.0.0.1:{httpd_port}");
        let read_cfg = Some(HttpdConfig {
            bind: read_bind.clone(),
        });

        // Supervise ingest + read API in the background; surface a fatal exit on
        // stderr so a test that then times out has the cause in its output.
        let app = App::new(
            source.clone(),
            committer,
            projections,
            db.conn.clone(),
            read_cfg,
        );
        let app = AbortOnDrop(tokio::spawn(async move {
            let result = app.run().await;
            if let Err(ref error) = result {
                eprintln!("archive app exited: {error:#}");
            }
            result
        }));

        Ok(Env {
            client,
            accounts,
            contracts,
            read_url: format!("http://{read_bind}"),
            http: reqwest::Client::new(),
            node_port,
            _app: app,
            _source: source,
            _block_store_dir: block_store_dir,
            _db: db,
        })
    }
}

/// A full archive environment: a mock Dango node producing blocks,
/// the `App` ingesting them into a throwaway Postgres via a temp RocksDB store,
/// and the REST read API to assert on. Build with [`Env::setup`] (all at once)
/// or [`PendingEnv::start_indexer`]; everything is torn down on drop.
pub struct Env {
    /// Broadcast transactions to the mock node with this — under `OnBroadcast`
    /// each broadcast drives exactly one block.
    pub client: HttpClient,
    /// The genesis-funded accounts (`owner`, `user1`..`user9`) to sign with.
    pub accounts: TestAccounts,
    /// The deployment's system contract addresses (e.g. `contracts.bank`).
    pub contracts: Contracts,
    /// Base URL of the in-process REST read API (`http://127.0.0.1:<port>`).
    read_url: String,
    /// HTTP client for the read API.
    http: reqwest::Client,
    /// The mock node's bound port.
    pub node_port: u16,

    // Drop guards, in drop order: stop ingest first, then release its inputs.
    // `_block_store_dir` is a plain `TempDir` (best-effort recursive remove on
    // drop), not a `DB::destroy`-on-drop wrapper, because the aborted `App` task
    // releases the RocksDB asynchronously and could still hold its lock here.
    _app: AbortOnDrop,
    _source: Arc<dyn BlockSource>,
    _block_store_dir: TempDir,
    _db: TestDb,
}

impl Env {
    /// Stand up the whole pipeline in one shot (node + db + indexer), for tests
    /// that don't need to control the chain height before the indexer starts.
    pub async fn setup() -> anyhow::Result<Self> {
        PendingEnv::setup().await?.start_indexer().await
    }

    /// The shared Postgres connection (the schema-isolated test database).
    #[must_use]
    pub fn conn(&self) -> &DatabaseConnection {
        &self._db.conn
    }

    /// GET `path` on the read API, returning the JSON body for a `2xx`, `None`
    /// for a `404` (an absent resource — e.g. `block(height)` the source does not
    /// hold). Any other status, or a transport error, is an `Err`.
    pub async fn get_opt(&self, path: &str) -> anyhow::Result<Option<serde_json::Value>> {
        let response = self
            .http
            .get(format!("{}{path}", self.read_url))
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body = response
            .bytes()
            .await
            .context("reading the response body")?;
        if !status.is_success() {
            anyhow::bail!("GET {path} -> {status}: {}", String::from_utf8_lossy(&body));
        }
        Ok(Some(
            serde_json::from_slice(&body).context("read API response to json")?,
        ))
    }

    /// GET `path`, requiring a `2xx` body (a `404` is an error here).
    pub async fn get(&self, path: &str) -> anyhow::Result<serde_json::Value> {
        self.get_opt(path)
            .await?
            .with_context(|| format!("GET {path} returned 404"))
    }

    /// GET `path` returning the raw body as text (redirects followed),
    /// requiring a `2xx` — for non-JSON resources like the Swagger UI page.
    pub async fn get_text(&self, path: &str) -> anyhow::Result<String> {
        let response = self
            .http
            .get(format!("{}{path}", self.read_url))
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        let status = response.status();
        let body = response.text().await.context("reading the response body")?;
        if !status.is_success() {
            anyhow::bail!("GET {path} -> {status}");
        }
        Ok(body)
    }

    /// Poll `GET /transactions/{hash}` until that tx is indexed or
    /// `timeout` elapses — the catch-up signal. The projection advances
    /// contiguously, so once a block's tx is visible every block below it has
    /// been ingested too. Returns the list of matching units. Tolerates the read
    /// API's startup window (a not-yet-bound server reads as "not indexed").
    /// Base URL of the in-process REST read API — for driving it with an
    /// external client (e.g. the SDK's `ArchiveClient`) instead of the raw
    /// helpers below.
    pub fn read_url(&self) -> &str {
        &self.read_url
    }

    pub async fn wait_for_tx_indexed(
        &self,
        hash: &str,
        timeout: Duration,
    ) -> anyhow::Result<serde_json::Value> {
        let path = format!("/transactions/{hash}");
        let start = std::time::Instant::now();
        loop {
            if let Ok(Some(units)) = self.get_opt(&path).await
                && units.as_array().is_some_and(|units| !units.is_empty())
            {
                return Ok(units);
            }
            if start.elapsed() >= timeout {
                anyhow::bail!("timed out after {timeout:?} waiting for tx {hash} to be indexed");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// A single, non-blocking check of whether `hash`'s tx is indexed yet — for
    /// a producer loop that pumps the chain forward until the backfill catches
    /// up. A transport error (read API not yet up) reads as "not indexed".
    pub async fn is_tx_indexed(&self, hash: &str) -> anyhow::Result<bool> {
        match self.get_opt(&format!("/transactions/{hash}")).await {
            Ok(Some(units)) => Ok(units.as_array().is_some_and(|units| !units.is_empty())),
            _ => Ok(false),
        }
    }

    /// Poll `GET /transactions/involving/{address}?role={role}` until it returns
    /// at least one item or `timeout` elapses — the bridge across the async
    /// ingest pipeline. Returns the page (`{ items, pageInfo }`).
    pub async fn wait_for_transactions_involving(
        &self,
        address: &str,
        role: &str,
        timeout: Duration,
    ) -> anyhow::Result<serde_json::Value> {
        let path = format!("/transactions/involving/{address}?role={role}");
        let start = std::time::Instant::now();
        loop {
            if let Ok(Some(page)) = self.get_opt(&path).await
                && page["items"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty())
            {
                return Ok(page);
            }
            if start.elapsed() >= timeout {
                anyhow::bail!(
                    "timed out after {timeout:?} waiting for a transaction \
                     involving {address} as {role}"
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Page through a feed 50 at a time, returning every item's `blockHeight`
    /// newest-first across pages — exercising cursor pagination end to end.
    /// `path` is the feed's REST path including any non-pagination query
    /// arguments, e.g. `/transactions/involving/0x..?role=sender` or
    /// `/events/contract?contract=0x..&names=sent`.
    pub async fn collect_heights(&self, path: &str) -> anyhow::Result<Vec<u64>> {
        let sep = if path.contains('?') {
            '&'
        } else {
            '?'
        };
        let mut heights = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let paged = match &after {
                Some(cursor) => format!("{path}{sep}first=50&after={cursor}"),
                None => format!("{path}{sep}first=50"),
            };
            let page = self.get(&paged).await?;
            if let Some(items) = page["items"].as_array() {
                for item in items {
                    heights.push(item["blockHeight"].as_u64().context("blockHeight")?);
                }
            }
            if page["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false) {
                after = Some(
                    page["pageInfo"]["endCursor"]
                        .as_str()
                        .context("endCursor")?
                        .to_string(),
                );
            } else {
                return Ok(heights);
            }
        }
    }
}

/// Sign and broadcast a USDC transfer `from` → `to`, driving exactly one block
/// on the mock node (under `OnBroadcast`). Returns the tx hash.
///
/// A free function, not an [`Env`] method, so the caller can borrow `client`
/// immutably and a `&mut` account from the same env at once without tripping
/// the borrow checker.
pub async fn broadcast_transfer(
    client: &HttpClient,
    from: &mut TestAccount,
    to: Addr,
    amount: u128,
) -> anyhow::Result<Hash256> {
    let tx = from.sign_transaction(
        NonEmpty::new_unchecked(vec![Message::transfer(
            to,
            Coins::one(usdc::DENOM.clone(), amount)?,
        )?]),
        MOCK_CHAIN_ID,
        1_000_000,
    )?;
    Ok(client.broadcast_tx(tx).await?.tx_hash)
}

/// Spin up an in-process mock Dango node (real chain + httpd) on a free port,
/// recover its genesis-funded accounts and system contracts, and hand back a
/// client to broadcast against it. The node runs on its own thread + runtime
/// because `mock_httpd_run_*` only returns when the server stops.
async fn spawn_mock_node() -> anyhow::Result<(HttpClient, TestAccounts, Contracts, u16)> {
    let port = mock_httpd_get_socket_addr();
    let (genesis_tx, genesis_rx) = tokio::sync::oneshot::channel();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(error) => {
                eprintln!("mock node runtime: {error}");
                return;
            },
        };
        rt.block_on(async move {
            if let Err(error) = mock_httpd_run_with_callback(
                port,
                BlockCreation::OnBroadcast,
                None,
                TestOption::default(),
                GenesisOption::preset_test(),
                None,
                None,
                |accounts, _, contracts, _, _| {
                    let _ = genesis_tx.send((accounts, contracts));
                },
            )
            .await
            {
                eprintln!("mock httpd server: {error}");
            }
        });
    });

    let (accounts, contracts) = genesis_rx
        .await
        .context("mock node stopped before the genesis callback fired")?;
    mock_httpd_wait_for_server_ready(port).await?;
    let client = HttpClient::new(format!("http://127.0.0.1:{port}"))
        .context("building the broadcast client")?;
    Ok((client, accounts, contracts, port))
}
