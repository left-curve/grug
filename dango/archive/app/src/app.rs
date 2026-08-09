use {
    crate::projection_loop,
    anyhow::anyhow,
    dango_archive_block_source::BlockSource,
    dango_archive_httpd::{Configurator, HttpdConfig, serve},
    dango_archive_projection::{Committer, Projection},
    dango_archive_types::AnyResult,
    futures::future::select_all,
    sea_orm::DatabaseConnection,
    std::sync::Arc,
};

/// Top-level orchestrator: owns a single [`BlockSource`], the [`Committer`]
/// shared by all projections, and a fixed set of [`Projection`]s. Spawns the
/// source's `run()` task, one [`projection_loop`] per projection, and — when the
/// read API is enabled — assembles the httpd from the projections' own routes
/// and supervises it, then waits for all of them.
///
/// The read surface is **derived from the projections**: each projection owns
/// the read routes over its tables, so `run` collects every projection's
/// [`routes`](Projection::routes) and hands them to the
/// [`serve`](dango_archive_httpd::serve) task along with the shared
/// Postgres pool and block source. The app stays agnostic to actix — it gathers
/// the registrars and supervises one more future.
///
/// Construct with [`App::new`] and drive with [`App::run`].
pub struct App {
    source: Arc<dyn BlockSource>,
    committer: Arc<dyn Committer>,
    projections: Vec<Arc<dyn Projection>>,
    /// The read side's Postgres pool, injected into the read API as app data
    /// (the same database the committer writes).
    db: DatabaseConnection,
    /// Read-API config (`None` = ingest-only). When set, `run` builds the httpd
    /// from the projections' routes and supervises it alongside the ingest tasks.
    read_cfg: Option<HttpdConfig>,
}

impl App {
    pub fn new(
        source: Arc<dyn BlockSource>,
        committer: Arc<dyn Committer>,
        projections: Vec<Arc<dyn Projection>>,
        db: DatabaseConnection,
        read_cfg: Option<HttpdConfig>,
    ) -> Self {
        Self {
            source,
            committer,
            projections,
            db,
            read_cfg,
        }
    }

    /// Migrate storage, then spawn the source, all projection loops, and the
    /// read-API server (if enabled), and wait for them.
    ///
    /// Returns when any task finishes (cleanly or with error). The ingest tasks
    /// and the server loop forever, so in practice this only returns on a panic
    /// or an unrecoverable error from one of the tasks.
    pub async fn run(self) -> AnyResult<()> {
        // Boot: the committer derives every owner's migrations from the
        // registered projections and applies them before any task starts.
        self.committer.migrate(&self.projections).await?;

        let mut handles = Vec::with_capacity(self.projections.len() + 2);

        handles.push(tokio::spawn(self.source.clone().run()));

        for p in &self.projections {
            handles.push(tokio::spawn(projection_loop(
                p.clone(),
                self.source.clone(),
                self.committer.clone(),
            )));
        }

        // Serve the read API from the same process that ingests: build one
        // configurator that mounts every projection's `services()` scopes, run
        // the httpd over the shared pool + source, and supervise it as one more
        // task — so a server error stops the app too. The read surface is
        // derived from the registered projections, never enumerated twice; the
        // OpenAPI docs come from the same projections (`api_doc()`), so the
        // served spec always matches the mounted routes.
        if let Some(cfg) = self.read_cfg {
            let api_docs = self
                .projections
                .iter()
                .filter_map(|projection| projection.api_doc())
                .collect();
            let projections = self.projections.clone();
            let configure: Configurator = Arc::new(move |service_config| {
                for projection in &projections {
                    for scope in projection.services() {
                        service_config.service(scope);
                    }
                }
            });
            handles.push(tokio::spawn(serve(
                cfg,
                self.db.clone(),
                self.source.clone(),
                configure,
                api_docs,
            )));
        }

        // Whichever task returns first — a clean end or an error — tears the
        // others down, so no task is left detached on the runtime when one exits
        // (and a clean exit of a single task can't hang `run` waiting on the
        // others that loop forever). Mirrors `RemoteBlockSource::run`.
        let (result, _index, remaining) = select_all(handles).await;
        for handle in remaining {
            handle.abort();
        }
        match result {
            Ok(task_result) => task_result,
            Err(join_err) => Err(anyhow!("indexer task panicked: {join_err}")),
        }
    }
}
