//! A mock [`QueryApp`](crate::traits::QueryApp) shared by the route alias
//! tests in this directory. Compiled only for tests: the module is
//! `#[cfg(test)]`-gated in `routes.rs`.

use {
    crate::{context::MinimalContext, traits::QueryApp},
    actix_web::{App, http::StatusCode, test, web},
    async_trait::async_trait,
    dango_app::{AppError, AppResult},
    dango_primitives::{
        Addr, BlockInfo, Coins, Json, JsonSerExt, Query, QueryBalancesRequest, QueryResponse,
        TxOutcome, UnsignedTx,
    },
    dango_types::config::{AppAddresses, AppConfig},
    std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

/// The perps contract address served by the mock's app config.
pub fn perps_addr() -> Addr {
    Addr::mock(9)
}

/// The account factory address served by the mock's app config.
pub fn factory_addr() -> Addr {
    Addr::mock(8)
}

/// An arbitrary user account address.
pub fn user_addr() -> Addr {
    Addr::mock(1)
}

/// A `QueryApp` that serves `app_config` (with [`perps_addr`] and
/// [`factory_addr`] as the contract addresses, after a configurable number of
/// failures), answers every `wasm_smart` query with the canned responder, and
/// answers `balances` queries with a canned coin set — recording the requests
/// for assertions.
pub struct MockApp {
    respond: Box<dyn Fn(Addr, &Json) -> Json + Send + Sync>,
    balances: Coins,
    app_config_calls: AtomicUsize,
    app_config_failures: AtomicUsize,
    wasm_smart_requests: Mutex<Vec<(Addr, Json)>>,
    last_balances_request: Mutex<Option<QueryBalancesRequest>>,
}

impl MockApp {
    /// A context whose responder receives `(contract, msg)` for every
    /// `wasm_smart` query.
    pub fn context<F>(app_config_failures: usize, respond: F) -> (MinimalContext, Arc<Self>)
    where
        F: Fn(Addr, &Json) -> Json + Send + Sync + 'static,
    {
        Self::context_with_balances(app_config_failures, Coins::new(), respond)
    }

    /// Same as [`Self::context`], with a canned response for `balances`
    /// queries.
    pub fn context_with_balances<F>(
        app_config_failures: usize,
        balances: Coins,
        respond: F,
    ) -> (MinimalContext, Arc<Self>)
    where
        F: Fn(Addr, &Json) -> Json + Send + Sync + 'static,
    {
        let app = Arc::new(Self {
            respond: Box::new(respond),
            balances,
            app_config_calls: AtomicUsize::new(0),
            app_config_failures: AtomicUsize::new(app_config_failures),
            wasm_smart_requests: Mutex::new(Vec::new()),
            last_balances_request: Mutex::new(None),
        });

        (MinimalContext::new(app.clone()), app)
    }

    /// Number of `app_config` queries served so far, for asserting the
    /// address cache.
    pub fn app_config_calls(&self) -> usize {
        self.app_config_calls.load(Ordering::SeqCst)
    }

    /// All `wasm_smart` requests received so far, as `(contract, msg)`.
    pub fn wasm_smart_requests(&self) -> Vec<(Addr, Json)> {
        self.wasm_smart_requests.lock().unwrap().clone()
    }

    /// The most recent `wasm_smart` request, as `(contract, msg)`.
    pub fn last_wasm_smart(&self) -> (Addr, Json) {
        self.wasm_smart_requests
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("no wasm_smart request recorded")
    }

    /// The most recent `balances` request.
    pub fn last_balances_request(&self) -> QueryBalancesRequest {
        self.last_balances_request
            .lock()
            .unwrap()
            .clone()
            .expect("no balances request recorded")
    }
}

#[async_trait]
impl QueryApp for MockApp {
    async fn query_app(&self, raw_req: Query) -> AppResult<(QueryResponse, u64)> {
        match raw_req {
            Query::AppConfig(_) => {
                self.app_config_calls.fetch_add(1, Ordering::SeqCst);

                // `fetch_update` is deprecated on nightly in favor of
                // `try_update`, which is not yet available on stable.
                #[allow(deprecated)]
                if self
                    .app_config_failures
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_ok()
                {
                    return Err(AppError::vm("app config not ready".to_string()));
                }

                let app_config = AppConfig {
                    addresses: AppAddresses {
                        perps: perps_addr(),
                        account_factory: factory_addr(),
                        ..Default::default()
                    },
                    ..Default::default()
                };

                Ok((QueryResponse::AppConfig(app_config.to_json_value()?), 1))
            },
            Query::WasmSmart(req) => {
                let response = (self.respond)(req.contract, &req.msg);

                self.wasm_smart_requests
                    .lock()
                    .unwrap()
                    .push((req.contract, req.msg));

                Ok((QueryResponse::WasmSmart(response), 1))
            },
            Query::Balances(req) => {
                *self.last_balances_request.lock().unwrap() = Some(req);

                Ok((QueryResponse::Balances(self.balances.clone()), 1))
            },
            _ => unimplemented!("not exercised by the route alias tests"),
        }
    }

    async fn simulate(&self, _unsigned_tx: UnsignedTx) -> AppResult<TxOutcome> {
        unimplemented!("not exercised by the route alias tests");
    }

    async fn chain_id(&self) -> AppResult<String> {
        unimplemented!("not exercised by the route alias tests");
    }

    async fn last_finalized_block(&self) -> AppResult<BlockInfo> {
        unimplemented!("not exercised by the route alias tests");
    }
}

/// GET `uri` against an app with the `/perps` and `/account` route groups
/// mounted, returning the status and the JSON body — `null` for the
/// plain-text bodies of error responses, so callers can assert on the status
/// alone.
pub async fn get(ctx: &MinimalContext, uri: &str) -> (StatusCode, Json) {
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(ctx.clone()))
            .service(crate::routes::perps::services())
            .service(crate::routes::account::services()),
    )
    .await;

    let response = test::call_service(&app, test::TestRequest::get().uri(uri).to_request()).await;
    let status = response.status();
    let body = test::read_body(response).await;

    let json = serde_json::from_slice(&body).unwrap_or(Json::null());

    (status, json)
}
