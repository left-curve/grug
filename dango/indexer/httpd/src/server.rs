use {
    super::error::Error,
    crate::{
        context::FullContext,
        middlewares::shutdown::ShutdownMiddleware,
        routes::{
            self,
            graphql::{GraphqlRequestTimeout, graphql_route},
            index::index,
        },
        subscription_limiter::SubscriptionLimiter,
    },
    actix_cors::Cors,
    actix_files::Files,
    actix_web::{
        App, HttpServer, http,
        middleware::{Compress, Logger},
        web::{self, ServiceConfig},
    },
    dango_primitives::HttpdConfig,
    sentry_actix::Sentry,
    std::{
        sync::{Arc, atomic::AtomicBool, mpsc},
        time::Duration,
    },
    utoipa::OpenApi as _,
    utoipa_swagger_ui::SwaggerUi,
};
#[cfg(feature = "metrics")]
use {
    crate::{middlewares::metrics::init_httpd_metrics, query_memo::init_query_memo_metrics},
    actix_web_metrics::ActixWebMetricsBuilder,
};

/// The OpenAPI document of the node's HTTP API — the REST routes, plus
/// documentation-only entries for the `/ws` WebSocket and the deprecated
/// GraphQL endpoint (neither is representable in OpenAPI). Derived from the
/// `#[utoipa::path]` annotations that sit on the handlers, so the spec lives
/// next to the code it describes.
#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "Dango Node API",
        description = "HTTP API of a Dango consensus node: liveness, raw \
                       blocks from the node's block cache, chain state \
                       queries, transaction simulation and broadcast, plus \
                       realtime feeds on the `/ws` WebSocket. The GraphQL \
                       API (`POST /graphql`) is deprecated and will be \
                       removed; prefer the REST routes and `/ws`.",
    ),
    paths(
        crate::routes::index::up,
        crate::routes::index::requester_ip,
        crate::routes::blocks::latest_block_info,
        crate::routes::blocks::block_info_by_height,
        crate::routes::blocks::block_result,
        crate::routes::blocks::block_result_by_height,
        crate::routes::blocks::latest_full_block,
        crate::routes::blocks::full_block_range,
        crate::routes::blocks::full_block_by_height,
        crate::routes::query::query,
        crate::routes::simulate::simulate,
        crate::routes::broadcast::broadcast,
        crate::routes::perps::param,
        crate::routes::perps::pair_param,
        crate::routes::perps::pair_params,
        crate::routes::perps::state,
        crate::routes::perps::pair_state,
        crate::routes::perps::pair_states,
        crate::routes::perps::liquidity_depth,
        crate::routes::perps::user_state,
        crate::routes::perps::orders_by_user,
        crate::routes::perps::order_by_client_order_id,
        crate::routes::perps::order,
        crate::routes::account::account,
        crate::routes::account::user,
        crate::routes::account::seen_nonces,
        crate::routes::account::session_seen_nonces,
        crate::routes::account::balances,
        crate::routes::ws::ws_doc,
        crate::routes::graphql::graphql_doc,
    ),
    // Swagger UI renders the sections in the order the tags are declared
    // here (no `tagsSorter` is configured, so document order wins).
    tags(
        (name = "chain", description = "Chain reads and writes proxied to the node: \
                                        state queries, transaction simulation, \
                                        transaction broadcast"),
        (name = "meta", description = "Liveness and diagnostics"),
        (name = "block", description = "Raw blocks from the node's block cache"),
        (name = "account", description = "GET aliases for account-related queries, keyed \
                                          by account address: account parameters and the \
                                          owning user from the account factory, seen \
                                          nonces from the account contract itself, and \
                                          chain-level balances."),
        (name = "perps", description = "GET aliases for the perps contract's queries. \
                                       Each route mirrors one `wasm_smart` query to \
                                       the perps contract — same read as `POST /query`, \
                                       with the contract address resolved server-side \
                                       and the parameters taken from the query string. \
                                       Responses are the contract's response objects, \
                                       verbatim."),
        (name = "websocket", description = "Realtime feeds over a multiplexed WebSocket"),
        (name = "graphql", description = "Deprecated GraphQL API — scheduled for removal"),
    )
)]
struct ApiDoc;

