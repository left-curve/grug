//! The activity projection's read **scopes**, one module per resource —
//! mirroring the in-process indexer's `routes/` layout (`#[get]`-routed handlers
//! grouped in a `web::scope`).
//!
//! - [`transaction`] — the `/transactions` scope;
//! - [`events`] — the `/events` scope.
//!
//! [`scopes`] gathers them into the list the app mounts on the shared httpd. The
//! handlers reach the shared Postgres pool and block source through actix app
//! data, so a scope carries no per-request state and is cheap to rebuild per
//! worker; the one projection-specific input — the perps address anchoring the
//! `/events/perps` shortcut — rides as scope-local app data.

mod events;
mod transaction;

use {actix_web::Scope, dango_primitives::Addr};

/// Every read scope the activity projection exposes — `/transactions` and
/// `/events` (the latter carrying the `/events/perps` shortcut exactly when a
/// perps address was injected).
pub(crate) fn scopes(perps_contract: Option<Addr>) -> Vec<Scope> {
    let mut scopes = vec![transaction::services()];
    scopes.extend(events::services(perps_contract));
    scopes
}

/// The projection's OpenAPI fragment — the same resources [`scopes`] mounts
/// (including the conditional `/events/perps`), merged from each module's own
/// doc so the two derivations can't drift apart.
pub(crate) fn api_doc(perps_mounted: bool) -> utoipa::openapi::OpenApi {
    let mut doc = transaction::api_doc();
    doc.merge(events::api_doc(perps_mounted));
    doc
}

#[cfg(test)]
mod tests {
    use {
        super::scopes,
        actix_web::{
            App, test,
            web::{self, ServiceConfig},
        },
        dango_archive_block_source::BlockSource,
        dango_archive_httpd::ApiError,
        dango_archive_types::{AnyResult, BlockData},
        dango_primitives::Addr,
        sea_orm::Database,
        std::sync::Arc,
    };

    /// A `BlockSource` the rejection paths never call (they fail before
    /// hydration): the read handles only need to *exist* as app data so actix
    /// extraction reaches the handler.
    struct StubSource;

    #[async_trait::async_trait]
    impl BlockSource for StubSource {
        async fn run(self: Arc<Self>) -> AnyResult<()> {
            Ok(())
        }

        async fn get(&self, _height: u64) -> AnyResult<Option<BlockData>> {
            Ok(None)
        }

        fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Arc<BlockData>> {
            tokio::sync::broadcast::channel(1).1
        }

        async fn contiguous_frontier(&self) -> AnyResult<Option<u64>> {
            Ok(None)
        }
    }

    /// Mirror the read-API app the httpd assembles: the shared read handles
    /// plus the path / query error handlers that make a malformed argument a
    /// uniform 400 (without `PathConfig`, actix would 404 a bad path param).
    /// The SQLite handle is throwaway — the rejection paths never query it.
    async fn test_config(perps_contract: Option<Addr>) -> impl FnOnce(&mut ServiceConfig) {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect the throwaway db");
        let source: Arc<dyn BlockSource> = Arc::new(StubSource);
        move |cfg: &mut ServiceConfig| {
            cfg.app_data(web::Data::new(db))
                .app_data(web::Data::new(source))
                .app_data(web::PathConfig::default().error_handler(|err, _req| {
                    ApiError::bad_request(format!("invalid path parameter: {err}")).into()
                }))
                .app_data(web::QueryConfig::default().error_handler(|err, _req| {
                    ApiError::bad_request(format!("invalid query parameter: {err}")).into()
                }));
            for scope in scopes(perps_contract) {
                cfg.service(scope);
            }
        }
    }

    /// Every malformed or unanchored request is rejected with a **400** before
    /// any database query — exercising actix's path / query extraction and our
    /// own guardrails (the unknown-type / unanchored-`/events` / bad-cursor 400s
    /// short-circuit before the Postgres SQL runs, so a throwaway SQLite handle
    /// that is never queried suffices). The happy paths, which need real
    /// Postgres, are covered by the `backfill` integration test.
    #[actix_web::test]
    async fn rejects_malformed_or_unanchored_requests() {
        let app =
            test::init_service(App::new().configure(test_config(Some(Addr::mock(1))).await)).await;

        for path in [
            "/events",                                  // no anchor: neither type nor involved
            "/events?type=not_a_type",                  // unknown event type
            "/events?type=transfer&after=zz",           // malformed cursor (not hex)
            "/events?involved=not_an_address",          // malformed address (query)
            "/events/contract",                         // missing required contract
            "/events/contract?contract=not_an_address", // malformed address (query)
            "/events/perps?user=not_an_address",        // malformed address (query)
            "/events/perps?after=zz",                   // malformed cursor (not hex)
            "/transactions/not_a_hash",                 // malformed hash (path)
            "/transactions/involving/not_an_address",   // malformed address (path)
        ] {
            let req = test::TestRequest::get().uri(path).to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(
                resp.status().as_u16(),
                400,
                "GET {path} should be a 400, got {}",
                resp.status()
            );
        }
    }

    /// `/events/perps` exists only when an anchor address was injected: without
    /// one the route is not mounted at all (404), never a half-configured
    /// handler. The mounted case is pinned by the same path answering 400 (a
    /// parse rejection — proof the handler ran) in the test above.
    #[actix_web::test]
    async fn perps_shortcut_unmounted_without_an_injected_address() {
        let app = test::init_service(App::new().configure(test_config(None).await)).await;

        let req = test::TestRequest::get()
            .uri("/events/perps?after=zz")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(
            resp.status().as_u16(),
            404,
            "without an injected perps address the shortcut route must not exist",
        );
    }

    /// The OpenAPI fragment mirrors the mounted routes: the four always-on
    /// paths are documented unconditionally, `/events/perps` exactly when the
    /// shortcut is mounted. (`actix_web::test`, not the bare `#[test]`: the
    /// module's `use actix_web::{test, ...}` shadows the built-in attribute.)
    #[actix_web::test]
    async fn api_doc_mirrors_the_mounted_routes() {
        for (perps_mounted, expects_perps) in [(false, false), (true, true)] {
            let doc = super::api_doc(perps_mounted);
            for path in [
                "/transactions/{hash}",
                "/transactions/involving/{address}",
                "/events",
                "/events/contract",
            ] {
                assert!(
                    doc.paths.paths.contains_key(path),
                    "the fragment should document {path}",
                );
            }
            assert_eq!(
                doc.paths.paths.contains_key("/events/perps"),
                expects_perps,
                "/events/perps must be documented iff mounted (perps_mounted = {perps_mounted})",
            );

            // The response schemas referenced by the paths are auto-collected
            // into the components, so the spec is self-contained.
            let components = doc.components.as_ref().expect("components");
            for schema in ["Event", "Transaction", "EventType", "UnitKind"] {
                assert!(
                    components.schemas.contains_key(schema),
                    "the fragment should carry the `{schema}` schema",
                );
            }
        }
    }
}
