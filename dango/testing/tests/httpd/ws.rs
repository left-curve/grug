//! End-to-end tests for the multiplexed WebSocket subscription endpoint
//! (`GET /ws`), which serves the validator's in-memory block / perps-event
//! feeds over a custom, `method`/`channel`-tagged JSON protocol.
//!
//! These run against a real TCP server (`actix_test::start`) and drive it with
//! a real WebSocket client (`TestServer::ws_at`), so the upgrade, the framing,
//! and the per-connection task are all exercised end to end.

use {
    actix_http::ws,
    anyhow::anyhow,
    dango_app::Indexer,
    dango_order_book::{OrderKind, Quantity, TimeInForce, UsdPrice},
    dango_primitives::{
        Addressable, Block, BlockInfo, Coins, FullBlock, QuerierExt, QueryResponse, ResultExt,
        btree_map, btree_set, coins,
    },
    dango_testing::{
        TestOption, build_app_service, create_perps_fill, pair_id, setup_perps_env,
        setup_test_naive_with_indexer,
    },
    dango_types::{constants::usdc, perps},
    futures_util::{SinkExt, Stream, StreamExt},
    serde_json::{Value, json},
    std::{collections::HashMap, sync::Arc, time::Duration},
    tokio::sync::{Mutex, mpsc},
};

/// Per-read idle budget. The in-memory snapshot arrives promptly and the live
/// tail is then silent, so a read that idles this long has drained the snapshot;
/// it is well below the 20s server keepalive ping, so no ping interferes.
const IDLE: Duration = Duration::from_secs(3);

/// Send one client message (a JSON object) as a text frame.
async fn send<S>(framed: &mut S, message: Value) -> anyhow::Result<()>
where
    S: SinkExt<ws::Message, Error = ws::ProtocolError> + Unpin,
{
    framed
        .send(ws::Message::Text(message.to_string().into()))
        .await
        .map_err(|err| anyhow!("ws send failed: {err:?}"))
}

/// Collect server messages until the stream idles past [`IDLE`], closes, or
/// `max` are gathered. Used to drain a subscription snapshot; never hangs,
/// because the live tail is silent.
async fn drain<S>(framed: &mut S, max: usize) -> anyhow::Result<Vec<Value>>
where
    S: Stream<Item = Result<ws::Frame, ws::ProtocolError>> + Unpin,
{
    let mut out = Vec::new();
    while out.len() < max {
        match tokio::time::timeout(IDLE, framed.next()).await {
            Err(_) | Ok(None) | Ok(Some(Ok(ws::Frame::Close(_)))) => break,
            Ok(Some(Ok(ws::Frame::Text(bytes)))) => out.push(serde_json::from_slice(&bytes)?),
            Ok(Some(Ok(_))) => {}, // ping/pong/binary/continuation
            Ok(Some(Err(err))) => return Err(anyhow!("ws frame error: {err:?}")),
        }
    }
    Ok(out)
}

/// Read until a server message satisfies `pred`, returning it, or error on idle.
async fn recv_until<S, F>(framed: &mut S, mut pred: F) -> anyhow::Result<Value>
where
    S: Stream<Item = Result<ws::Frame, ws::ProtocolError>> + Unpin,
    F: FnMut(&Value) -> bool,
{
    loop {
        match tokio::time::timeout(IDLE, framed.next()).await {
            Err(_) => return Err(anyhow!("timed out waiting for a matching frame")),
            Ok(None) | Ok(Some(Ok(ws::Frame::Close(_)))) => {
                return Err(anyhow!("socket closed before a matching frame"));
            },
            Ok(Some(Ok(ws::Frame::Text(bytes)))) => {
                let value: Value = serde_json::from_slice(&bytes)?;
                if pred(&value) {
                    return Ok(value);
                }
            },
            Ok(Some(Ok(_))) => {},
            Ok(Some(Err(err))) => return Err(anyhow!("ws frame error: {err:?}")),
        }
    }
}

fn channel(message: &Value) -> Option<&str> {
    message.get("channel").and_then(Value::as_str)
}