pub fn config_app<G>(
    app_ctx: FullContext,
    graphql_schema: G,
    max_body_bytes: usize,
) -> Box<dyn Fn(&mut ServiceConfig)>
where
    G: Clone + 'static,
{
    // Built once per `config_app` call (`run_server`'s worker factory calls it
    // once per worker); cloned into the Swagger service each time the returned
    // closure runs.
    let api_doc = ApiDoc::openapi();

    Box::new(move |cfg: &mut ServiceConfig| {
        let mut service_config = cfg
            .service(index)
            .service(routes::index::up)
            .service(routes::index::requester_ip)
            .service(routes::index::sentry_raise)
            .service(routes::blocks::services())
            .service(routes::perps::services())
            .service(routes::account::services())
            .service(routes::ws::services())
            .service(routes::query::query)
            .service(routes::simulate::simulate)
            .service(routes::broadcast::broadcast)
            .service(graphql_route::<
                crate::graphql::query::FullQuery,
                crate::graphql::mutation::IndexerMutation,
                crate::graphql::subscription::FullSubscription,
            >())
            // The API docs: Swagger UI on `/docs/` over the OpenAPI document,
            // also served plain at `/openapi.json`. The base path `/` (the
            // `index` service above) redirects here.
            .service(SwaggerUi::new("/docs/{_:.*}").url("/openapi.json", api_doc.clone()));

        // Add static file serving if static_files_path is configured
        if let Some(static_path) = &app_ctx.static_files_path {
            #[cfg(feature = "tracing")]
            tracing::info!(static_path, "Exposing static files at /static");

            service_config = service_config.service(
                Files::new("/static", static_path)
                    .prefer_utf8(true)
                    .use_last_modified(true),
            );
        }

        service_config
            .default_service(web::to(routes::index::not_found_handler))
            .app_data(web::PayloadConfig::default().limit(max_body_bytes))
            .app_data(web::Data::new(app_ctx.db.clone()))
            .app_data(web::Data::new(app_ctx.base.clone()))
            .app_data(web::Data::new(app_ctx.clone()))
            .app_data(web::Data::new(graphql_schema.clone()));
    })
}

/// Run the full-mode HTTP server (indexer features enabled).
///
/// The shutdown_flag should be set when signals are received to return 503 for
/// new requests. Actix Web handles graceful shutdown automatically on
/// SIGTERM/SIGINT.
///
/// If `port_sender` is provided, the actual bound port will be sent via the
/// channel after binding. Use port 0 to let the OS allocate an available port
/// (useful for tests).
pub async fn run_server(
    httpd_config: &HttpdConfig,
    context: FullContext,
    shutdown_flag: Arc<AtomicBool>,
    port_sender: Option<mpsc::Sender<u16>>,
) -> Result<(), Error> {
    let graphql_schema = crate::graphql::build_full_schema(context.clone());

    #[cfg(feature = "tracing")]
    tracing::info!(
        httpd_config.ip,
        httpd_config.port,
        "Starting indexer httpd server"
    );

    #[cfg(feature = "metrics")]
    let metrics = ActixWebMetricsBuilder::new().build();

    #[cfg(feature = "metrics")]
    init_httpd_metrics();

    #[cfg(feature = "metrics")]
    init_query_memo_metrics();

    let subscription_limiter = SubscriptionLimiter::new(
        httpd_config.max_subscriptions_per_connection,
        httpd_config.max_subscriptions_global,
    );

    let graphql_request_timeout = GraphqlRequestTimeout(Duration::from_secs(
        httpd_config.graphql_request_timeout_secs,
    ));

    let cors_allowed_origin = httpd_config.cors_allowed_origin.clone();
    let graphql_max_body_bytes = httpd_config.graphql_max_body_bytes;
    let shutdown_flag_clone = shutdown_flag.clone();
    let server = HttpServer::new(move || {
        let mut cors = Cors::default()
            .allowed_methods(vec!["POST", "GET", "OPTIONS"])
            .allowed_headers(vec![
                http::header::AUTHORIZATION,
                http::header::ACCEPT,
                http::header::CONTENT_TYPE,
                http::header::HeaderName::from_static("sentry-trace"),
                http::header::HeaderName::from_static("baggage"),
            ])
            .max_age(3600);

        if let Some(origin) = cors_allowed_origin.as_deref() {
            for origin in origin.split(',') {
                cors = cors.allowed_origin(origin.trim());
            }
        } else {
            cors = cors.allow_any_origin();
        }

        let app = App::new()
            .wrap(ShutdownMiddleware::new(shutdown_flag_clone.clone()))
            .wrap(Sentry::new())
            .wrap(Logger::default())
            .wrap(Compress::default())
            .wrap(cors);

        #[cfg(feature = "metrics")]
        let app = app.wrap(metrics.clone());

        app.app_data(web::Data::new(subscription_limiter.clone()))
            .app_data(web::Data::new(graphql_request_timeout))
            .configure(config_app(
                context.clone(),
                graphql_schema.clone(),
                graphql_max_body_bytes,
            ))
    })
    .workers(httpd_config.workers)
    .max_connections(httpd_config.max_connections)
    .backlog(httpd_config.backlog)
    .keep_alive(actix_web::http::KeepAlive::Timeout(
        std::time::Duration::from_secs(httpd_config.keep_alive_secs),
    ))
    .client_request_timeout(std::time::Duration::from_secs(
        httpd_config.client_request_timeout_secs,
    ))
    .client_disconnect_timeout(std::time::Duration::from_secs(
        httpd_config.client_disconnect_timeout_secs,
    ))
    .worker_max_blocking_threads(httpd_config.worker_max_blocking_threads)
    .bind((&*httpd_config.ip, httpd_config.port))?;

    // Send the actual bound port if a channel was provided
    if let Some(sender) = port_sender
        && let Some(addr) = server.addrs().first()
    {
        let actual_port = addr.port();
        #[cfg(feature = "tracing")]
        tracing::info!(actual_port, "Server bound to port");
        let _ = sender.send(actual_port);
    }

    server.run().await?;

    Ok(())
}

