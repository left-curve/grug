use {
    crate::{
        config::{Config, GrugConfig, MetricsHttpdConfig, PythLazerConfig, TendermintConfig},
        home_directory::HomeDirectory,
        telemetry,
    },
    anyhow::anyhow,
    clap::Parser,
    dango_app::{
        AbciService, App, Db, HaltReason, Indexer, NaiveProposalPreparer, NullIndexer,
        SimpleCommitment,
    },
    dango_config_parser::parse_config,
    dango_db_disk::DiskDb,
    dango_genesis::GenesisCodes,
    dango_indexer_hooked::HookedIndexer,
    dango_indexer_httpd::TendermintRpcClient,
    dango_primitives::{GIT_COMMIT, HttpdConfig},
    dango_proposal_preparer::ProposalPreparer,
    dango_vm_rust::RustVm,
    metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle},
    std::sync::{Arc, atomic::AtomicBool},
    tokio::{
        signal::unix::{SignalKind, signal},
        sync::watch,
    },
    tower_abci::v038::{Server, split},
};

#[derive(Parser)]
pub struct StartCmd;

impl StartCmd {
    pub async fn run(self, app_dir: HomeDirectory) -> anyhow::Result<()> {
        tracing::info!("Using git commit: {GIT_COMMIT}");

        // Initialize metrics handler.
        // This should be done as soon as possible to capture all events.
        let metrics_handler = PrometheusBuilder::new().install_recorder()?;

        tracing::info!("Metrics handler initialized");

        // Parse the config file.
        let cfg: Config = parse_config(app_dir.config_file())?;

        // Emit startup logs now that the subscriber is initialized.
        if cfg.sentry.enabled {
            tracing::info!("Sentry initialized");
        } else {
            tracing::info!("Sentry is disabled");
        }
        if cfg.trace.enabled {
            tracing::info!(endpoint = %cfg.trace.endpoint, protocol = ?cfg.trace.protocol, "OpenTelemetry OTLP exporter initialized");
        } else {
            tracing::info!("OpenTelemetry OTLP exporter is disabled");
        }

        // Open disk DB.
        let db = DiskDb::<SimpleCommitment>::open_with_priority(
            app_dir.data_dir(),
            cfg.grug.priority_range.clone(),
        )?;

        // We need to call `RustVm::genesis_codes()` to properly build the contract wrappers.
        let _codes = RustVm::genesis_codes();

        // Create Rust VM.
        let vm = RustVm::new(
            // Below are parameters if we want to switch to `HybridVm`:
            // cfg.grug.wasm_cache_capacity,
            // [
            //     codes.account_factory.to_bytes().sha2_256(),
            //     codes.account_single.to_bytes().sha2_256(),
            //     codes.bank.to_bytes().sha2_256(),
            //     codes.gateway.to_bytes().sha2_256(),
            //     codes.hyperlane.ism.to_bytes().sha2_256(),
            //     codes.hyperlane.mailbox.to_bytes().sha2_256(),
            //     codes.hyperlane.va.to_bytes().sha2_256(),
            //     codes.oracle.to_bytes().sha2_256(),
            //     codes.vesting.to_bytes().sha2_256(),
            //     codes.warp.to_bytes().sha2_256(),
            // ]
        );

        // Create the base app instance for HTTP server
        let app = App::new(
            db.clone(),
            vm.clone(),
            NaiveProposalPreparer,
            NullIndexer,
            cfg.grug.query_gas_limit,
            None, // the `App` instance for use in httpd doesn't need the upgrade handler
            env!("CARGO_PKG_VERSION"),
        );

        let app = Arc::new(app);

        let (hooked_indexer, dango_httpd_context) = self
            .setup_indexer_stack(app_dir, &cfg, app.clone(), &cfg.tendermint.rpc_addr)
            .await?;

        let indexer_clone = hooked_indexer.clone();

        // Create shutdown flags for HTTP servers (to return 503 during shutdown)
        let httpd_shutdown_flag = Arc::new(AtomicBool::new(false));
        let httpd_shutdown_flags = vec![httpd_shutdown_flag.clone()];

        // Run the ABCI server — the indexer always runs, hooked into the app —
        // optionally with the httpd and metrics servers.
        //
        // Capture the result instead of `?`-propagating so we can *always*
        // wait for the indexer's pending post-indexing tasks to drain before
        // returning, even if one of the servers errored out (e.g. planned
        // halt propagated as an ABCI error).
        let run_result: anyhow::Result<()> = match (cfg.httpd.enabled, cfg.metrics_httpd.enabled) {
            (true, true) => {
                // HTTP server and metrics server both enabled
                tokio::try_join!(
                    Self::run_dango_httpd_server(
                        &cfg.httpd,
                        dango_httpd_context,
                        httpd_shutdown_flag.clone()
                    ),
                    Self::run_metrics_httpd_server(&cfg.metrics_httpd, metrics_handler),
                    self.run_with_indexer(
                        cfg.grug,
                        cfg.tendermint,
                        cfg.pyth,
                        db,
                        vm,
                        hooked_indexer.clone(),
                        hooked_indexer,
                        httpd_shutdown_flags
                    )
                )
                .map(|_| ())
            },
            (true, false) => {
                // HTTP server enabled, metrics disabled
                tokio::try_join!(
                    Self::run_dango_httpd_server(
                        &cfg.httpd,
                        dango_httpd_context,
                        httpd_shutdown_flag.clone()
                    ),
                    self.run_with_indexer(
                        cfg.grug,
                        cfg.tendermint,
                        cfg.pyth,
                        db,
                        vm,
                        hooked_indexer.clone(),
                        hooked_indexer,
                        httpd_shutdown_flags
                    )
                )
                .map(|_| ())
            },
            (false, true) => {
                // Metrics enabled, HTTP server disabled
                tokio::try_join!(
                    Self::run_metrics_httpd_server(&cfg.metrics_httpd, metrics_handler),
                    self.run_with_indexer(
                        cfg.grug,
                        cfg.tendermint,
                        cfg.pyth,
                        db,
                        vm,
                        hooked_indexer.clone(),
                        hooked_indexer,
                        vec![] // No HTTP server shutdown flags
                    )
                )
                .map(|_| ())
            },
            (false, false) => {
                // Neither HTTP server nor metrics server
                self.run_with_indexer(
                    cfg.grug,
                    cfg.tendermint,
                    cfg.pyth,
                    db,
                    vm,
                    hooked_indexer.clone(),
                    hooked_indexer,
                    vec![], // No HTTP server shutdown flags
                )
                .await
            },
        };

        // Always drain the indexer's in-flight post-indexing tasks before
        // returning, even if `run_result` is `Err`.
        if let Err(err) = indexer_clone.wait_for_finish().await {
            tracing::error!(%err, "Error waiting for indexer to finish");
        }

        run_result
    }