/// Subscribing to `blockInfo` and `block` on one socket replays the retained
/// window on each channel independently: both are acked, each frame parses into
/// exactly its primitive — a bare `BlockInfo` (no transactions), a `Block` (no
/// execution outcome); both types reject unknown fields, so parsing proves
/// nothing extra leaked in — and the two channels report the same ascending
/// heights, since they project the same ring.
#[tokio::test(flavor = "multi_thread")]
async fn ws_block_info_and_block_streams_replay_blocks() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, ctx, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    let pair = pair_id();
    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 100_000).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair, 2_000, 3).await;
    suite.app.indexer.wait_for_finish().await?;

    let since = ctx.stream_context.blocks().floor().unwrap_or_default();

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            tokio::task::spawn_local(async move {
                let mut srv = actix_test::start(move || build_app_service(ctx.clone()));
                let mut framed = srv
                    .ws_at("/ws")
                    .await
                    .map_err(|err| anyhow!("ws upgrade failed: {err}"))?;

                send(
                    &mut framed,
                    json!({"method": "subscribe", "id": 8, "subscription": {"type": "blockInfo", "since": since}}),
                )
                .await?;
                send(
                    &mut framed,
                    json!({"method": "subscribe", "id": 9, "subscription": {"type": "block", "since": since}}),
                )
                .await?;

                let messages = drain(&mut framed, 256).await?;

                // Both subscriptions are acknowledged.
                for id in [8, 9] {
                    messages
                        .iter()
                        .find(|m| {
                            channel(m) == Some("subscriptionResponse")
                                && m["id"].as_u64() == Some(id)
                        })
                        .ok_or_else(|| anyhow!("no ack for subscription {id}; got {messages:?}"))?;
                }

                // `blockInfo` frames are tagged id = 8 and parse into a bare
                // `BlockInfo`, with ascending heights.
                let mut info_heights = Vec::new();
                for frame in messages.iter().filter(|m| channel(m) == Some("blockInfo")) {
                    assert_eq!(frame["id"].as_u64(), Some(8));
                    let info: BlockInfo = serde_json::from_value(frame["data"].clone())
                        .map_err(|err| anyhow!("frame data is not a BlockInfo: {err}"))?;
                    info_heights.push(info.height);
                }
                assert!(
                    info_heights.len() >= 2,
                    "expected at least two blockInfo frames; got {messages:?}"
                );
                assert!(
                    info_heights.windows(2).all(|w| w[1] > w[0]),
                    "blockInfo heights must ascend: {info_heights:?}"
                );

                // `block` frames are tagged id = 9 and parse into a `Block`.
                let mut block_heights = Vec::new();
                for frame in messages.iter().filter(|m| channel(m) == Some("block")) {
                    assert_eq!(frame["id"].as_u64(), Some(9));
                    let block: Block = serde_json::from_value(frame["data"].clone())
                        .map_err(|err| anyhow!("frame data is not a Block: {err}"))?;
                    block_heights.push(block.info.height);
                }

                // Both channels project the same ring, so they replay the same
                // height sequence.
                assert_eq!(info_heights, block_heights);

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// Subscribing to `fullBlock` with a `since` replays the retained window then
/// holds the live tail: an ack arrives first, then ascending `fullBlock` frames,
/// each tagged with the subscription `id` and parsing back into a `FullBlock`.
#[tokio::test(flavor = "multi_thread")]
async fn ws_full_block_stream_replays_blocks() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, ctx, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    let pair = pair_id();
    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 100_000).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair, 2_000, 3).await;
    suite.app.indexer.wait_for_finish().await?;

    let since = ctx.stream_context.blocks().floor().unwrap_or_default();

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            tokio::task::spawn_local(async move {
                let mut srv = actix_test::start(move || build_app_service(ctx.clone()));
                let mut framed = srv
                    .ws_at("/ws")
                    .await
                    .map_err(|err| anyhow!("ws upgrade failed: {err}"))?;

                send(
                    &mut framed,
                    json!({"method": "subscribe", "id": 7, "subscription": {"type": "fullBlock", "since": since}}),
                )
                .await?;

                let messages = drain(&mut framed, 256).await?;

                // First, the subscription is acknowledged.
                let ack = messages
                    .iter()
                    .find(|m| channel(m) == Some("subscriptionResponse"))
                    .ok_or_else(|| anyhow!("no subscriptionResponse ack; got {messages:?}"))?;
                assert_eq!(ack["id"].as_u64(), Some(7));

                // Then ascending `fullBlock` data frames, each tagged id = 7 and
                // parsing back into a `FullBlock`.
                let blocks: Vec<&Value> = messages
                    .iter()
                    .filter(|m| channel(m) == Some("fullBlock"))
                    .collect();
                assert!(blocks.len() >= 2, "expected at least two block frames; got {messages:?}");

                let mut heights = Vec::new();
                for frame in &blocks {
                    assert_eq!(frame["id"].as_u64(), Some(7));
                    let block: FullBlock = serde_json::from_value(frame["data"].clone())
                        .map_err(|err| anyhow!("frame data is not a FullBlock: {err}"))?;
                    heights.push(block.block.info.height);
                }
                assert!(
                    heights.windows(2).all(|w| w[1] > w[0]),
                    "block heights must ascend: {heights:?}"
                );

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// A `pairIds`-filtered `perpsEvents` subscription replays only blocks carrying
/// a matching event; every delivered event is for that pair, the frames are
/// tagged with the subscription `id`, and the payload carries the camelCase wire
/// shape (`blockHeight` / `eventType` / `pairId` / ...).
#[tokio::test(flavor = "multi_thread")]
async fn ws_perps_events_stream_filtered_by_pair() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, ctx, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    let pair = pair_id();
    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 100_000).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair, 2_000, 3).await;
    suite.app.indexer.wait_for_finish().await?;

    let since = ctx.stream_context.perps().floor().unwrap_or_default();
    let pair_str = pair.to_string();

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            let pair_str = pair_str.clone();
            tokio::task::spawn_local(async move {
                let mut srv = actix_test::start(move || build_app_service(ctx.clone()));
                let mut framed = srv
                    .ws_at("/ws")
                    .await
                    .map_err(|err| anyhow!("ws upgrade failed: {err}"))?;

                send(
                    &mut framed,
                    json!({"method": "subscribe", "id": 1, "subscription": {"type": "perpsEvents", "since": since, "pairIds": [pair_str]}}),
                )
                .await?;

                let messages = drain(&mut framed, 256).await?;

                let batches: Vec<&Value> = messages
                    .iter()
                    .filter(|m| channel(m) == Some("perpsEvents"))
                    .collect();
                assert!(!batches.is_empty(), "expected at least one perps batch; got {messages:?}");

                let mut saw_order_filled = false;
                for frame in &batches {
                    assert_eq!(frame["id"].as_u64(), Some(1));
                    let data = &frame["data"];
                    // Wire shape: camelCase batch fields are present.
                    assert!(data.get("blockHeight").is_some(), "missing blockHeight: {data}");
                    assert!(data.get("createdAt").is_some(), "missing createdAt: {data}");
                    for event in data["events"].as_array().unwrap_or(&vec![]) {
                        assert_eq!(
                            event["pairId"].as_str(),
                            Some(pair_str.as_str()),
                            "pair filter leaked a non-matching event: {event}"
                        );
                        if event["eventType"].as_str() == Some("order_filled") {
                            saw_order_filled = true;
                        }
                    }
                }
                assert!(saw_order_filled, "the pair-filtered stream should replay order_filled");

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// Omitting the filters (the WS analogue of GraphQL's absent arguments) matches
/// every event rather than suppressing them.
#[tokio::test(flavor = "multi_thread")]
async fn ws_perps_events_stream_absent_filter_matches_all() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, ctx, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    let pair = pair_id();
    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 100_000).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair, 2_000, 3).await;
    suite.app.indexer.wait_for_finish().await?;

    let since = ctx.stream_context.perps().floor().unwrap_or_default();

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            tokio::task::spawn_local(async move {
                let mut srv = actix_test::start(move || build_app_service(ctx.clone()));
                let mut framed = srv
                    .ws_at("/ws")
                    .await
                    .map_err(|err| anyhow!("ws upgrade failed: {err}"))?;

                send(
                    &mut framed,
                    json!({"method": "subscribe", "id": 1, "subscription": {"type": "perpsEvents", "since": since}}),
                )
                .await?;

                let messages = drain(&mut framed, 256).await?;
                let total: usize = messages
                    .iter()
                    .filter(|m| channel(m) == Some("perpsEvents"))
                    .filter_map(|m| m["data"]["events"].as_array())
                    .map(Vec::len)
                    .sum();

                assert!(total > 0, "absent filters must match all events, not suppress them");

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// A `since` older than the retained in-memory window does not open a stream; it
/// replies with an `error`-keyed frame tagged `resync` (carrying the offending
/// `id`) on the subscription's own `perpsEvents` channel — the WS analogue of
/// the SSE `409 Conflict`.
#[tokio::test(flavor = "multi_thread")]
async fn ws_perps_events_stream_resync_required_is_error_frame() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, ctx, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    let pair = pair_id();
    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 100_000).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair, 2_000, 3).await;
    suite.app.indexer.wait_for_finish().await?;

    let floor = ctx
        .stream_context
        .perps()
        .floor()
        .expect("perps ring is non-empty after a fill");
    assert!(floor >= 1, "unexpected ring floor: {floor}");
    let stale = floor - 1;

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            tokio::task::spawn_local(async move {
                let mut srv = actix_test::start(move || build_app_service(ctx.clone()));
                let mut framed = srv
                    .ws_at("/ws")
                    .await
                    .map_err(|err| anyhow!("ws upgrade failed: {err}"))?;

                send(
                    &mut framed,
                    json!({"method": "subscribe", "id": 3, "subscription": {"type": "perpsEvents", "since": stale}}),
                )
                .await?;

                let error = recv_until(&mut framed, |m| {
                    channel(m) == Some("perpsEvents") && m.get("error").is_some()
                })
                .await?;
                assert_eq!(error["id"].as_u64(), Some(3));
                assert_eq!(error["error"]["code"].as_str(), Some("resync"));

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// `ping` is answered with a `pong` carrying the same `id`, and `unsubscribe`
/// acknowledges the matching subscription — exercising the control protocol on a
/// single socket.
#[tokio::test(flavor = "multi_thread")]
async fn ws_ping_pong_and_unsubscribe() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, ctx, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    let pair = pair_id();
    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 100_000).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair, 2_000, 3).await;
    suite.app.indexer.wait_for_finish().await?;

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            tokio::task::spawn_local(async move {
                let mut srv = actix_test::start(move || build_app_service(ctx.clone()));
                let mut framed = srv
                    .ws_at("/ws")
                    .await
                    .map_err(|err| anyhow!("ws upgrade failed: {err}"))?;

                // ping -> pong, id echoed.
                send(&mut framed, json!({"method": "ping", "id": 9})).await?;
                let pong = recv_until(&mut framed, |m| channel(m) == Some("pong")).await?;
                assert_eq!(pong["id"].as_u64(), Some(9));

                // subscribe -> ack, then unsubscribe -> ack.
                send(
                    &mut framed,
                    json!({"method": "subscribe", "id": 2, "subscription": {"type": "perpsEvents"}}),
                )
                .await?;
                let sub_ack = recv_until(&mut framed, |m| {
                    channel(m) == Some("subscriptionResponse") && m["id"].as_u64() == Some(2)
                })
                .await?;
                assert_eq!(sub_ack["data"]["method"].as_str(), Some("subscribe"));

                send(&mut framed, json!({"method": "unsubscribe", "id": 2})).await?;
                let unsub_ack = recv_until(&mut framed, |m| {
                    channel(m) == Some("subscriptionResponse")
                        && m["data"]["method"].as_str() == Some("unsubscribe")
                })
                .await?;
                assert_eq!(unsub_ack["id"].as_u64(), Some(2));

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// A `query` runs a one-time, read-only state query over the socket: the reply
/// is a single `data` frame on the `query` channel carrying the raw
/// `QueryResponse` — the same shape as REST `POST /query`. A failing query is
/// answered with a co-located `queryFailed` error frame that ends nothing: the
/// socket stays usable and the one-shot `id` becomes free to reuse.
#[tokio::test(flavor = "multi_thread")]
async fn ws_query_roundtrip_and_error() -> anyhow::Result<()> {
    let (suite, _accounts, _, _contracts, _, ctx, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;
    suite.app.indexer.wait_for_finish().await?;

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            tokio::task::spawn_local(async move {
                let mut srv = actix_test::start(move || build_app_service(ctx.clone()));
                let mut framed = srv
                    .ws_at("/ws")
                    .await
                    .map_err(|err| anyhow!("ws upgrade failed: {err}"))?;

                // A well-formed query is answered with the raw `QueryResponse`,
                // tagged with the request's id.
                send(
                    &mut framed,
                    json!({"method": "query", "id": 21, "query": {"config": {}}}),
                )
                .await?;
                let reply = recv_until(&mut framed, |m| {
                    channel(m) == Some("query") && m["id"].as_u64() == Some(21)
                })
                .await?;
                let response: QueryResponse = serde_json::from_value(reply["data"].clone())
                    .map_err(|err| anyhow!("frame data is not a QueryResponse: {err}"))?;
                assert!(
                    matches!(response, QueryResponse::Config(_)),
                    "expected a config response: {reply:?}"
                );

                // A failing query — no contract lives at the zero address — is
                // a co-located `queryFailed` error frame.
                send(
                    &mut framed,
                    json!({"method": "query", "id": 22, "query": {"contract": {"address": "0x0000000000000000000000000000000000000000"}}}),
                )
                .await?;
                let error = recv_until(&mut framed, |m| {
                    channel(m) == Some("query") && m.get("error").is_some()
                })
                .await?;
                assert_eq!(error["id"].as_u64(), Some(22));
                assert_eq!(error["error"]["code"].as_str(), Some("queryFailed"));
                assert!(
                    !error["error"]["message"]
                        .as_str()
                        .unwrap_or_default()
                        .is_empty(),
                    "queryFailed must carry the query error: {error:?}"
                );

                // The failure ended nothing: the socket still answers, and the
                // one-shot id is free to reuse.
                send(&mut framed, json!({"method": "ping", "id": 23})).await?;
                let pong = recv_until(&mut framed, |m| channel(m) == Some("pong")).await?;
                assert_eq!(pong["id"].as_u64(), Some(23));

                send(
                    &mut framed,
                    json!({"method": "query", "id": 22, "query": {"config": {}}}),
                )
                .await?;
                let reply = recv_until(&mut framed, |m| {
                    channel(m) == Some("query") && m.get("data").is_some()
                })
                .await?;
                assert_eq!(reply["id"].as_u64(), Some(22));

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// A `query` subscription streams `{blockHeight, response}` frames: an initial
/// snapshot at subscribe time, then one frame per matching block. Two
/// subscriptions holding the same query see identical payloads at equal
/// heights (they are served from the shared per-block execution), each with
/// strictly ascending heights.
#[tokio::test(flavor = "multi_thread")]
async fn ws_query_subscription_streams_on_new_blocks() -> anyhow::Result<()> {
    let (suite, mut accounts, _, _contracts, _, ctx, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;
    suite.app.indexer.wait_for_finish().await?;

    // A producer task minting one transfer block per trigger, so ticks arrive
    // while the subscriptions are live.
    let suite = Arc::new(Mutex::new(suite));
    let (make_block, mut block_requests) = mpsc::channel::<u32>(1);
    tokio::spawn(async move {
        while block_requests.recv().await.is_some() {
            let mut suite = suite.lock().await;
            suite
                .transfer(
                    &mut accounts.user1,
                    accounts.user2.address(),
                    coins! { usdc::DENOM.clone() => 100 },
                )
                .await
                .should_succeed();
        }
    });

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            tokio::task::spawn_local(async move {
                let mut srv = actix_test::start(move || build_app_service(ctx.clone()));
                let mut framed = srv
                    .ws_at("/ws")
                    .await
                    .map_err(|err| anyhow!("ws upgrade failed: {err}"))?;

                // Two subscriptions holding the same query.
                for id in [7, 8] {
                    send(
                        &mut framed,
                        json!({"method": "subscribe", "id": id, "subscription": {"type": "query", "query": {"config": {}}, "interval": 1}}),
                    )
                    .await?;
                }

                // Both receive an initial snapshot whose response is a `Config`.
                let mut last_heights: HashMap<u64, u64> = HashMap::new();
                let mut payloads: HashMap<(u64, u64), Value> = HashMap::new();
                for _ in 0..2 {
                    let frame = recv_until(&mut framed, |m| {
                        channel(m) == Some("query") && m.get("data").is_some()
                    })
                    .await?;
                    let id = frame["id"].as_u64().unwrap();
                    let height = frame["data"]["blockHeight"]
                        .as_u64()
                        .ok_or_else(|| anyhow!("missing blockHeight: {frame:?}"))?;
                    let response: QueryResponse =
                        serde_json::from_value(frame["data"]["response"].clone())?;
                    assert!(
                        matches!(response, QueryResponse::Config(_)),
                        "expected a config response: {frame:?}"
                    );
                    payloads.insert((id, height), frame["data"].clone());
                    last_heights.insert(id, height);
                }
                assert_eq!(last_heights.len(), 2, "both subscriptions must snapshot");
                let initial_heights = last_heights.clone();

                // Mint blocks until both subscriptions have ticked past their
                // snapshot, asserting strict per-subscription height ascent.
                make_block.send(1).await?;
                make_block.send(1).await?;
                while last_heights
                    .iter()
                    .any(|(id, height)| height <= &initial_heights[id])
                {
                    let frame = recv_until(&mut framed, |m| {
                        channel(m) == Some("query") && m.get("data").is_some()
                    })
                    .await?;
                    let id = frame["id"].as_u64().unwrap();
                    let height = frame["data"]["blockHeight"].as_u64().unwrap();
                    assert!(
                        height > last_heights[&id],
                        "heights must strictly ascend per subscription: {frame:?}"
                    );
                    payloads.insert((id, height), frame["data"].clone());
                    last_heights.insert(id, height);
                }

                // Wherever the two subscriptions saw the same height — the
                // initial snapshot guarantees at least one — the payloads are
                // identical.
                let mut shared = 0;
                for ((id, height), data) in &payloads {
                    let sibling = if *id == 7 { 8 } else { 7 };
                    if let Some(other) = payloads.get(&(sibling, *height)) {
                        assert_eq!(data, other, "same query at same height must match");
                        shared += 1;
                    }
                }
                assert!(shared > 0, "expected a height common to both subscriptions");

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// Query-subscription error handling: a zero `interval` is rejected with a
/// co-located `badRequest`; a subscription whose query fails is acked (the
/// stream opens), then ended by a single terminal `queryFailed` frame — with
/// the socket staying usable throughout.
#[tokio::test(flavor = "multi_thread")]
async fn ws_query_subscription_bad_interval_and_failing_query() -> anyhow::Result<()> {
    let (suite, _accounts, _, _contracts, _, ctx, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;
    suite.app.indexer.wait_for_finish().await?;

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            tokio::task::spawn_local(async move {
                let mut srv = actix_test::start(move || build_app_service(ctx.clone()));
                let mut framed = srv
                    .ws_at("/ws")
                    .await
                    .map_err(|err| anyhow!("ws upgrade failed: {err}"))?;

                // A zero interval is rejected up front, before any ack.
                send(
                    &mut framed,
                    json!({"method": "subscribe", "id": 4, "subscription": {"type": "query", "query": {"config": {}}, "interval": 0}}),
                )
                .await?;
                let error = recv_until(&mut framed, |m| {
                    channel(m) == Some("query") && m.get("error").is_some()
                })
                .await?;
                assert_eq!(error["id"].as_u64(), Some(4));
                assert_eq!(error["error"]["code"].as_str(), Some("badRequest"));

                // A failing query — no contract lives at the zero address — is
                // acked, then ended by a terminal `queryFailed` (the initial
                // snapshot is the failing execution).
                send(
                    &mut framed,
                    json!({"method": "subscribe", "id": 5, "subscription": {"type": "query", "query": {"contract": {"address": "0x0000000000000000000000000000000000000000"}}}}),
                )
                .await?;
                let ack = recv_until(&mut framed, |m| {
                    channel(m) == Some("subscriptionResponse") && m["id"].as_u64() == Some(5)
                })
                .await?;
                assert_eq!(ack["data"]["type"].as_str(), Some("query"));

                let error = recv_until(&mut framed, |m| {
                    channel(m) == Some("query") && m.get("error").is_some()
                })
                .await?;
                assert_eq!(error["id"].as_u64(), Some(5));
                assert_eq!(error["error"]["code"].as_str(), Some("queryFailed"));

                // The failure ended only that subscription; the socket lives.
                send(&mut framed, json!({"method": "ping", "id": 6})).await?;
                let pong = recv_until(&mut framed, |m| channel(m) == Some("pong")).await?;
                assert_eq!(pong["id"].as_u64(), Some(6));

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}

/// The perps alias subscriptions — `perpsPairState`, `perpsUserState`,
/// `perpsOrdersByUser`, `perpsLiquidityDepth` — desugar into standing
/// queries: each is acked and snapshots on its own channel with the contract
/// response unwrapped (verbatim what the REST twin returns); a raw `query`
/// twin of the same read sees the identical payload inside the `wasm_smart`
/// envelope at equal heights (they share the memoized execution); minting
/// blocks ticks strictly-higher frames; a zero `interval` is rejected with a
/// co-located `badRequest`; and an unknown pair streams the verbatim `null`.
#[tokio::test(flavor = "multi_thread")]
async fn ws_perps_alias_subscriptions_stream_unwrapped_responses() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, ctx, _, _, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    let pair = pair_id();
    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 100_000).await;

    // Configure a liquidity-depth bucket size (the test genesis has none) —
    // before placing orders, since depth tracks orders placed afterwards.
    let param: perps::Param = suite
        .query_wasm_smart(contracts.perps, perps::QueryParamRequest {})
        .should_succeed();
    let pair_param: Option<perps::PairParam> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryPairParamRequest {
                pair_id: pair.clone(),
            },
        )
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param,
                pair_params: btree_map! {
                    pair.clone() => perps::PairParam {
                        bucket_sizes: btree_set! { UsdPrice::new_int(100) },
                        ..pair_param.expect("the test genesis should have the pair")
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // A resting bid, so the depth, open-order, and user-state snapshots have
    // content.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_500),
                    time_in_force: TimeInForce::GoodTilCanceled,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite.app.indexer.wait_for_finish().await?;

    let user = accounts.user1.address();
    let perps_contract = contracts.perps;
    let pair_str = pair.to_string();

    // A producer task minting fill blocks per trigger, so ticks arrive while
    // the subscriptions are live.
    let suite = Arc::new(Mutex::new(suite));
    let (make_block, mut block_requests) = mpsc::channel::<u32>(1);
    {
        let suite = suite.clone();
        let pair = pair.clone();
        tokio::spawn(async move {
            while block_requests.recv().await.is_some() {
                let mut suite = suite.lock().await;
                create_perps_fill(&mut suite, &mut accounts, &contracts, &pair, 2_000, 1).await;
            }
        });
    }

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            tokio::task::spawn_local(async move {
                let mut srv = actix_test::start(move || build_app_service(ctx.clone()));
                let mut framed = srv
                    .ws_at("/ws")
                    .await
                    .map_err(|err| anyhow!("ws upgrade failed: {err}"))?;

                // The four aliases, plus a raw `query` twin of the pair-state
                // read (id 5).
                let subscriptions = [
                    (1, json!({"type": "perpsPairState", "pair_id": pair_str, "interval": 1})),
                    (2, json!({"type": "perpsUserState", "user": user, "include_all": true, "interval": 1})),
                    (3, json!({"type": "perpsOrdersByUser", "user": user, "interval": 1})),
                    (4, json!({"type": "perpsLiquidityDepth", "pair_id": pair_str, "bucket_size": "100", "interval": 1})),
                    (5, json!({"type": "query", "query": {"wasm_smart": {"contract": perps_contract, "msg": {"pair_state": {"pair_id": pair_str}}}}, "interval": 1})),
                ];
                for (id, subscription) in &subscriptions {
                    send(
                        &mut framed,
                        json!({"method": "subscribe", "id": id, "subscription": subscription}),
                    )
                    .await?;
                }

                let channels: HashMap<u64, &str> = HashMap::from([
                    (1, "perpsPairState"),
                    (2, "perpsUserState"),
                    (3, "perpsOrdersByUser"),
                    (4, "perpsLiquidityDepth"),
                    (5, "query"),
                ]);

                // Every subscription snapshots on its own channel with the
                // right id.
                let mut last_heights: HashMap<u64, u64> = HashMap::new();
                let mut payloads: HashMap<(u64, u64), Value> = HashMap::new();
                while last_heights.len() < subscriptions.len() {
                    let frame = recv_until(&mut framed, |m| {
                        m.get("data").is_some() && channel(m) != Some("subscriptionResponse")
                    })
                    .await?;
                    let Some(id) = frame["id"].as_u64() else {
                        continue;
                    };
                    let Some(expected_channel) = channels.get(&id) else {
                        continue;
                    };

                    assert_eq!(channel(&frame), Some(*expected_channel));

                    let height = frame["data"]["blockHeight"]
                        .as_u64()
                        .ok_or_else(|| anyhow!("missing blockHeight: {frame:?}"))?;
                    payloads.insert((id, height), frame["data"].clone());
                    last_heights.insert(id, height);
                }

                // The alias snapshots carry the unwrapped contract responses.
                let snapshot = |id: u64| &payloads[&(id, last_heights[&id])]["response"];

                assert!(
                    snapshot(1).get("index_price").is_some(),
                    "pair state should be unwrapped: {:?}",
                    snapshot(1)
                );
                assert!(
                    !snapshot(2)["margin"].is_null(),
                    "user state should have margin: {:?}",
                    snapshot(2)
                );
                assert_eq!(
                    snapshot(3).as_object().map(|orders| orders.len()),
                    Some(1),
                    "one resting order expected: {:?}",
                    snapshot(3)
                );
                assert!(
                    !snapshot(4)["bids"].as_object().unwrap().is_empty(),
                    "the resting bid should aggregate into a depth bucket: {:?}",
                    snapshot(4)
                );

                // Mint fills until every subscription has ticked past its
                // snapshot, with strictly ascending per-subscription heights.
                let initial_heights = last_heights.clone();
                make_block.send(1).await?;
                make_block.send(1).await?;
                while last_heights
                    .iter()
                    .any(|(id, height)| height <= &initial_heights[id])
                {
                    let frame = recv_until(&mut framed, |m| {
                        m.get("data").is_some() && channel(m) != Some("subscriptionResponse")
                    })
                    .await?;
                    let Some(id) = frame["id"].as_u64() else {
                        continue;
                    };
                    if !channels.contains_key(&id) {
                        continue;
                    }

                    let height = frame["data"]["blockHeight"].as_u64().unwrap();
                    assert!(
                        height > last_heights[&id],
                        "heights must strictly ascend per subscription: {frame:?}"
                    );
                    payloads.insert((id, height), frame["data"].clone());
                    last_heights.insert(id, height);
                }

                // Wherever the alias (id 1) and its raw twin (id 5) saw the
                // same height — the simultaneous snapshots guarantee at least
                // one — the alias payload is exactly the raw payload with the
                // `wasm_smart` envelope removed.
                let mut shared = 0;
                for ((id, height), data) in &payloads {
                    if *id != 1 {
                        continue;
                    }
                    if let Some(raw) = payloads.get(&(5, *height)) {
                        assert_eq!(
                            data["response"], raw["response"]["wasm_smart"],
                            "alias and raw twin must serve the same execution",
                        );
                        shared += 1;
                    }
                }
                assert!(shared > 0, "expected a height common to alias and twin");

                // A zero interval is rejected on the alias's own channel.
                send(
                    &mut framed,
                    json!({"method": "subscribe", "id": 9, "subscription": {"type": "perpsLiquidityDepth", "pair_id": pair_str, "bucket_size": "100", "interval": 0}}),
                )
                .await?;
                let error = recv_until(&mut framed, |m| {
                    channel(m) == Some("perpsLiquidityDepth") && m["id"].as_u64() == Some(9)
                })
                .await?;
                assert_eq!(error["error"]["code"].as_str(), Some("badRequest"));

                // An unknown pair streams the verbatim `null` — the WS
                // analogue of the REST 404.
                send(
                    &mut framed,
                    json!({"method": "subscribe", "id": 10, "subscription": {"type": "perpsPairState", "pair_id": "perp/nonexistent", "interval": 1}}),
                )
                .await?;
                let frame = recv_until(&mut framed, |m| {
                    channel(m) == Some("perpsPairState")
                        && m["id"].as_u64() == Some(10)
                        && m.get("data").is_some()
                })
                .await?;
                assert!(
                    frame["data"]["response"].is_null(),
                    "an unknown pair should stream null: {frame:?}"
                );

                Ok::<(), anyhow::Error>(())
            })
            .await
        })
        .await?
}