// ---- tests ----

#[cfg(test)]
mod tests {
    use {super::*, actix_web::test};

    /// The docs surface, mounted as [`config_app`] mounts it: the OpenAPI spec
    /// is served at `/openapi.json` with every documented path, Swagger UI
    /// answers on `/docs/`, and the base path redirects there. Only the docs
    /// subset is mounted here — exercising the full `config_app` needs a real
    /// `FullContext`, which only the `dango-testing` harness can build (its
    /// `openapi_spec_is_served` test covers that half).
    #[actix_web::test]
    async fn docs_are_served_from_the_base_path() {
        let app = test::init_service(
            App::new()
                .service(SwaggerUi::new("/docs/{_:.*}").url("/openapi.json", ApiDoc::openapi()))
                .service(crate::routes::index::index),
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

        // The spec documents every route, including the documentation-only
        // `/ws` and `/graphql` entries.
        let req = test::TestRequest::get().uri("/openapi.json").to_request();
        let spec: serde_json::Value = test::call_and_read_body_json(&app, req).await;
        for path in [
            "/up",
            "/requester-ip",
            "/block/info",
            "/block/info/{block_height}",
            "/block/result",
            "/block/result/{block_height}",
            "/block/full",
            "/block/full/range",
            "/block/full/{block_height}",
            "/query",
            "/simulate",
            "/broadcast",
            "/perps/param",
            "/perps/pair-param",
            "/perps/pair-params",
            "/perps/state",
            "/perps/pair-state",
            "/perps/pair-states",
            "/perps/liquidity-depth",
            "/perps/user-state",
            "/perps/order/by-user",
            "/perps/order/by-client-order-id",
            "/perps/order/{order_id}",
            "/account/{address}",
            "/account/{address}/user",
            "/account/{address}/seen-nonces",
            "/account/{address}/session-seen-nonces",
            "/account/{address}/balances",
            "/ws",
            "/graphql",
        ] {
            assert!(
                spec["paths"].get(path).is_some(),
                "the spec should document {path}",
            );
        }

        // The GraphQL entry is flagged deprecated (rendered struck-through in
        // Swagger UI).
        assert_eq!(
            spec["paths"]["/graphql"]["post"]["deprecated"],
            serde_json::Value::Bool(true),
            "the graphql entry should be flagged deprecated",
        );

        // The sections appear in the intended order — Swagger UI renders
        // tags in document order.
        assert_eq!(
            spec["tags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tag| tag["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "chain",
                "meta",
                "block",
                "account",
                "perps",
                "websocket",
                "graphql",
            ],
        );
    }
}
