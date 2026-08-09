use {
    crate::{config::HttpdConfig, error::ApiError},
    actix_cors::Cors,
    actix_web::{App, HttpResponse, HttpServer, Responder, dev::Service as _, rt::System, web},
    anyhow::{Context as _, bail},
    dango_archive_block_source::BlockSource,
    dango_archive_types::AnyResult,
    futures::future::BoxFuture,
    sea_orm::DatabaseConnection,
    std::sync::Arc,
    tokio::sync::oneshot,
    utoipa::OpenApi as _,
    utoipa_swagger_ui::SwaggerUi,
};

/// Mounts the projections' read routes onto an actix
/// [`ServiceConfig`](web::ServiceConfig).
///
/// The app builds one of these — a closure that, per worker, asks every
/// projection for its `services()` scopes and registers them — and hands it to
/// [`serve`], which calls it when it builds each worker's app (hence `Fn`). The
/// handlers the scopes mount reach the shared Postgres pool and block source
/// through actix app data, injected here, so this stays projection-agnostic: the
/// httpd never names a projection, it just applies whatever configurator the app
/// gives it. Mirrors the in-process indexer's `config_app`.
pub type Configurator = Arc<dyn Fn(&mut web::ServiceConfig) + Send + Sync>;

/// The OpenAPI document of the httpd's own built-in routes — the base every
/// projection's [`api_doc`](dango_archive_projection) fragment is merged into.
/// Derived from the same `#[utoipa::path]` annotations that sit on the
/// handlers, so the spec lives next to the code it describes.
#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "Dango Archive API",
        description = "REST read surface of the Dango archive: raw blocks from \
                       the block store, plus each projection's feeds. All feeds \
                       are newest-first and keyset-paginated (`first` / `after`, \
                       the page's `endCursor` rolls back in as the next `after`).",
    ),
    paths(block, latest_block, up)
)]
struct ApiDoc;

/// Serve the read API until the process stops, as a boxed task.
///
/// Builds an actix-web server from the shared read handles and the app's route
/// configurator: the Postgres pool and the block source are injected as app data
/// (handlers pull them with `web::Data`), the core `GET /blocks/{height}`,
/// `GET /blocks/latest`, and `GET /up` routes are mounted directly, and
/// `configure` adds the projections' feed scopes.
///
/// `api_docs` are the projections' OpenAPI fragments (gathered by the app the
/// same way the route configurator is); they are merged into the httpd's own
/// [`ApiDoc`] and the result is served as `GET /openapi.json`, with Swagger UI
/// on `GET /docs/` and a redirect from the base `GET /` — so the httpd stays
/// projection-agnostic for the docs exactly as it is for the routes.
///
/// Returned type-erased (`BoxFuture`) so the app supervises it like any other
/// task. actix-web's server future is `!Send`, so it runs on a dedicated thread
/// with its own actix `System`; the outcome comes back over a oneshot, and only
/// that `Send` receiver is held across the await — so the returned future is
/// `Send`.
pub fn serve(
    config: HttpdConfig,
    db: DatabaseConnection,
    source: Arc<dyn BlockSource>,
    configure: Configurator,
    api_docs: Vec<utoipa::openapi::OpenApi>,
) -> BoxFuture<'static, AnyResult<()>> {
    Box::pin(async move {
        let (tx, rx) = oneshot::channel();

        std::thread::Builder::new()
            .name("archive-httpd".to_string())
            .spawn(move || {
                let outcome =
                    System::new().block_on(serve_actix(config, db, source, configure, api_docs));
                // A gone receiver just means the app is already tearing down.
                let _ = tx.send(outcome);
            })
            .context("spawning the httpd thread")?;

        match rx.await {
            Ok(outcome) => outcome,
            Err(_) => bail!("httpd thread terminated without reporting an outcome"),
        }
    })
}

/// The full OpenAPI document: the httpd's own [`ApiDoc`] with every
/// projection's fragment merged in.
fn merged_api_doc(api_docs: Vec<utoipa::openapi::OpenApi>) -> utoipa::openapi::OpenApi {
    let mut doc = ApiDoc::openapi();
    for fragment in api_docs {
        doc.merge(fragment);
    }
    doc
}

