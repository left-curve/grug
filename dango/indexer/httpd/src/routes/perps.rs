use {
    crate::context::MinimalContext,
    actix_web::{
        Error, HttpResponse, Scope,
        error::{
            ErrorBadRequest, ErrorInternalServerError, ErrorNotFound, ErrorServiceUnavailable,
        },
        get, web,
    },
    dango_order_book::{ClientOrderId, OrderId, PairId, UsdPrice},
    dango_primitives::{Addr, Json, Query},
    dango_types::perps,
    serde::Deserialize,
    utoipa::IntoParams,
};

/// Routes under `/perps` — GET aliases for the perps contract's queries.
///
/// Each route mirrors one `wasm_smart` query to the perps contract, so that,
/// e.g. `GET /perps/liquidity-depth?pair_id=perp/ethusd&bucket_size=10` is the
/// same read as `POST /query` with body:
///
/// ```json
/// {
///   "wasm_smart": {
///     "contract": "<perps address>",
///     "msg": {
///       "liquidity_depth": {
///         "pair_id": "perp/ethusd",
///         "bucket_size": "10"
///       }
///     }
///   }
/// }
/// ```
///
/// The perps contract address is resolved from the chain's app config
/// server-side ([`MinimalContext::perps_address`]); clients never pass it.
/// Responses are the contract's response objects verbatim, so client-side
/// types written for the contract queries can be reused as-is.
///
/// Casing convention: URL path segments are kebab-case; query parameters
/// keep the snake_case spelling of the contract wire fields they forward to,
/// and take the same wire encoding as the JSON fields they alias (numbers
/// inside grug types are string-encoded, so `bucket_size=10` parses as a
/// `UsdPrice`; plain Rust integers such as `limit` are unquoted).
pub fn services() -> Scope {
    web::scope("/perps")
        .service(param)
        .service(pair_param)
        .service(pair_params)
        .service(state)
        .service(pair_state)
        .service(pair_states)
        .service(liquidity_depth)
        .service(user_state)
        // The literal `/order/...` routes are registered before
        // `/order/{order_id}`, so that "by-user" and "by-client-order-id" are
        // not captured as order IDs.
        .service(orders_by_user)
        .service(order_by_client_order_id)
        .service(order)
}

/// Run a `wasm_smart` query against the perps contract and return the raw
/// contract response.
///
/// Error mapping: the perps address not being resolvable is a 503 — the chain
/// may not have committed its genesis state yet, and resolution is retried on
/// the next request; a failed query is a 400 carrying the error message,
/// mirroring `POST /query`.
async fn query_perps(app_ctx: &MinimalContext, msg: &perps::QueryMsg) -> Result<Json, Error> {
    let contract = app_ctx.perps_address().await.map_err(|err| {
        ErrorServiceUnavailable(format!(
            "failed to resolve the perps contract address: {err}"
        ))
    })?;

    let query = Query::wasm_smart(contract, msg).map_err(ErrorInternalServerError)?;

    let (response, _) = app_ctx
        .dango_app
        .query_app(query)
        .await
        .map_err(|err| ErrorBadRequest(err.to_string()))?;

    Ok(response.into_wasm_smart())
}

/// Respond 200 with the JSON, or 404 if it is `null` — for the single-item
/// lookups whose contract response is an `Option`.
fn json_or_not_found(json: Json, what: String) -> Result<HttpResponse, Error> {
    if json.is_null() {
        Err(ErrorNotFound(format!("{what} not found")))
    } else {
        Ok(HttpResponse::Ok().json(json))
    }
}

// ---- global and pair-level queries ----

#[utoipa::path(
    get,
    path = "/perps/param",
    tag = "perps",
    summary = "Global parameters",
    description = "Global parameters of the perps contract: fee rate \
                   schedules, liquidation parameters, vault configuration, \
                   referral parameters, etc. Alias of the contract's `param` \
                   query; the response is the contract's `Param` object.",
    responses(
        (status = 200, description = "The contract's `Param` object", body = serde_json::Value),
        (status = 400, description = "The query failed"),
        (status = 503, description = "The perps contract address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/param")]