    /// Setup the hooked indexer with both SQL and Dango indexers, and prepare contexts for HTTP servers
    async fn setup_indexer_stack(
        &self,
        app_dir: HomeDirectory,
        cfg: &Config,
        app: Arc<App<DiskDb<SimpleCommitment>, RustVm, NaiveProposalPreparer, NullIndexer>>,
        tendermint_rpc_addr: &str,
    ) -> anyhow::Result<(HookedIndexer, dango_indexer_httpd::context::FullContext)> {
        let sql_indexer = dango_indexer_sql::IndexerBuilder::default()
            .with_database_url(&cfg.indexer.database.url)
            .with_database_max_connections(cfg.indexer.database.max_connections)
            .with_sqlx_pubsub()
            .build()
            .await
            .map_err(|err| anyhow!("failed to build indexer: {err:?}"))?;
        let sql_context = sql_indexer.context.clone();

        let clickhouse_context = dango_indexer_clickhouse::context::Context::new(
            cfg.indexer.clickhouse.url.clone(),
            cfg.indexer.clickhouse.database.clone(),
            cfg.indexer.clickhouse.user.clone(),
            cfg.indexer.clickhouse.password.clone(),
        );

        let clickhouse_indexer = dango_indexer_clickhouse::Indexer::new(clickhouse_context.clone());

        let indexer_cache = dango_indexer_cache::Cache::new_with_dir(app_dir.indexer_dir());
        let indexer_cache_context = indexer_cache.context.clone();

        let mut hooked_indexer = HookedIndexer::new(indexer_cache, sql_indexer, clickhouse_indexer);

        let dango_httpd_context = dango_indexer_httpd::context::FullContext::new(
            indexer_cache_context,
            sql_context,
            clickhouse_context,
            hooked_indexer.stream.context(),
            app.clone(),
            Arc::new(TendermintRpcClient::new(tendermint_rpc_addr)?),
            cfg.httpd.static_files_path.clone(),
        );

        let storage = app
            .db
            .state_storage_with_comment(None, "hooked_indexer")
            .map_err(|e| anyhow!("Failed to get state storage: {e}"))?;
        hooked_indexer
            .start(&storage)
            .await
            .map_err(|e| anyhow!("Failed to start indexer: {e}"))?;

        Ok((hooked_indexer, dango_httpd_context))
    }