/// Mount the routes and drive the actix server. Runs on the dedicated httpd
/// thread, inside its actix `System`.
async fn serve_actix(
    config: HttpdConfig,
    db: DatabaseConnection,
    source: Arc<dyn BlockSource>,
    configure: Configurator,
    api_docs: Vec<utoipa::openapi::OpenApi>,
) -> AnyResult<()> {
    let bind = config.bind.clone();
    let api_doc = merged_api_doc(api_docs);

    #[cfg(feature = "tracing")]
    tracing::info!(%bind, "archive httpd listening");

    HttpServer::new(move || {
        // The factory runs once per worker; clone the shared handles and the
        // route configurator into each worker's app.
        let configure = configure.clone();
        App::new()
            .app_data(web::Data::new(db.clone()))
            .app_data(web::Data::new(source.clone()))
            // A malformed argument is a 400 with our JSON envelope, uniformly:
            // actix's default for a bad path param is a 404 (and for a bad query
            // a plain-text 400), so map both onto `ApiError::bad_request`.
            .app_data(web::PathConfig::default().error_handler(|err, _req| {
                ApiError::bad_request(format!("invalid path parameter: {err}")).into()
            }))
            .app_data(web::QueryConfig::default().error_handler(|err, _req| {
                ApiError::bad_request(format!("invalid query parameter: {err}")).into()
            }))
            // Dev-permissive CORS for now; lock down via config later.
            .wrap(Cors::permissive())
            // End-to-end request stats: an in-flight gauge bracketing the call,
            // plus a count and a latency histogram. A no-op without `metrics`.
            .wrap_fn(|req, srv| {
                #[cfg(feature = "metrics")]
                let start = std::time::Instant::now();
                #[cfg(feature = "metrics")]
                metrics::gauge!(crate::metrics::HTTP_IN_FLIGHT).increment(1.0);
                let fut = srv.call(req);
                async move {
                    let res = fut.await;
                    #[cfg(feature = "metrics")]
                    {
                        metrics::gauge!(crate::metrics::HTTP_IN_FLIGHT).decrement(1.0);
                        metrics::counter!(crate::metrics::HTTP_REQUESTS).increment(1);
                        metrics::histogram!(crate::metrics::HTTP_REQUEST_DURATION)
                            .record(start.elapsed().as_secs_f64());
                    }
                    res
                }
            })
            // Core routes: the block reads (not a projection) and the liveness
            // probe. `/blocks/latest` must be registered before
            // `/blocks/{height}` so the literal segment wins the match (actix
            // matches in registration order; after `{height}`, "latest" would
            // be a failed u64 parse → 400).
            .route("/blocks/latest", web::get().to(latest_block))
            .route("/blocks/{height}", web::get().to(block))
            .route("/up", web::get().to(up))
            // The projections' feed scopes, mounted by the app's configurator.
            .configure(move |cfg| (*configure)(cfg))
            // The API docs: Swagger UI on `/docs/` over the merged OpenAPI
            // document (also served plain at `/openapi.json`), with the base
            // path redirecting there — hitting the service root in a browser
            // lands on the docs.
            .service(
                SwaggerUi::new("/docs/{_:.*}").url("/openapi.json", api_doc.clone()),
            )
            .route("/", web::get().to(docs_redirect))
    })
    // The app owns process signals; the server simply stops with the process.
    .disable_signals()
    .bind(&bind)
    .with_context(|| format!("binding httpd to {bind}"))?
    .run()
    .await
    .context("httpd server stopped with an error")?;

    Ok(())
}

// ---- handlers ----

