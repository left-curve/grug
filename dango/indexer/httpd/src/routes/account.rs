use {
    crate::context::MinimalContext,
    actix_web::{
        Error, HttpResponse, Scope,
        error::{ErrorBadRequest, ErrorInternalServerError, ErrorServiceUnavailable},
        get, web,
    },
    dango_primitives::{Addr, ByteArray, Denom, Json, JsonDeExt, Query},
    // The account module is imported under an alias: the actix macro on the
    // `account` handler below generates a unit struct of that name, which
    // would collide with the module.
    dango_types::{account::QueryMsg as AccountQueryMsg, account_factory},
    serde::{Deserialize, Serialize},
    utoipa::IntoParams,
};

/// Routes under `/account` — GET aliases for account-related queries, all
/// keyed by an account address in the path.
///
/// Casing convention (as elsewhere): URL path segments are kebab-case; query
/// parameters keep the snake_case spelling and string encoding of the
/// contract wire fields they forward to.
///
/// Unlike the `/perps` aliases, which all target the one perps contract, the
/// routes here resolve the target of the query in three different ways:
///
/// - `/{address}` and `/{address}/user` are `wasm_smart` queries to the
///   account factory, whose address comes from the chain's app config
///   ([`MinimalContext::account_factory_address`]).
/// - `/{address}/seen-nonces` and `/{address}/session-seen-nonces` are
///   `wasm_smart` queries to the account contract at `{address}` itself.
/// - `/{address}/balances` is the chain-level `balances` query — no contract
///   involved at all.
pub fn services() -> Scope {
    web::scope("/account")
        .service(account)
        .service(user)
        .service(seen_nonces)
        .service(session_seen_nonces)
        .service(balances)
}

/// Run a `wasm_smart` query against the given contract and return the raw
/// contract response. A failed query is a 400 carrying the error message,
/// mirroring `POST /query`.
async fn query_wasm_smart<M>(
    app_ctx: &MinimalContext,
    contract: Addr,
    msg: &M,
) -> Result<Json, Error>
where
    M: Serialize,
{
    let query = Query::wasm_smart(contract, msg).map_err(ErrorInternalServerError)?;

    let (response, _) = app_ctx
        .dango_app
        .query_app(query)
        .await
        .map_err(|err| ErrorBadRequest(err.to_string()))?;

    Ok(response.into_wasm_smart())
}

/// Resolve the account factory address, mapping a failure to a 503 — the
/// chain may not have committed its genesis state yet; resolution is retried
/// on the next request.
async fn factory_address(app_ctx: &MinimalContext) -> Result<Addr, Error> {
    app_ctx.account_factory_address().await.map_err(|err| {
        ErrorServiceUnavailable(format!(
            "failed to resolve the account factory address: {err}"
        ))
    })
}

#[utoipa::path(
    get,
    path = "/account/{address}",
    tag = "account",
    summary = "Account parameters",
    description = "Parameters of the account at the given address: its \
                   account index and the index of the user who owns it. \
                   Alias of the account factory's `account` query; the \
                   response is the factory's `Account` object. An address \
                   not registered in the factory fails the query (a 400 \
                   carrying the contract's error).",
    params(
        ("address" = String, Path, description = "Account address"),
    ),
    responses(
        (status = 200, description = "The factory's `Account` object", body = serde_json::Value),
        (status = 400, description = "The query failed, e.g. no account with this address"),
        (status = 503, description = "The account factory address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/{address}")]
pub async fn account(
    path: web::Path<Addr>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let address = path.into_inner();
    let factory = factory_address(&app_ctx).await?;

    let response = query_wasm_smart(
        &app_ctx,
        factory,
        &account_factory::QueryMsg::Account { address },
    )
    .await?;

    Ok(HttpResponse::Ok().json(response))
}