    /// Run the full-featured HTTP server (with indexer features)
    /// The shutdown flag should be set when signals are received to return 503 for new requests.
    ///
    /// The HTTP port is bound immediately; the ClickHouse and perps trade
    /// cache preloads run concurrently in a background task. `/up` does not
    /// touch any of those caches (it only reads
    /// `dango_app.last_finalized_block()` and a Postgres blocks query), and
    /// the GraphQL handlers that do read from them already fall through to
    /// ClickHouse / return empty state on a cache miss, so handlers reading
    /// during warm-up see the same state as a freshly indexed node.
    async fn run_dango_httpd_server(
        cfg: &HttpdConfig,
        dango_httpd_context: dango_indexer_httpd::context::FullContext,
        shutdown_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<()> {
        tracing::info!(
            "Starting full-featured HTTP server at {}:{}",
            &cfg.ip,
            cfg.port
        );

        // Spawn the cache warm-up so it does not block the HTTP server from
        // binding. Both contexts and their cache fields are `Arc<RwLock<…>>`
        // under the hood, so the cloned context shares state with the one
        // handed to `run_server`.
        let warmup_ctx = dango_httpd_context.clone();
        tokio::spawn(async move {
            let warmup_start = std::time::Instant::now();
            metrics::gauge!("dango_httpd.cache_warmup.in_progress").set(1.0);

            tracing::info!("Starting dango HTTP cache warm-up");

            // The two preloads hit different databases (ClickHouse and
            // Postgres), so run them concurrently to halve wall-time.
            let (clickhouse_result, perps_trade_result) = tokio::join!(
                warmup_ctx.clickhouse_context.start_cache(),
                warmup_ctx.start_perps_trade_cache(),
            );

            if let Err(err) = clickhouse_result {
                tracing::warn!(
                    %err,
                    "ClickHouse cache preload failed; handlers will fall back to live queries"
                );
            }
            if let Err(err) = perps_trade_result {
                tracing::warn!(
                    %err,
                    "Perps trade cache preload failed; new subscribers will see empty initial state until the next trade"
                );
            }

            let elapsed = warmup_start.elapsed();
            metrics::gauge!("dango_httpd.cache_warmup.in_progress").set(0.0);
            metrics::histogram!("dango_httpd.cache_warmup.duration_seconds")
                .record(elapsed.as_secs_f64());

            tracing::info!(
                elapsed_secs = elapsed.as_secs_f64(),
                "Dango HTTP cache warm-up complete"
            );
        });

        dango_indexer_httpd::server::run_server(cfg, dango_httpd_context, shutdown_flag, None)
            .await
            .map_err(|err| {
                tracing::error!("Failed to run full-featured HTTP server: {err:?}");
                err.into()
            })
    }

    /// Run the metrics HTTP server
    async fn run_metrics_httpd_server(
        cfg: &MetricsHttpdConfig,
        metrics_handler: PrometheusHandle,
    ) -> anyhow::Result<()> {
        dango_indexer_metrics::run_metrics_server(&cfg.ip, cfg.port, metrics_handler)
            .await
            .map_err(|err| {
                tracing::error!("Failed to run metrics HTTP server: {err:?}");
                err.into()
            })
    }

    /// Reference:
    /// - Namada:
    ///   https://github.com/namada-net/namada/blob/v101.1.4/crates/node/src/lib.rs#L737-L774
    /// - Penumbra:
    ///   https://github.com/penumbra-zone/penumbra/blob/dafaa19109fd06b67cb294a097bad803ade4ac7c/crates/core/app/src/server.rs#L47-L73
    async fn run_with_indexer<ID>(
        self,
        grug_cfg: GrugConfig,
        tendermint_cfg: TendermintConfig,
        pyth_lazer_cfg: PythLazerConfig,
        db: DiskDb<SimpleCommitment>,
        vm: RustVm,
        indexer: ID,
        mut indexer_for_shutdown: ID,
        httpd_shutdown_flags: Vec<Arc<AtomicBool>>,
    ) -> anyhow::Result<()>
    where
        ID: Indexer + Send + Sync + 'static,
    {
        // Channel used by the app to request a graceful shutdown from inside
        // `finalize_block` (see `dango_app::HaltReason`). Initial value is
        // `None`; a `Some(reason)` means the app has requested a halt.
        let (halt_tx, mut halt_rx) = watch::channel::<Option<HaltReason>>(None);
        let halt_tx = Arc::new(halt_tx);

        // `AbciService` wraps the `App` in an `Arc` internally so each
        // per-request future captures a cheap `Arc::clone` instead of
        // cloning the `App` (which would clone `PythHandler` to a "dud"
        // copy and trigger `HookedIndexer::Drop` on every request).
        let service = AbciService::new(
            App::new(
                db,
                vm,
                ProposalPreparer::new(pyth_lazer_cfg.endpoints, pyth_lazer_cfg.access_token),
                indexer,
                grug_cfg.query_gas_limit,
                Some(dango_upgrade::do_upgrade), // Important: set the upgrade handler.
                env!("CARGO_PKG_VERSION"),
            )
            .with_shutdown_trigger(halt_tx)
            .with_retain_recent_blocks(tendermint_cfg.retain_recent_blocks),
        );

        let (consensus, mempool, snapshot, info) = split::service(service, 1);

        let abci_server = Server::builder()
            .consensus(consensus)
            .snapshot(snapshot)
            .mempool(mempool)
            .info(info)
            .finish()
            // Safety: the consensus, snapshot, mempool, and info services have all been provided
            // to the builder above.
            .expect("all components of abci have been provided");

        // Listen for SIGINT and SIGTERM signals.
        // SIGINT is received when user presses Ctrl-C.
        // SIGTERM is received when user does `systemctl stop`.
        let mut sigint = signal(SignalKind::interrupt())?;
        let mut sigterm = signal(SignalKind::terminate())?;

        let shutdown = async |indexer_for_shutdown: &mut ID| -> anyhow::Result<()> {
            // Set shutdown flags to return 503 for new HTTP requests
            for flag in &httpd_shutdown_flags {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }

            // Give a brief moment for the flags to propagate
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            if let Err(err) = indexer_for_shutdown.shutdown().await {
                tracing::error!(err = %err, "Error shutting down indexer");
            }

            telemetry::shutdown();
            telemetry::shutdown_sentry();

            Ok(())
        };

        // Wait for the ABCI server to exit, a signal, or an app-initiated halt
        // (e.g. scheduled upgrade with the wrong binary). We *always* run the
        // shutdown sequence afterwards, regardless of which arm fires.
        let select_result: anyhow::Result<()> = tokio::select! {
            result = async { abci_server.listen_tcp(tendermint_cfg.abci_addr).await } => {
                result.map_err(|err| anyhow!("failed to start ABCI server: {err:?}"))
            },
            _ = sigint.recv() => {
                tracing::info!("Received SIGINT, shutting down");
                Ok(())
            },
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM, shutting down");
                Ok(())
            },
            _ = halt_rx.changed() => {
                tracing::warn!(reason = ?halt_rx.borrow(), "App requested graceful shutdown");
                Ok(())
            },
        };

        // Always run the shutdown sequence, even if the ABCI server exited
        // with an error, so indexer writes and telemetry are flushed.
        if let Err(err) = shutdown(&mut indexer_for_shutdown).await {
            tracing::error!(%err, "Graceful shutdown failed");
        }

        // If the app itself requested the halt, treat it as a clean exit so
        // systemd doesn't restart us — the operator must deploy the correct
        // binary before the chain resumes. The ABCI server very likely
        // returned an error too (CometBFT disconnected after we rejected
        // `finalize_block`), but that error is *expected* in this case.
        if halt_rx.borrow().is_some() {
            // The sibling HTTP server futures in the outer `try_join!`
            // (`run_dango_httpd_server`, `run_metrics_httpd_server`) rely on
            // actix-web's built-in SIGTERM handler to shut down — on the
            // normal signal paths that happens automatically because the OS
            // delivers the signal to every listener. A planned halt has no
            // such signal, so we raise one ourselves: without it, actix
            // keeps accepting connections and `try_join!` deadlocks until
            // systemd SIGKILLs us, losing the graceful shutdown this path
            // was designed to provide.
            //
            // NOTE: this assumes `HttpServer` keeps its default signal
            // handling. If a future change calls `.disable_signals()`,
            // switch to an explicit `ServerHandle::stop(true)` wired via a
            // cancel signal.
            //
            // Safety: `libc::raise` is a thin wrapper over the POSIX
            // `raise(3)` syscall; it is async-signal-safe and has no
            // preconditions on the caller.
            unsafe { libc::raise(libc::SIGTERM) };
            return Ok(());
        }

        select_result
    }
}