pub async fn param(app_ctx: web::Data<MinimalContext>) -> Result<HttpResponse, Error> {
    let response = query_perps(&app_ctx, &perps::QueryMsg::Param {}).await?;

    Ok(HttpResponse::Ok().json(response))
}

#[utoipa::path(
    get,
    path = "/perps/pair-param",
    tag = "perps",
    summary = "Pair parameters",
    description = "Parameters of a single trading pair: tick size, minimum \
                   order size, price band, open interest cap, margin ratios, \
                   liquidity depth bucket sizes, etc. Alias of the contract's \
                   `pair_param` query; the response is the contract's \
                   `PairParam` object.",
    params(PairIdQuery),
    responses(
        (status = 200, description = "The contract's `PairParam` object", body = serde_json::Value),
        (status = 400, description = "The query failed"),
        (status = 404, description = "No pair with this ID"),
        (status = 503, description = "The perps contract address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/pair-param")]
pub async fn pair_param(
    query: web::Query<PairIdQuery>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let response = query_perps(&app_ctx, &query.to_pair_param_msg()).await?;

    json_or_not_found(response, format!("pair `{}`", query.pair_id))
}

#[utoipa::path(
    get,
    path = "/perps/pair-params",
    tag = "perps",
    summary = "Enumerate pair parameters",
    description = "Parameters of all trading pairs, as a map from pair ID to \
                   `PairParam`. Alias of the contract's `pair_params` query. \
                   Paginated: iteration starts after `start_after`, returning \
                   at most `limit` entries; the contract picks the defaults \
                   when omitted.",
    params(PairsPageQuery),
    responses(
        (status = 200, description = "Map of pair ID to the contract's `PairParam` object", body = serde_json::Value),
        (status = 400, description = "The query failed"),
        (status = 503, description = "The perps contract address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/pair-params")]
pub async fn pair_params(
    query: web::Query<PairsPageQuery>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let PairsPageQuery { start_after, limit } = query.into_inner();

    let response = query_perps(
        &app_ctx,
        &perps::QueryMsg::PairParams { start_after, limit },
    )
    .await?;

    Ok(HttpResponse::Ok().json(response))
}

#[utoipa::path(
    get,
    path = "/perps/state",
    tag = "perps",
    summary = "Global state",
    description = "Global state of the perps contract: last funding time, \
                   vault share supply, insurance fund and treasury balances. \
                   Alias of the contract's `state` query; the response is the \
                   contract's `State` object.",
    responses(
        (status = 200, description = "The contract's `State` object", body = serde_json::Value),
        (status = 400, description = "The query failed"),
        (status = 503, description = "The perps contract address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/state")]
pub async fn state(app_ctx: web::Data<MinimalContext>) -> Result<HttpResponse, Error> {
    let response = query_perps(&app_ctx, &perps::QueryMsg::State {}).await?;

    Ok(HttpResponse::Ok().json(response))
}

#[utoipa::path(
    get,
    path = "/perps/pair-state",
    tag = "perps",
    summary = "Pair state",
    description = "State of a single trading pair: long and short open \
                   interest, funding rate and accumulator, index price. Alias \
                   of the contract's `pair_state` query; the response is the \
                   contract's `PairState` object.",
    params(PairIdQuery),
    responses(
        (status = 200, description = "The contract's `PairState` object", body = serde_json::Value),
        (status = 400, description = "The query failed"),
        (status = 404, description = "No pair with this ID"),
        (status = 503, description = "The perps contract address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/pair-state")]
pub async fn pair_state(
    query: web::Query<PairIdQuery>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let response = query_perps(&app_ctx, &query.to_pair_state_msg()).await?;

    json_or_not_found(response, format!("pair `{}`", query.pair_id))
}

#[utoipa::path(
    get,
    path = "/perps/pair-states",
    tag = "perps",
    summary = "Enumerate pair states",
    description = "States of all trading pairs, as a map from pair ID to \
                   `PairState`. Alias of the contract's `pair_states` query. \
                   Paginated: iteration starts after `start_after`, returning \
                   at most `limit` entries; the contract picks the defaults \
                   when omitted.",
    params(PairsPageQuery),
    responses(
        (status = 200, description = "Map of pair ID to the contract's `PairState` object", body = serde_json::Value),
        (status = 400, description = "The query failed"),
        (status = 503, description = "The perps contract address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/pair-states")]