/// The full block at `height` — metadata, transactions, and execution outcome
/// (`{ block, outcome }`) — read straight from the configured
/// [`BlockSource`]. `404` when the source does not hold that height (below its
/// backfill floor, or not yet ingested); the body is the canonical block JSON,
/// the same shape the node's `/block/full/{height}` route returns.
#[utoipa::path(
    get,
    path = "/blocks/{height}",
    tag = "block",
    summary = "Block by height",
    description = "The full block at `height` — `{ block, outcome }`, the same \
                   JSON the node's `/block/full/{height}` route serves.",
    params(
        ("height" = u64, Path, description = "Block height"),
    ),
    responses(
        (status = 200, description = "The block at that height, as `{ block, outcome }`"),
        (status = 404, description = "The source does not hold that height"),
    ),
)]
async fn block(
    source: web::Data<Arc<dyn BlockSource>>,
    height: web::Path<u64>,
) -> actix_web::Result<HttpResponse> {
    let block = source
        .get(height.into_inner())
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?;
    match block {
        Some(block) => Ok(HttpResponse::Ok().json(block)),
        None => Ok(HttpResponse::NotFound().finish()),
    }
}

/// The latest indexed block — the block at the source's **contiguous
/// frontier**: the highest `H` with every height in `[1, H]` stored, i.e. the
/// newest block that can be served *together with all the history below it*
/// (`GET /blocks/{height}` answers for every height up to this one). During a
/// backfill this climbs from the bottom and can trail the chain tip
/// the live tail is already storing above a gap; once the store is gap-free it
/// **is** the tip. The frontier is O(1) in-memory state owned by the block
/// store — no scan, no extra cache. `404` while the store holds no contiguous
/// prefix yet (a cold, still-empty archive); the body is the same
/// `{ block, outcome }` JSON as `/blocks/{height}`.
#[utoipa::path(
    get,
    path = "/blocks/latest",
    tag = "block",
    summary = "Latest indexed block",
    description = "The block at the source's contiguous frontier — the highest \
                   `H` with every height in `[1, H]` stored, i.e. the newest \
                   block servable together with all the history below it. \
                   During a backfill this trails the chain tip; once the store \
                   is gap-free it is the tip. Same `{ block, outcome }` shape \
                   as `/blocks/{height}`.",
    responses(
        (status = 200, description = "The block at the contiguous frontier"),
        (status = 404, description = "The store holds no contiguous prefix yet (cold, still-empty archive)"),
    ),
)]
async fn latest_block(source: web::Data<Arc<dyn BlockSource>>) -> actix_web::Result<HttpResponse> {
    let frontier = source
        .contiguous_frontier()
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?;
    let Some(height) = frontier else {
        return Ok(HttpResponse::NotFound().finish());
    };
    // `h <= frontier ⟹ get(h) = Some` (the source persists before it
    // broadcasts, and the frontier only grows) — a miss here is a broken
    // invariant, not an absent resource, so it surfaces as a 500.
    let block = source
        .get(height)
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?
        .ok_or_else(|| {
            actix_web::error::ErrorInternalServerError(format!(
                "frontier {height} missing from the store"
            ))
        })?;
    Ok(HttpResponse::Ok().json(block))
}

/// Liveness probe.
#[utoipa::path(
    get,
    path = "/up",
    tag = "meta",
    summary = "Liveness probe",
    responses(
        (status = 200, description = "The service is up", body = serde_json::Value,
         example = json!({ "status": "ok" })),
    ),
)]
async fn up() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({ "status": "ok" }))
}

/// `GET /` — the base path lands on the API docs.
async fn docs_redirect() -> HttpResponse {
    HttpResponse::Found()
        .insert_header(("location", "/docs/"))
        .finish()
}

// ---- tests ----

#[cfg(test)]
mod tests {
    use {
        super::*,
        actix_web::{App, test},
        dango_archive_types::BlockData,
        dango_primitives::{Block, BlockInfo, BlockOutcome, Hash256, Timestamp},
        tokio::sync::broadcast,
    };

    /// A `BlockSource` with a scripted frontier: `get` serves any height iff
    /// `holds_blocks` — cleared to fake the impossible "frontier names a height
    /// the store lost" state, so the 500 path is pinned too.
    struct StubSource {
        frontier: Option<u64>,
        holds_blocks: bool,
    }

    #[async_trait::async_trait]
    impl BlockSource for StubSource {
        async fn run(self: Arc<Self>) -> AnyResult<()> {
            Ok(())
        }

