use {
    assert_json_diff::assert_json_include,
    assertor::*,
    dango_math::Uint64,
    dango_order_book::{OrderKind, Quantity, TimeInForce, UsdPrice},
    dango_primitives::{
        Addressable, Block, BlockOutcome, ByteArray, Coins, QuerierExt, ResultExt, btree_map,
        btree_set,
    },
    dango_testing::{
        TestOption, build_app_service, call_api, call_api_post, call_api_with_headers, pair_id,
        setup_perps_env, setup_test_naive_with_indexer,
        setup_test_naive_with_indexer_and_create_blocks,
    },
    dango_types::{account_factory, perps},
    serde_json::json,
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequesterIpResponse {
    remote_ip: Option<String>,
    peer_ip: Option<String>,
    x_forwarded_for: Option<String>,
    forwarded: Option<String>,
    cf_connecting_ip: Option<String>,
    true_client_ip: Option<String>,
    x_real_ip: Option<String>,
}

#[tokio::test(flavor = "multi_thread")]
async fn up_returns_200() -> anyhow::Result<()> {
    let (_, _, httpd_context, _db_guard) = setup_test_naive_with_indexer_and_create_blocks(
        TestOption::default().with_mocked_clickhouse(),
        1,
    )
    .await;

    let local_set = tokio::task::LocalSet::new();

    local_set
        .run_until(async {
            tokio::task::spawn_local(async {
                let app = build_app_service(httpd_context);

                let up_response = call_api::<serde_json::Value>(app, "/up").await?;

                let expected = json!({
                    "block": { "height": 1 },
                    "is_running": false,
                    "indexed_block_height": 1,
                    "chain_id": "",
                });

                assert_json_include!(actual: up_response, expected: expected);

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

#[tokio::test(flavor = "multi_thread")]
async fn api_returns_block() -> anyhow::Result<()> {
    let (_, _, httpd_context, _db_guard) = setup_test_naive_with_indexer_and_create_blocks(
        TestOption::default().with_mocked_clickhouse(),
        1,
    )
    .await;

    let local_set = tokio::task::LocalSet::new();

    local_set
        .run_until(async {
            tokio::task::spawn_local(async {
                let app = build_app_service(httpd_context.clone());

                let block = call_api::<Block>(app, "/block/info/1").await?;
                assert_that!(block.info.height).is_equal_to(1);

                let app = build_app_service(httpd_context.clone());

                let block = call_api::<Block>(app, "/block/info").await?;
                assert_that!(block.info.height).is_equal_to(1);

                let app = build_app_service(httpd_context.clone());

                let block_outcome = call_api::<BlockOutcome>(app, "/block/result/1").await?;
                assert_that!(block_outcome.cron_outcomes).is_empty();
                assert_that!(block_outcome.tx_outcomes).has_length(1);

                let app = build_app_service(httpd_context.clone());

                let block_outcome = call_api::<BlockOutcome>(app, "/block/result").await?;
                assert_that!(block_outcome.cron_outcomes).is_empty();
                assert_that!(block_outcome.tx_outcomes).has_length(1);

                let app = build_app_service(httpd_context);

                let block_outcome = call_api::<BlockOutcome>(app, "/block/result/2").await;
                assert_that!(block_outcome).is_err();

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// The real `config_app` (the same assembly `run_server` uses) serves the
/// OpenAPI spec: proof the docs are mounted in production, complementing the
/// httpd crate's in-crate test, which mounts only the docs subset.
#[tokio::test(flavor = "multi_thread")]
async fn openapi_spec_is_served() -> anyhow::Result<()> {
    let (_, _, _, _, _, httpd_context, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default().with_mocked_clickhouse()).await;

    let local_set = tokio::task::LocalSet::new();

    local_set
        .run_until(async {
            tokio::task::spawn_local(async {
                let app = build_app_service(httpd_context);

                let spec = call_api::<serde_json::Value>(app, "/openapi.json").await?;

                assert_eq!(spec["info"]["title"], "Dango Node API");

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

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// The `/perps/*` aliases: each returns exactly what the equivalent raw
/// `wasm_smart` query returns through `POST /query`, with the parameters
/// parsed from the URL.
#[tokio::test(flavor = "multi_thread")]
async fn perps_aliases_mirror_contract_queries() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, httpd_context, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default().with_mocked_clickhouse()).await;

    // Oracle prices ($2,000 ETH) and margin for user1 and user2.
    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 100_000).await;

    // The test genesis configures no liquidity depth bucket sizes; add one so
    // the `liquidity_depth` alias has something to return. Depth bookkeeping
    // tracks orders placed after the bucket size is configured, so this comes
    // before the orders. The current parameters are read back from the chain
    // and re-submitted with only `bucket_sizes` changed.
    let param: perps::Param = suite
        .query_wasm_smart(contracts.perps, perps::QueryParamRequest {})
        .should_succeed();
    let pair_param: Option<perps::PairParam> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryPairParamRequest { pair_id: pair_id() },
        )
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param,
                pair_params: btree_map! {
                    pair_id() => perps::PairParam {
                        bucket_sizes: btree_set! { UsdPrice::new_int(100) },
                        ..pair_param.expect("the test genesis should have the pair")
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Two resting bids for user1, priced below the oracle price so they rest
    // on the book rather than cross; the second carries a client order ID.
    for (price, client_order_id) in [(1_500, None), (1_400, Some(Uint64::new(42)))] {
        suite
            .execute(
                &mut accounts.user1,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(
                    perps::SubmitOrderRequest {
                        pair_id: pair_id(),
                        size: Quantity::new_int(5),
                        kind: OrderKind::Limit {
                            limit_price: UsdPrice::new_int(price),
                            time_in_force: TimeInForce::GoodTilCanceled,
                            client_order_id,
                        },
                        reduce_only: false,
                        tp: None,
                        sl: None,
                    },
                )),
                Coins::new(),
            )
            .await
            .should_succeed();
    }

    let user = accounts.user1.address();
    let perps_contract = contracts.perps;

    let local_set = tokio::task::LocalSet::new();

    local_set
        .run_until(async move {
            tokio::task::spawn_local(async move {
                // Parity: every alias returns what the equivalent raw query
                // returns.
                for (alias, msg) in [
                    ("/perps/param".to_string(), json!({ "param": {} })),
                    (
                        format!("/perps/pair-param?pair_id={}", pair_id()),
                        json!({ "pair_param": { "pair_id": pair_id() } }),
                    ),
                    (
                        "/perps/pair-params".to_string(),
                        json!({ "pair_params": {} }),
                    ),
                    ("/perps/state".to_string(), json!({ "state": {} })),
                    (
                        format!("/perps/pair-state?pair_id={}", pair_id()),
                        json!({ "pair_state": { "pair_id": pair_id() } }),
                    ),
                    (
                        "/perps/pair-states".to_string(),
                        json!({ "pair_states": {} }),
                    ),
                    (
                        format!("/perps/user-state?user={user}&include_all=true"),
                        json!({ "user_state_extended": { "user": user, "include_all": true } }),
                    ),
                    (
                        format!("/perps/order/by-user?user={user}"),
                        json!({ "orders_by_user": { "user": user } }),
                    ),
                    (
                        format!("/perps/order/by-client-order-id?user={user}&client_order_id=42"),
                        json!({
                            "order_by_client_order_id": {
                                "user": user,
                                "client_order_id": "42",
                            },
                        }),
                    ),
                ] {
                    let alias_response = call_api::<serde_json::Value>(
                        build_app_service(httpd_context.clone()),
                        &alias,
                    )
                    .await?;

                    let query_response = call_api_post::<serde_json::Value, _>(
                        build_app_service(httpd_context.clone()),
                        "/query",
                        &json!({ "wasm_smart": { "contract": perps_contract, "msg": msg } }),
                    )
                    .await?;

                    assert_eq!(
                        alias_response, query_response["wasm_smart"],
                        "alias {alias} should mirror the raw query",
                    );
                }

                // The two resting bids show up in the aggregated depth at the
                // pair's first configured bucket size.
                let pair_param = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/perps/pair-param?pair_id={}", pair_id()),
                )
                .await?;
                let bucket_size = pair_param["bucket_sizes"][0]
                    .as_str()
                    .expect("pair should have at least one bucket size")
                    .to_string();

                let depth = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!(
                        "/perps/liquidity-depth?pair_id={}&bucket_size={bucket_size}",
                        pair_id(),
                    ),
                )
                .await?;
                assert!(
                    !depth["bids"].as_object().unwrap().is_empty(),
                    "the resting bids should aggregate into at least one bucket",
                );

                // `user_state` computes the opt-in fields when asked.
                let user_state = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/perps/user-state?user={user}&include_all=true"),
                )
                .await?;
                assert!(
                    !user_state["equity"].is_null(),
                    "include_all should compute the equity",
                );

                // The full order map, from which the two orders' system-
                // assigned IDs are learned.
                let orders = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/perps/order/by-user?user={user}"),
                )
                .await?;
                let order_ids = orders
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>();
                assert_that!(order_ids).has_length(2);

                let oldest = order_ids
                    .iter()
                    .min_by_key(|id| id.parse::<u64>().unwrap())
                    .unwrap()
                    .clone();
                let newest = order_ids
                    .iter()
                    .max_by_key(|id| id.parse::<u64>().unwrap())
                    .unwrap()
                    .clone();

                // The path-parameter lookup returns the order in the
                // `QueryOrderResponse` shape: the `by-user` item plus the
                // order's user.
                let order = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/perps/order/{oldest}"),
                )
                .await?;
                let mut expected = orders[&oldest].as_object().unwrap().clone();
                expected.insert("user".to_string(), json!(user));
                assert_eq!(order, serde_json::Value::Object(expected));

                // An order ID not on the book is a 404 (a non-JSON body,
                // surfacing here as an error).
                let missing = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    "/perps/order/999999",
                )
                .await;
                assert_that!(missing).is_err();

                // The client-order-ID lookup returns the contract's
                // `QueryOrderByClientOrderIdResponse`: the `by-user` item
                // fields, plus the system-assigned `order_id`, minus the
                // `client_order_id` the caller already knows.
                let by_cid = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/perps/order/by-client-order-id?user={user}&client_order_id=42"),
                )
                .await?;
                let mut expected = orders[&newest].as_object().unwrap().clone();
                expected.remove("client_order_id");
                expected.insert("order_id".to_string(), json!(newest));
                assert_eq!(by_cid, serde_json::Value::Object(expected));

                // No resting order carries this client order ID: a 404.
                let unknown_cid = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/perps/order/by-client-order-id?user={user}&client_order_id=43"),
                )
                .await;
                assert_that!(unknown_cid).is_err();

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// The `/account/*` aliases: account parameters, the owning user (composed
/// from two factory queries), seen nonces from the account contract itself,
/// and chain-level balances — each mirroring the equivalent raw query
/// through `POST /query`.
#[tokio::test(flavor = "multi_thread")]
async fn account_aliases_mirror_contract_queries() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, httpd_context, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default().with_mocked_clickhouse()).await;

    // Register a subaccount for user1, so the owning user has two accounts.
    suite
        .execute(
            &mut accounts.user1,
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterAccount {},
            Coins::new(),
        )
        .await
        .should_succeed();

    let master = accounts.user1.address();
    let factory = contracts.account_factory;

    let local_set = tokio::task::LocalSet::new();

    local_set
        .run_until(async move {
            tokio::task::spawn_local(async move {
                // `/account/{address}`: the factory's `Account` object,
                // verbatim.
                let account = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/account/{master}"),
                )
                .await?;

                let raw = call_api_post::<serde_json::Value, _>(
                    build_app_service(httpd_context.clone()),
                    "/query",
                    &json!({ "wasm_smart": {
                        "contract": factory,
                        "msg": { "account": { "address": master } },
                    } }),
                )
                .await?;
                assert_eq!(account, raw["wasm_smart"]);

                // `/account/{address}/user`: equals the raw `user` query by
                // the owner index taken from the `account` response...
                let user = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/account/{master}/user"),
                )
                .await?;

                let raw = call_api_post::<serde_json::Value, _>(
                    build_app_service(httpd_context.clone()),
                    "/query",
                    &json!({ "wasm_smart": {
                        "contract": factory,
                        "msg": { "user": { "index": account["owner"] } },
                    } }),
                )
                .await?;
                assert_eq!(user, raw["wasm_smart"]);

                // ...contains both accounts, the master being the lowest
                // account index...
                let accounts_map = user["accounts"].as_object().unwrap();
                assert_eq!(accounts_map.len(), 2);

                let master_index = accounts_map
                    .keys()
                    .map(|index| index.parse::<u64>().unwrap())
                    .min()
                    .unwrap();
                assert_eq!(
                    accounts_map[master_index.to_string().as_str()],
                    json!(master),
                    "the lowest account index should be the master account",
                );

                // ...and is the same whether looked up via the master or the
                // subaccount address.
                let subaccount = accounts_map
                    .values()
                    .find(|address| **address != json!(master))
                    .expect("the user should have a subaccount")
                    .as_str()
                    .unwrap()
                    .to_string();

                let via_subaccount = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/account/{subaccount}/user"),
                )
                .await?;
                assert_eq!(via_subaccount, user);

                // `/account/{address}/seen-nonces`: non-empty — user1 has
                // sent a transaction — and equal to the raw query against
                // the account contract itself.
                let nonces = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/account/{master}/seen-nonces"),
                )
                .await?;
                assert!(
                    !nonces.as_array().unwrap().is_empty(),
                    "the master account has sent a transaction, so it should \
                     have recorded a nonce",
                );

                let raw = call_api_post::<serde_json::Value, _>(
                    build_app_service(httpd_context.clone()),
                    "/query",
                    &json!({ "wasm_smart": {
                        "contract": master,
                        "msg": { "seen_nonces": {} },
                    } }),
                )
                .await?;
                assert_eq!(nonces, raw["wasm_smart"]);

                // `/account/{address}/session-seen-nonces`: no session key
                // has signed anything, so the alias and the raw query both
                // return the empty set. The key travels in its base64 wire
                // encoding, URL-encoded.
                let session_key = ByteArray::<33>::from([7; 33]);
                let encoded = session_key
                    .to_string()
                    .replace('+', "%2B")
                    .replace('/', "%2F")
                    .replace('=', "%3D");

                let session_nonces = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/account/{master}/session-seen-nonces?session_key={encoded}"),
                )
                .await?;
                assert_eq!(session_nonces, json!([]));

                let raw = call_api_post::<serde_json::Value, _>(
                    build_app_service(httpd_context.clone()),
                    "/query",
                    &json!({ "wasm_smart": {
                        "contract": master,
                        "msg": { "session_seen_nonces": { "session_key": session_key } },
                    } }),
                )
                .await?;
                assert_eq!(session_nonces, raw["wasm_smart"]);

                // `/account/{address}/balances`: the chain-level query — note
                // the different response envelope.
                let balances = call_api::<serde_json::Value>(
                    build_app_service(httpd_context.clone()),
                    &format!("/account/{master}/balances"),
                )
                .await?;
                assert!(
                    !balances.as_object().unwrap().is_empty(),
                    "user1 should hold genesis balances",
                );

                let raw = call_api_post::<serde_json::Value, _>(
                    build_app_service(httpd_context.clone()),
                    "/query",
                    &json!({ "balances": { "address": master } }),
                )
                .await?;
                assert_eq!(balances, raw["balances"]);

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

#[tokio::test(flavor = "multi_thread")]
async fn requester_ip_returns_forwarded_client_ip() -> anyhow::Result<()> {
    let (_, _, _, _, _, httpd_context, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default().with_mocked_clickhouse()).await;

    let local_set = tokio::task::LocalSet::new();

    local_set
        .run_until(async {
            tokio::task::spawn_local(async {
                let app = build_app_service(httpd_context);
                let response = call_api_with_headers::<RequesterIpResponse>(
                    app,
                    "/requester-ip",
                    &[("X-Forwarded-For", "198.51.100.10, 127.0.0.1")],
                )
                .await?;

                assert_that!(response.remote_ip).is_equal_to(Some("198.51.100.10".to_string()));
                assert_that!(response.x_forwarded_for)
                    .is_equal_to(Some("198.51.100.10, 127.0.0.1".to_string()));
                assert_that!(response.peer_ip).is_some();
                assert_that!(response.forwarded).is_none();
                assert_that!(response.cf_connecting_ip).is_none();
                assert_that!(response.true_client_ip).is_none();
                assert_that!(response.x_real_ip).is_none();

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}