pub async fn pair_states(
    query: web::Query<PairsPageQuery>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let PairsPageQuery { start_after, limit } = query.into_inner();

    let response = query_perps(
        &app_ctx,
        &perps::QueryMsg::PairStates { start_after, limit },
    )
    .await?;

    Ok(HttpResponse::Ok().json(response))
}

#[utoipa::path(
    get,
    path = "/perps/liquidity-depth",
    tag = "perps",
    summary = "Order book depth",
    description = "Aggregated order book depth of one trading pair at one of \
                   its pre-configured bucket sizes (see `bucket_sizes` in the \
                   pair's `PairParam`). Alias of the contract's \
                   `liquidity_depth` query; the response is the contract's \
                   `LiquidityDepthResponse` object: `bids` and `asks` maps \
                   from bucket price to aggregated size and notional. Bids \
                   are best (highest) first when iterated in descending key \
                   order; asks best (lowest) first in ascending key order.",
    params(LiquidityDepthQuery),
    responses(
        (status = 200, description = "The contract's `LiquidityDepthResponse` object", body = serde_json::Value),
        (status = 400, description = "The query failed, e.g. unknown pair or `bucket_size` not one of the pair's configured bucket sizes"),
        (status = 503, description = "The perps contract address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/liquidity-depth")]
pub async fn liquidity_depth(
    query: web::Query<LiquidityDepthQuery>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let response = query_perps(&app_ctx, &query.to_query_msg()).await?;

    Ok(HttpResponse::Ok().json(response))
}

// ---- user-level queries ----

#[utoipa::path(
    get,
    path = "/perps/user-state",
    tag = "perps",
    summary = "User state",
    description = "State of one user: margin, vault shares, pending unlocks, \
                   reserved margin, open order count, and positions (each \
                   with its attached conditional orders). Alias of the \
                   contract's `user_state_extended` query; the response is \
                   the contract's `UserStateExtended` object. The `include_*` \
                   flags opt into fields computed on the fly — equity, \
                   available margin, maintenance margin, and per-position \
                   unrealized PnL, unrealized funding and liquidation price; \
                   fields not requested are `null`. `include_all=true` \
                   computes everything, overriding the individual flags.",
    params(UserStateQuery),
    responses(
        (status = 200, description = "The contract's `UserStateExtended` object", body = serde_json::Value),
        (status = 400, description = "The query failed, e.g. the user has no state in the perps contract"),
        (status = 503, description = "The perps contract address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/user-state")]
pub async fn user_state(
    query: web::Query<UserStateQuery>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let response = query_perps(&app_ctx, &query.to_query_msg()).await?;

    Ok(HttpResponse::Ok().json(response))
}

// ---- order queries ----

#[utoipa::path(
    get,
    path = "/perps/order/by-user",
    tag = "perps",
    summary = "Open orders of a user",
    description = "All resting limit orders of one user, as a map from \
                   system-assigned order ID to the order. Alias of the \
                   contract's `orders_by_user` query. Unpaginated: a user's \
                   open orders are bounded by the `max_open_orders` global \
                   parameter.",
    params(OrdersByUserQuery),
    responses(
        (status = 200, description = "Map of order ID to the contract's `QueryOrdersByUserResponseItem` object", body = serde_json::Value),
        (status = 400, description = "The query failed"),
        (status = 503, description = "The perps contract address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/order/by-user")]
pub async fn orders_by_user(
    query: web::Query<OrdersByUserQuery>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let response = query_perps(&app_ctx, &query.to_query_msg()).await?;

    Ok(HttpResponse::Ok().json(response))
}