        async fn get(&self, height: u64) -> AnyResult<Option<BlockData>> {
            Ok(self.holds_blocks.then(|| BlockData {
                block: Block {
                    info: BlockInfo {
                        height,
                        timestamp: Timestamp::from_nanos(0),
                        hash: Hash256::ZERO,
                    },
                    txs: vec![],
                },
                outcome: BlockOutcome {
                    height,
                    app_hash: Hash256::ZERO,
                    cron_outcomes: vec![],
                    tx_outcomes: vec![],
                },
            }))
        }

        fn subscribe(&self) -> broadcast::Receiver<Arc<BlockData>> {
            broadcast::channel(1).1
        }

        async fn contiguous_frontier(&self) -> AnyResult<Option<u64>> {
            Ok(self.frontier)
        }
    }

    /// The two block routes over a stub source, mounted in the same order as
    /// [`serve_actix`] — `/blocks/latest` before `/blocks/{height}`, so the
    /// literal wins the match.
    fn block_routes(stub: StubSource) -> impl FnOnce(&mut web::ServiceConfig) {
        let source: Arc<dyn BlockSource> = Arc::new(stub);
        move |cfg| {
            cfg.app_data(web::Data::new(source))
                .route("/blocks/latest", web::get().to(latest_block))
                .route("/blocks/{height}", web::get().to(block));
        }
    }

    /// The happy path: `latest` resolves the frontier, serves that block (the
    /// literal route wins over `{height}`), and the by-height route still
    /// works.
    #[actix_web::test]
    async fn latest_block_serves_the_frontier() {
        let app = test::init_service(App::new().configure(block_routes(StubSource {
            frontier: Some(7),
            holds_blocks: true,
        })))
        .await;

        let req = test::TestRequest::get().uri("/blocks/latest").to_request();
        let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;
        assert_eq!(body["block"]["info"]["height"].as_u64(), Some(7));

        let req = test::TestRequest::get().uri("/blocks/3").to_request();
        let resp = test::call_service(&app, req).await;
        assert!(
            resp.status().is_success(),
            "the by-height route still works"
        );
    }

    /// A cold, still-empty archive has no contiguous prefix: `latest` is a 404,
    /// like any absent resource.
    #[actix_web::test]
    async fn latest_block_404_when_the_store_is_empty() {
        let app = test::init_service(App::new().configure(block_routes(StubSource {
            frontier: None,
            holds_blocks: true,
        })))
        .await;

        let req = test::TestRequest::get().uri("/blocks/latest").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 404);
    }

    /// A frontier pointing at a height the store cannot serve violates
    /// `h <= frontier ⟹ get(h) = Some` — an internal error (500), never a 404
    /// that would read as "no latest block".
    #[actix_web::test]
    async fn latest_block_500_when_the_frontier_block_is_missing() {
        let app = test::init_service(App::new().configure(block_routes(StubSource {
            frontier: Some(7),
            holds_blocks: false,
        })))
        .await;

        let req = test::TestRequest::get().uri("/blocks/latest").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 500);
    }

    /// The docs surface, mounted as [`serve_actix`] mounts it: the merged spec
    /// is served at `/openapi.json` (here with no projection fragments — just
    /// the core paths), Swagger UI answers on `/docs/`, and the base path
    /// redirects there.
    #[actix_web::test]
    async fn docs_are_served_from_the_base_path() {
        let app = test::init_service(
            App::new()
                .service(
                    SwaggerUi::new("/docs/{_:.*}").url("/openapi.json", merged_api_doc(vec![])),
                )
                .route("/", web::get().to(docs_redirect)),
        )
        .await;

        // The base path lands on the docs.
        let req = test::TestRequest::get().uri("/").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 302);
        assert_eq!(
            resp.headers().get("location").unwrap().to_str().unwrap(),
            "/docs/"
        );

        // The UI itself.
        let req = test::TestRequest::get().uri("/docs/").to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success(), "swagger ui should be served");

        // The spec carries the httpd's own routes.
        let req = test::TestRequest::get().uri("/openapi.json").to_request();
        let spec: serde_json::Value = test::call_and_read_body_json(&app, req).await;
        for path in ["/blocks/{height}", "/blocks/latest", "/up"] {
            assert!(
                spec["paths"].get(path).is_some(),
                "the spec should document {path}",
            );
        }
    }
}