#[utoipa::path(
    get,
    path = "/account/{address}/user",
    tag = "account",
    summary = "User owning an account",
    description = "The user who owns the account at the given address — \
                   which may be the user's master account or any of their \
                   subaccounts. The response is the factory's `User` object, \
                   verbatim: the user's index, username, keys, and all their \
                   accounts keyed by account index (the master account is \
                   the lowest index). Composed from the factory's `account` \
                   and `user` queries.",
    params(
        ("address" = String, Path, description = "Address of any of the user's accounts"),
    ),
    responses(
        (status = 200, description = "The factory's `User` object", body = serde_json::Value),
        (status = 400, description = "The query failed, e.g. no account with this address"),
        (status = 503, description = "The account factory address could not be resolved"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/{address}/user")]
pub async fn user(
    path: web::Path<Addr>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let address = path.into_inner();
    let factory = factory_address(&app_ctx).await?;

    // First resolve which user owns the account...
    let account_info: account_factory::Account = query_wasm_smart(
        &app_ctx,
        factory,
        &account_factory::QueryMsg::Account { address },
    )
    .await?
    .deserialize_json()
    .map_err(ErrorInternalServerError)?;

    // ...then return that user, verbatim.
    let response = query_wasm_smart(
        &app_ctx,
        factory,
        &account_factory::QueryMsg::User(account_factory::UserIndexOrName::Index(
            account_info.owner,
        )),
    )
    .await?;

    Ok(HttpResponse::Ok().json(response))
}

#[utoipa::path(
    get,
    path = "/account/{address}/seen-nonces",
    tag = "account",
    summary = "Seen transaction nonces",
    description = "The most recent transaction nonces recorded by the \
                   account at the given address for standard (master-key) \
                   credentials — needed to pick a valid nonce when building \
                   a transaction. Alias of the account contract's \
                   `seen_nonces` query; the response is an array of nonces. \
                   An address that is not an account contract fails the \
                   query (a 400).",
    params(
        ("address" = String, Path, description = "Account address"),
    ),
    responses(
        (status = 200, description = "The set of seen nonces, as a JSON array", body = serde_json::Value),
        (status = 400, description = "The query failed, e.g. the address is not an account contract"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/{address}/seen-nonces")]
pub async fn seen_nonces(
    path: web::Path<Addr>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let address = path.into_inner();

    let response = query_wasm_smart(&app_ctx, address, &AccountQueryMsg::SeenNonces {}).await?;

    Ok(HttpResponse::Ok().json(response))
}

#[utoipa::path(
    get,
    path = "/account/{address}/session-seen-nonces",
    tag = "account",
    summary = "Seen nonces of a session key",
    description = "The most recent transaction nonces recorded by the \
                   account at the given address for the given session key. \
                   Alias of the account contract's `session_seen_nonces` \
                   query; the response is an array of nonces.",
    params(
        ("address" = String, Path, description = "Account address"),
        SessionSeenNoncesQuery,
    ),
    responses(
        (status = 200, description = "The set of seen nonces, as a JSON array", body = serde_json::Value),
        (status = 400, description = "The query failed, e.g. the address is not an account contract"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/{address}/session-seen-nonces")]
pub async fn session_seen_nonces(
    path: web::Path<Addr>,
    query: web::Query<SessionSeenNoncesQuery>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let address = path.into_inner();
    let SessionSeenNoncesQuery { session_key } = query.into_inner();

    let response = query_wasm_smart(
        &app_ctx,
        address,
        &AccountQueryMsg::SessionSeenNonces { session_key },
    )
    .await?;

    Ok(HttpResponse::Ok().json(response))
}

#[utoipa::path(
    get,
    path = "/account/{address}/balances",
    tag = "account",
    summary = "Balances of an address",
    description = "Balances held by the given address in all denoms, as a \
                   map from denom to amount. Alias of the chain-level \
                   `balances` query — the address does not have to be a \
                   Dango account; any address works. Paginated: iteration \
                   starts after the `start_after` denom (exclusive), \
                   returning at most `limit` entries; the chain picks the \
                   defaults when omitted.",
    params(
        ("address" = String, Path, description = "Any address"),
        BalancesQuery,
    ),
    responses(
        (status = 200, description = "Map of denom to amount", body = serde_json::Value),
        (status = 400, description = "The query failed"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/{address}/balances")]
pub async fn balances(
    path: web::Path<Addr>,
    query: web::Query<BalancesQuery>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let address = path.into_inner();
    let BalancesQuery { start_after, limit } = query.into_inner();

    let (response, _) = app_ctx
        .dango_app
        .query_app(Query::balances(address, start_after, limit))
        .await
        .map_err(|err| ErrorBadRequest(err.to_string()))?;

    Ok(HttpResponse::Ok().json(response.into_balances()))
}

// ---- request types ----

#[derive(Deserialize, IntoParams)]
pub struct SessionSeenNoncesQuery {
    /// The session public key (33 bytes), in the same base64 encoding as on
    /// the JSON wire. URL-encode the value, as base64 may contain `+`, `/`
    /// and `=`.
    #[param(value_type = String)]
    session_key: ByteArray<33>,
}

#[derive(Deserialize, IntoParams)]
pub struct BalancesQuery {
    /// Denom after which iteration starts (exclusive). The chain starts from
    /// the beginning when omitted.
    #[param(value_type = Option<String>)]
    start_after: Option<Denom>,

    /// Maximum number of entries to return. The chain picks its default page
    /// limit when omitted.
    limit: Option<u32>,
}

// ---- tests ----

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::routes::mock_app::{MockApp, factory_addr, get, user_addr},
        actix_web::http::StatusCode,
        dango_primitives::{Coins, JsonSerExt, QueryBalancesRequest, json},
        dango_types::constants::usdc,
    };

    #[actix_web::test]
    async fn account_route_queries_the_factory() {
        let canned = json!({ "index": 12, "owner": 7 });
        let canned_clone = canned.clone();
        let (ctx, app) = MockApp::context(0, move |_, _| canned_clone.clone());

        let (status, body) = get(&ctx, &format!("/account/{}", user_addr())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, canned);

        assert_eq!(
            app.last_wasm_smart(),
            (
                factory_addr(),
                account_factory::QueryMsg::Account {
                    address: user_addr(),
                }
                .to_json_value()
                .unwrap(),
            ),
        );
    }

    #[actix_web::test]
    async fn user_route_composes_the_two_factory_queries() {
        let user_json = json!({ "index": 7, "name": "larry" });
        let user_json_clone = user_json.clone();

        // Answer the `account` query with an account owned by user 7, and the
        // `user` query with the canned user.
        let (ctx, app) = MockApp::context(0, move |_, msg| {
            if msg.get("account").is_some() {
                json!({ "index": 12, "owner": 7 })
            } else {
                user_json_clone.clone()
            }
        });

        let (status, body) = get(&ctx, &format!("/account/{}/user", user_addr())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, user_json);

        // Both queries went to the factory: first the account lookup, then
        // the user lookup with the owner index from the first response.
        assert_eq!(
            app.wasm_smart_requests(),
            vec![
                (
                    factory_addr(),
                    account_factory::QueryMsg::Account {
                        address: user_addr(),
                    }
                    .to_json_value()
                    .unwrap(),
                ),
                (
                    factory_addr(),
                    account_factory::QueryMsg::User(account_factory::UserIndexOrName::Index(7))
                        .to_json_value()
                        .unwrap(),
                ),
            ]
        );
    }

    #[actix_web::test]
    async fn nonce_routes_query_the_account_itself() {
        let (ctx, app) = MockApp::context(0, |_, _| json!([1, 2, 3]));

        let (status, body) = get(&ctx, &format!("/account/{}/seen-nonces", user_addr())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!([1, 2, 3]));

        // The queried contract is the account at the path address.
        assert_eq!(
            app.last_wasm_smart(),
            (
                user_addr(),
                AccountQueryMsg::SeenNonces {}.to_json_value().unwrap(),
            ),
        );

        // The session variant forwards the key, given in the wire's base64
        // encoding (URL-encoded).
        let session_key = ByteArray::<33>::from([7; 33]);
        let encoded = session_key
            .to_string()
            .replace('+', "%2B")
            .replace('/', "%2F")
            .replace('=', "%3D");

        let (status, _) = get(
            &ctx,
            &format!(
                "/account/{}/session-seen-nonces?session_key={encoded}",
                user_addr(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        assert_eq!(
            app.last_wasm_smart(),
            (
                user_addr(),
                AccountQueryMsg::SessionSeenNonces { session_key }
                    .to_json_value()
                    .unwrap(),
            ),
        );
    }

    #[actix_web::test]
    async fn balances_route_issues_the_chain_query() {
        // Note: the binding is named `coins`, not `balances`, as the actix
        // macro on the `balances` handler above generates a unit struct of
        // that name, which a `let balances` would be parsed against.
        let coins = Coins::one(usdc::DENOM.clone(), 100).unwrap();
        let (ctx, app) = MockApp::context_with_balances(0, coins.clone(), |_, _| Json::null());

        let (status, body) = get(
            &ctx,
            &format!(
                "/account/{}/balances?start_after={}&limit=5",
                user_addr(),
                usdc::DENOM.clone(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, coins.to_json_value().unwrap());

        assert_eq!(
            app.last_balances_request(),
            QueryBalancesRequest {
                address: user_addr(),
                start_after: Some(usdc::DENOM.clone()),
                limit: Some(5),
            }
        );
    }

    #[actix_web::test]
    async fn factory_resolution_failure_is_a_503_and_the_cache_is_shared() {
        let (ctx, app) = MockApp::context(1, |_, _| json!({}));

        // The first resolution attempt fails; nothing is cached.
        let (status, _) = get(&ctx, &format!("/account/{}", user_addr())).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

        // The next request retries and succeeds.
        let (status, _) = get(&ctx, &format!("/account/{}", user_addr())).await;
        assert_eq!(status, StatusCode::OK);

        // The perps routes share the same cache: no further app config query.
        let (status, _) = get(&ctx, "/perps/param").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(app.app_config_calls(), 2);
    }
}