#[utoipa::path(
    get,
    path = "/perps/order/by-client-order-id",
    tag = "perps",
    summary = "Open order by client order ID",
    description = "The resting limit order of one user carrying the given \
                   client-assigned order ID (unique among a user's resting \
                   orders). Alias of the contract's `order_by_client_order_id` \
                   query; the response is the contract's \
                   `QueryOrderByClientOrderIdResponse` object, which includes \
                   the system-assigned `order_id` — the piece of information \
                   this lookup typically serves. Only resting orders are \
                   found.",
    params(OrderByClientOrderIdQuery),
    responses(
        (status = 200, description = "The contract's `QueryOrderByClientOrderIdResponse` object", body = serde_json::Value),
        (status = 400, description = "The query failed"),
        (status = 404, description = "No resting order of this user carries this client order ID"),
        (status = 503, description = "The perps contract address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/order/by-client-order-id")]
pub async fn order_by_client_order_id(
    query: web::Query<OrderByClientOrderIdQuery>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let OrderByClientOrderIdQuery {
        user,
        client_order_id,
    } = query.into_inner();

    let response = query_perps(
        &app_ctx,
        &perps::QueryMsg::OrderByClientOrderId {
            user,
            client_order_id,
        },
    )
    .await?;

    json_or_not_found(
        response,
        format!("order of user {user} with client order ID {client_order_id}"),
    )
}

#[utoipa::path(
    get,
    path = "/perps/order/{order_id}",
    tag = "perps",
    summary = "Open order by ID",
    description = "One resting limit order by its system-assigned order ID. \
                   Alias of the contract's `order` query; the response is the \
                   contract's `QueryOrderResponse` object. Only resting \
                   orders are found — for filled or canceled orders, use the \
                   perps event feeds.",
    params(
        ("order_id" = String, Path, description = "System-assigned order ID (a string-encoded integer)"),
    ),
    responses(
        (status = 200, description = "The contract's `QueryOrderResponse` object", body = serde_json::Value),
        (status = 400, description = "The query failed"),
        (status = 404, description = "No resting order with this ID"),
        (status = 503, description = "The perps contract address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/order/{order_id}")]
pub async fn order(
    path: web::Path<OrderId>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let order_id = path.into_inner();

    let response = query_perps(&app_ctx, &perps::QueryMsg::Order { order_id }).await?;

    json_or_not_found(response, format!("order {order_id}"))
}

// ---- request/response types ----

#[derive(Debug, Deserialize, IntoParams)]
pub struct PairIdQuery {
    /// Trading pair ID, e.g. `perp/ethusd`.
    #[param(value_type = String, example = "perp/ethusd")]
    pub(crate) pair_id: PairId,
}

impl PairIdQuery {
    /// The `pair_param` query this parameter set selects.
    fn to_pair_param_msg(&self) -> perps::QueryMsg {
        perps::QueryMsg::PairParam {
            pair_id: self.pair_id.clone(),
        }
    }

    /// The `pair_state` query this parameter set selects — shared with the
    /// WS `perpsPairState` subscription, so both desugar to the same wire
    /// message.
    pub(crate) fn to_pair_state_msg(&self) -> perps::QueryMsg {
        perps::QueryMsg::PairState {
            pair_id: self.pair_id.clone(),
        }
    }
}

#[derive(Deserialize, IntoParams)]
pub struct PairsPageQuery {
    /// Pair ID after which iteration starts (exclusive). The contract starts
    /// from the beginning when omitted.
    #[param(value_type = Option<String>)]
    start_after: Option<PairId>,

    /// Maximum number of entries to return. The contract picks its default
    /// page limit when omitted.
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct LiquidityDepthQuery {
    /// Trading pair ID, e.g. `perp/ethusd`.
    #[param(value_type = String, example = "perp/ethusd")]
    pub(crate) pair_id: PairId,

    /// Price bucket size; must be one of the pair's configured
    /// `bucket_sizes` (see the pair's `PairParam`).
    #[param(value_type = String, example = "10")]
    pub(crate) bucket_size: UsdPrice,

    /// Maximum number of buckets per side. The contract picks its default
    /// page limit when omitted.
    pub(crate) limit: Option<u32>,
}

impl LiquidityDepthQuery {
    /// The `liquidity_depth` query this parameter set selects — shared with
    /// the WS `perpsLiquidityDepth` subscription, so both desugar to the same
    /// wire message.
    pub(crate) fn to_query_msg(&self) -> perps::QueryMsg {
        perps::QueryMsg::LiquidityDepth {
            pair_id: self.pair_id.clone(),
            bucket_size: self.bucket_size,
            limit: self.limit,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct UserStateQuery {
    /// Account address.
    #[param(value_type = String)]
    pub(crate) user: Addr,

    /// Compute the user's equity.
    #[serde(default)]
    pub(crate) include_equity: bool,

    /// Compute the user's available margin.
    #[serde(default)]
    pub(crate) include_available_margin: bool,

    /// Compute the user's maintenance margin.
    #[serde(default)]
    pub(crate) include_maintenance_margin: bool,

    /// Compute each position's unrealized PnL.
    #[serde(default)]
    pub(crate) include_unrealized_pnl: bool,

    /// Compute each position's unrealized funding.
    #[serde(default)]
    pub(crate) include_unrealized_funding: bool,

    /// Compute each position's liquidation price.
    #[serde(default)]
    pub(crate) include_liquidation_price: bool,

    /// Compute all of the above, overriding the individual flags.
    #[serde(default)]
    pub(crate) include_all: bool,
}

impl UserStateQuery {
    /// The `user_state_extended` query this parameter set selects — shared
    /// with the WS `perpsUserState` subscription, so both desugar to the same
    /// wire message.
    pub(crate) fn to_query_msg(&self) -> perps::QueryMsg {
        perps::QueryMsg::UserStateExtended {
            user: self.user,
            include_equity: self.include_equity,
            include_available_margin: self.include_available_margin,
            include_maintenance_margin: self.include_maintenance_margin,
            include_unrealized_pnl: self.include_unrealized_pnl,
            include_unrealized_funding: self.include_unrealized_funding,
            include_liquidation_price: self.include_liquidation_price,
            include_all: self.include_all,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct OrdersByUserQuery {
    /// Account address.
    #[param(value_type = String)]
    pub(crate) user: Addr,
}

impl OrdersByUserQuery {
    /// The `orders_by_user` query this parameter set selects — shared with
    /// the WS `perpsOrdersByUser` subscription, so both desugar to the same
    /// wire message.
    pub(crate) fn to_query_msg(&self) -> perps::QueryMsg {
        perps::QueryMsg::OrdersByUser { user: self.user }
    }
}

#[derive(Deserialize, IntoParams)]
pub struct OrderByClientOrderIdQuery {
    /// Account address.
    #[param(value_type = String)]
    user: Addr,

    /// The client-assigned order ID (a string-encoded integer, unique among
    /// the user's resting orders).
    #[param(value_type = String, example = "42")]
    client_order_id: ClientOrderId,
}

// ---- tests ----

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::routes::mock_app::{MockApp, get, perps_addr, user_addr},
        actix_web::http::StatusCode,
        dango_order_book::{Quantity, QueryOrderByClientOrderIdResponse, UsdValue},
        dango_primitives::{JsonSerExt, Timestamp},
    };

    fn cid_order_response() -> QueryOrderByClientOrderIdResponse {
        QueryOrderByClientOrderIdResponse {
            order_id: OrderId::new(10),
            pair_id: "perp/ethusd".parse().unwrap(),
            size: Quantity::new_int(5),
            limit_price: UsdPrice::new_int(1_500),
            reduce_only: false,
            reserved_margin: UsdValue::new_int(750),
            created_at: Timestamp::from_seconds(1),
            tp: None,
            sl: None,
        }
    }

    #[actix_web::test]
    async fn aliases_query_the_perps_contract_and_cache_its_address() {
        let canned = dango_primitives::json!({ "max_open_orders": 100 });
        let canned_clone = canned.clone();
        let (ctx, app) = MockApp::context(0, move |_, _| canned_clone.clone());

        let (status, body) = get(&ctx, "/perps/param").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, canned);

        // The query went to the perps contract, carrying the `param` message.
        let (contract, msg) = app.last_wasm_smart();
        assert_eq!(contract, perps_addr());
        assert_eq!(msg, perps::QueryMsg::Param {}.to_json_value().unwrap());

        // A second request reuses the cached address: still one `app_config`
        // query.
        let (status, _) = get(&ctx, "/perps/param").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(app.app_config_calls(), 1);
    }

    #[actix_web::test]
    async fn failed_address_resolution_is_a_503_and_is_retried() {
        let (ctx, app) = MockApp::context(1, |_, _| Json::null());

        let (status, _) = get(&ctx, "/perps/state").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

        // The failure is not cached: the next request retries and succeeds.
        let (status, _) = get(&ctx, "/perps/state").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(app.app_config_calls(), 2);
    }

    #[actix_web::test]
    async fn single_item_lookups_respond_404_on_null() {
        let (ctx, app) = MockApp::context(0, |_, _| Json::null());

        let (status, _) = get(&ctx, "/perps/pair-param?pair_id=perp/ethusd").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = get(&ctx, "/perps/pair-state?pair_id=perp/ethusd").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = get(
            &ctx,
            &format!(
                "/perps/order/by-client-order-id?user={}&client_order_id=7",
                user_addr(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = get(&ctx, "/perps/order/123").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // The path parameter parsed into the order ID.
        let (_, msg) = app.last_wasm_smart();
        assert_eq!(
            msg,
            perps::QueryMsg::Order {
                order_id: OrderId::new(123),
            }
            .to_json_value()
            .unwrap(),
        );
    }

    #[actix_web::test]
    async fn orders_by_user_is_a_passthrough() {
        let (ctx, app) = MockApp::context(0, |_, _| dango_primitives::json!({}));

        let (status, _) = get(&ctx, &format!("/perps/order/by-user?user={}", user_addr())).await;
        assert_eq!(status, StatusCode::OK);

        let (_, msg) = app.last_wasm_smart();
        assert_eq!(
            msg,
            perps::QueryMsg::OrdersByUser { user: user_addr() }
                .to_json_value()
                .unwrap(),
        );
    }

    #[actix_web::test]
    async fn order_by_client_order_id_is_a_passthrough() {
        let (ctx, app) = MockApp::context(0, |_, _| cid_order_response().to_json_value().unwrap());

        let uri = format!(
            "/perps/order/by-client-order-id?user={}&client_order_id=7",
            user_addr(),
        );

        // The contract response passes through untouched.
        let (status, body) = get(&ctx, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, cid_order_response().to_json_value().unwrap());

        // The query params parsed into the contract query message.
        let (_, msg) = app.last_wasm_smart();
        assert_eq!(
            msg,
            perps::QueryMsg::OrderByClientOrderId {
                user: user_addr(),
                client_order_id: ClientOrderId::new(7),
            }
            .to_json_value()
            .unwrap(),
        );
    }

    #[actix_web::test]
    async fn user_state_forwards_the_include_flags() {
        let (ctx, app) = MockApp::context(0, |_, _| dango_primitives::json!({}));

        let (status, _) = get(
            &ctx,
            &format!(
                "/perps/user-state?user={}&include_equity=true&include_all=true",
                user_addr(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (_, msg) = app.last_wasm_smart();
        assert_eq!(
            msg,
            perps::QueryMsg::UserStateExtended {
                user: user_addr(),
                include_equity: true,
                include_available_margin: false,
                include_maintenance_margin: false,
                include_unrealized_pnl: false,
                include_unrealized_funding: false,
                include_liquidation_price: false,
                include_all: true,
            }
            .to_json_value()
            .unwrap(),
        );
    }

    #[actix_web::test]
    async fn liquidity_depth_forwards_typed_params() {
        let (ctx, app) = MockApp::context(0, |_, _| dango_primitives::json!({}));

        let (status, _) = get(
            &ctx,
            "/perps/liquidity-depth?pair_id=perp/ethusd&bucket_size=10&limit=5",
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (_, msg) = app.last_wasm_smart();
        assert_eq!(
            msg,
            perps::QueryMsg::LiquidityDepth {
                pair_id: "perp/ethusd".parse().unwrap(),
                bucket_size: UsdPrice::new_int(10),
                limit: Some(5),
            }
            .to_json_value()
            .unwrap(),
        );
    }
}
