//! Multiplexed WebSocket subscription endpoint (`GET /ws`).
//!
//! One socket carries any number of subscriptions, each identified by a
//! client-chosen `id`. Client messages are `method`-tagged
//! (`subscribe` / `unsubscribe` / `ping` / `broadcast` / `query`); server
//! messages are `channel`-tagged
//! (`subscriptionResponse` / `<channel>` / `pong`). The `id` of a `subscribe` is
//! echoed on its acknowledgement and on every frame it produces, so concurrent
//! subscriptions (e.g. several `perpsEvents` feeds with different filters) are
//! demultiplexable on the client.
//!
//! Errors are co-located with the operation they concern: a problem opening or
//! running a subscription rides that subscription's own channel and `id`, as an
//! `error`-keyed frame alongside its `data`-keyed frames, so a client handles a
//! feed's failure on the same channel it reads from. Only errors with no
//! channel to attribute them to — an unparseable frame, or an `unsubscribe` for
//! an unknown `id` — use the dedicated `error` channel.
//!
//! The following channel types are served, all reusing the in-memory validator
//! stream that backed the `full_block` / `perps_events` GraphQL subscriptions:
//!
//! - `perpsEvents` — perps-contract events grouped per block, narrowed by the
//!   `eventTypes` / `pairIds` / `users` / `orderIds` / `clientOrderIds` filters.
//! - `blockInfo` — every finalized block's metadata
//!   (`{height, timestamp, hash}`).
//! - `block` — every finalized block without its execution outcome
//!   (`{info, txs}`).
//! - `fullBlock` — every finalized block in full (`{block, outcome}`).
//! - `query` — a standing query: an initial snapshot at subscribe time, then a
//!   re-run once per block whose height is a multiple of `interval`, each
//!   frame a `{blockHeight, response}`. Identical concurrent subscriptions
//!   share one execution per tick (see [`crate::query_memo`]).
//! - `perpsPairState` / `perpsUserState` / `perpsOrdersByUser` /
//!   `perpsLiquidityDepth` — standing-query aliases of the matching `/perps/*`
//!   REST routes, taking the routes' snake_case parameters in place of a raw
//!   grug `Query`. Each desugars at subscribe time — the perps contract
//!   address is resolved server-side — into the same execution path as
//!   `query`, so an alias and a raw `query` subscription for the same read
//!   share executions through the memo. Frames are `{blockHeight, response}`
//!   on the alias's own channel, with `response` unwrapped to the raw
//!   contract response, exactly what the REST twin returns.
//!
//! Two one-shot request/response methods ride the same socket: `broadcast`
//! (submit a signed transaction to the mempool) and `query` (run a read-only
//! query against the latest finalized state, the same `Query`/`QueryResponse`
//! shapes as REST `POST /query`). Each is answered with a single frame on its
//! own channel, tagged with the request's `id`.
//!
//! The transport is otherwise a thin shell over
//! [`dango_indexer_stream::Context`]; only the framing differs from the SSE and
//! GraphQL transports. The message enums are left open (no `#[non_exhaustive]`
//! barrier, but room in the protocol) so further read channels (`l2Book`,
//! `candle`, …) can be added without breaking existing clients.

use {
    crate::{
        context::FullContext,
        graphql::query::core::CoreQuery,
        query_memo::QueryFrame,
        request_ip::RequesterIp,
        routes::perps::{LiquidityDepthQuery, OrdersByUserQuery, PairIdQuery, UserStateQuery},
        subscription_limiter::{ConnectionLimiter, SubscriptionLimiter, guard_subscription_stream},
    },
    actix_web::{HttpRequest, HttpResponse, Resource, guard, web},
    actix_ws::{AggregatedMessage, Session},
    dango_indexer_stream::make_perps_filter,
    dango_primitives::{HttpRequestDetails, Json, Query, QueryResponse, Tx},
    dango_types::perps,
    futures_util::{StreamExt, stream},
    serde::{Deserialize, Serialize},
    std::{collections::HashSet, pin::Pin, sync::Arc, time::Duration},
    tokio::time::{Instant, MissedTickBehavior, interval},
    tokio_stream::StreamMap,
};

/// How often the server sends a WebSocket control-frame ping and re-checks the
/// idle deadline.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

/// Close a socket that has not produced any inbound frame — including the
/// automatic pong to our keepalive ping — for this long. A live but idle
/// subscriber stays connected because its WebSocket stack pongs our pings; a
/// dead connection is reaped within `IDLE_TIMEOUT`.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Each active subscription's stream, already projected to ready-to-send JSON
/// text frames. Boxing erases the per-channel item type so both channels share
/// one [`StreamMap`]; `Pin<Box<_>>` is `Unpin`, as `StreamMap` requires.
type FrameStream = Pin<Box<dyn stream::Stream<Item = String> + Send>>;

/// Register the `GET /ws` upgrade route. The `upgrade: websocket` header guard
/// mirrors the GraphQL route, so a plain (non-upgrade) `GET /ws` falls through
/// to the default 404 handler instead of erroring inside the upgrade.
pub fn services() -> Resource {
    web::resource("/ws").route(
        web::get()
            .guard(guard::Header("upgrade", "websocket"))
            .to(ws_index),
    )
}

/// Doc-only stub carrying the OpenAPI path item for `GET /ws` — never mounted;
/// the real route is the upgrade-guarded resource in [`services`], which
/// `#[utoipa::path]` cannot annotate. OpenAPI cannot model WebSocket traffic,
/// so the entry is purely informational.
#[utoipa::path(
    get,
    path = "/ws",
    tag = "websocket",
    summary = "Multiplexed WebSocket subscriptions",
    description = "WebSocket upgrade endpoint. One socket carries any number \
                   of subscriptions, each identified by a client-chosen `id`. \
                   Client messages are `method`-tagged JSON: `subscribe` — \
                   opening a `perpsEvents` feed (per-block perps events, \
                   narrowed by `eventTypes` / `pairIds` / `users` / `orderIds` \
                   / `clientOrderIds` filters), a `blockInfo` feed (every \
                   finalized block's metadata, the `info` field of `Block`), \
                   a `block` feed (every finalized block without its \
                   execution outcome, the same shape as \
                   `/block/info/{block_height}`), a `fullBlock` feed \
                   (every finalized `{ block, outcome }`, the same shape as \
                   `/block/full/{block_height}`), a `query` feed (a \
                   standing read-only state query, re-run every `interval` \
                   blocks), or a standing perps alias feed — \
                   `perpsPairState`, `perpsUserState`, `perpsOrdersByUser`, \
                   `perpsLiquidityDepth` — the WS twins of the matching \
                   `/perps/*` REST routes, taking the same snake_case \
                   parameters plus `interval`, with the contract address \
                   resolved server-side and each frame's `response` being \
                   the raw contract response — plus `unsubscribe`, `ping`, \
                   `broadcast` (submit a signed `Tx` over the socket), and \
                   `query` (run a one-time read-only state query, the same \
                   `Query`/`QueryResponse` shapes as `POST /query`). \
                   Server frames are `channel`-tagged: `subscriptionResponse`, \
                   `perpsEvents`, `blockInfo`, `block`, `fullBlock`, \
                   `perpsPairState`, `perpsUserState`, `perpsOrdersByUser`, \
                   `perpsLiquidityDepth`, `broadcast`, `query`, `pong`, and \
                   `error`. The server pings every 20 seconds and closes a \
                   socket idle for 60 seconds. \
                   **Swagger UI cannot open WebSocket connections** — this \
                   entry is documentation only.",
    responses(
        (status = 101, description = "Switching Protocols — WebSocket handshake accepted; \
                                      all further traffic is JSON frames"),
        (status = 404, description = "Without an `Upgrade: websocket` header the route does \
                                      not match and the request falls through to the 404 \
                                      handler"),
    ),
)]
pub fn ws_doc() {}

// ---- client and server message types ----

/// A message sent by the client. Discriminated by `method`.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum ClientMessage {
    /// Open a subscription. `id` is the client-chosen handle echoed on the
    /// acknowledgement and every resulting data frame, and used to
    /// `unsubscribe`.
    Subscribe { id: u64, subscription: Subscription },

    /// Close the subscription opened with this `id`.
    Unsubscribe { id: u64 },

    /// Application-level heartbeat; answered with a `pong` carrying the same
    /// `id`.
    Ping {
        #[serde(default)]
        id: Option<u64>,
    },

    /// Broadcast a signed transaction to the mempool. Answered on the
    /// `broadcast` channel: a `BroadcastTxOutcome` (`data`) — including a mempool
    /// rejection, which rides `check_tx.result` — or an `error` frame only on a
    /// transport failure to the consensus node. The REST `POST /broadcast` is the
    /// default; this lets a client already holding a `/ws` connection broadcast
    /// without a separate HTTP request.
    Broadcast { id: u64, tx: Tx },

    /// Run a one-time, read-only query against the latest finalized state.
    /// Answered on the `query` channel: the raw `QueryResponse` (`data`) — the
    /// same shape the REST `POST /query` returns — or an `error` frame if the
    /// query fails. The REST route is the default; this lets a client already
    /// holding a `/ws` connection query state without a separate HTTP request.
    Query { id: u64, query: Query },
}

/// The feed a `subscribe` selects, discriminated by `type`.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum Subscription {
    /// Per-block perps-contract events — order lifecycle, fills, liquidations,
    /// and deleveraging — narrowed by the [`PerpsEventsParams`] filters.
    /// Delivered on the `perpsEvents` channel.
    PerpsEvents {
        #[serde(default)]
        since: Option<u64>,

        #[serde(default)]
        event_types: Option<HashSet<String>>,

        #[serde(default)]
        pair_ids: Option<HashSet<String>>,

        #[serde(default)]
        users: Option<HashSet<String>>,

        #[serde(default)]
        order_ids: Option<HashSet<String>>,

        #[serde(default)]
        client_order_ids: Option<HashSet<String>>,
    },

    /// Every finalized block's metadata (height, timestamp, hash) — the `info`
    /// field of `Block`. Delivered on the `blockInfo` channel.
    BlockInfo {
        #[serde(default)]
        since: Option<u64>,
    },

    /// Every finalized block as produced by the consensus engine — metadata
    /// and transactions, without the execution outcome. Delivered on the
    /// `block` channel.
    Block {
        #[serde(default)]
        since: Option<u64>,
    },

    /// Every finalized block in full (`Block` + `BlockOutcome`). Delivered on
    /// the `fullBlock` channel.
    FullBlock {
        #[serde(default)]
        since: Option<u64>,
    },

    /// A standing read-only query: an initial snapshot at subscribe time, then
    /// a re-run once per block whose height is a multiple of `interval`, each
    /// frame carrying `{blockHeight, response}` on the `query` channel.
    ///
    /// Live-only — there is no `since` replay, because historical state cannot
    /// be re-queried; on reconnect, resubscribe and take the fresh snapshot.
    Query {
        query: Query,

        #[serde(default = "default_query_interval")]
        interval: u64,
    },

    /// A standing `pair_state` read for one trading pair — open interest,
    /// funding rate, index price — the WS twin of REST
    /// `GET /perps/pair-state`, with the same snake_case parameters and the
    /// same `interval` semantics as `query`. `response` is the contract's
    /// `PairState`, verbatim; an unknown pair streams `null`, the WS analogue
    /// of the REST 404.
    PerpsPairState {
        #[serde(flatten)]
        params: PairIdQuery,

        #[serde(default = "default_query_interval")]
        interval: u64,
    },

    /// A standing `user_state_extended` read for one user — margin, equity,
    /// positions, per the `include_*` flags — the WS twin of REST
    /// `GET /perps/user-state`.
    PerpsUserState {
        #[serde(flatten)]
        params: UserStateQuery,

        #[serde(default = "default_query_interval")]
        interval: u64,
    },

    /// A standing `orders_by_user` read — a user's resting limit orders keyed
    /// by order ID — the WS twin of REST `GET /perps/order/by-user`. For
    /// incremental order updates, prefer the push-based `perpsEvents` feed;
    /// this is the periodically-refreshed snapshot form.
    PerpsOrdersByUser {
        #[serde(flatten)]
        params: OrdersByUserQuery,

        #[serde(default = "default_query_interval")]
        interval: u64,
    },

    /// A standing `liquidity_depth` read — aggregated order book depth at one
    /// of the pair's configured bucket sizes — the WS twin of REST
    /// `GET /perps/liquidity-depth`. `interval: 1` gives per-block book
    /// updates.
    PerpsLiquidityDepth {
        #[serde(flatten)]
        params: LiquidityDepthQuery,

        #[serde(default = "default_query_interval")]
        interval: u64,
    },
}

/// A `query` subscription that does not say otherwise re-runs every 10 blocks
/// — matching the GraphQL `query_app` default, and keeping the load modest
/// when many clients subscribe without choosing an interval. A client that
/// wants per-block updates asks for `interval: 1` explicitly.
const fn default_query_interval() -> u64 {
    10
}

impl Subscription {
    /// The channel name data frames from this subscription carry.
    fn channel(&self) -> &'static str {
        match self {
            Subscription::PerpsEvents { .. } => "perpsEvents",
            Subscription::BlockInfo { .. } => "blockInfo",
            Subscription::Block { .. } => "block",
            Subscription::FullBlock { .. } => "fullBlock",
            Subscription::Query { .. } => "query",
            Subscription::PerpsPairState { .. } => "perpsPairState",
            Subscription::PerpsUserState { .. } => "perpsUserState",
            Subscription::PerpsOrdersByUser { .. } => "perpsOrdersByUser",
            Subscription::PerpsLiquidityDepth { .. } => "perpsLiquidityDepth",
        }
    }
}

/// A control message sent by the server. Discriminated by `channel`. Data frames
/// are serialized separately via [`DataFrame`] because their payload type varies
/// per channel.
#[derive(Debug, Serialize)]
#[serde(tag = "channel", rename_all = "camelCase")]
enum ServerControl<'a> {
    SubscriptionResponse {
        id: u64,
        data: Ack<'a>,
    },
    Pong {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
    },
    /// A connection-level error with no subscription channel to attribute it to
    /// — an unparseable frame, or an `unsubscribe` for an unknown `id`. Errors
    /// that concern a specific subscription instead ride its channel; see
    /// [`ErrorFrame`].
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        error: ErrorData<'a>,
    },
}

#[derive(Debug, Serialize)]
struct Ack<'a> {
    /// `"subscribe"` or `"unsubscribe"`.
    method: &'a str,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    channel: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct ErrorData<'a> {
    code: &'a str,
    message: &'a str,
}

/// One block's worth of data on a channel, tagged with its source subscription
/// `id`.
#[derive(Debug, Serialize)]
struct DataFrame<'a, T> {
    channel: &'static str,
    id: u64,
    data: &'a T,
}

/// An error scoped to a subscription, delivered on that subscription's own
/// channel and `id` — mirroring its data frames — so a client handles a feed's
/// failure on the same channel it reads from. Connection-level errors that have
/// no channel to attribute them to use [`ServerControl::Error`] instead.
#[derive(Debug, Serialize)]
struct ErrorFrame<'a> {
    channel: &'static str,
    id: u64,
    error: ErrorData<'a>,
}

// ---- handler ----

/// Accept the WebSocket upgrade and drive the connection on a detached task.
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
pub async fn ws_index(
    req: HttpRequest,
    body: web::Payload,
    app_ctx: web::Data<FullContext>,
    limiter: web::Data<SubscriptionLimiter>,
) -> actix_web::Result<HttpResponse> {
    let (response, session, msg_stream) = actix_ws::handle(&req, body)?;

    // Computed once per connection and reused by every `broadcast` on this
    // socket, mirroring how the REST/GraphQL broadcast paths capture it per
    // request.
    let http_details = RequesterIp::from_request(&req).into_http_request_details();
    let conn_limiter = limiter.new_connection();

    // The connection loop runs on a detached task, so it owns the context (a
    // cheap `Arc`-cloning `FullContext`) rather than borrowing `web::Data`.
    actix_web::rt::spawn(connection_loop(
        session,
        msg_stream,
        app_ctx.as_ref().clone(),
        http_details,
        conn_limiter,
    ));

    Ok(response)
}

/// The per-connection event loop: multiplex inbound control messages, active
/// subscription streams, and the keepalive timer over one socket.
async fn connection_loop(
    mut session: Session,
    msg_stream: actix_ws::MessageStream,
    app_ctx: FullContext,
    http_details: HttpRequestDetails,
    conn_limiter: ConnectionLimiter,
) {
    let mut msg_stream = msg_stream.aggregate_continuations();
    let mut streams = StreamMap::new();

    let mut keepalive = interval(KEEPALIVE_INTERVAL);
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut last_seen = Instant::now();

    loop {
        tokio::select! {
            // Inbound control messages from the client.
            inbound = msg_stream.next() => {
                let Some(Ok(message)) = inbound else { break };

                last_seen = Instant::now();

                match message {
                    AggregatedMessage::Text(text) => {
                        if !handle_text(&text, &mut session, &mut streams, &app_ctx, &http_details, &conn_limiter).await {
                            break;
                        }
                    },
                    AggregatedMessage::Ping(bytes) => {
                        if session.pong(&bytes).await.is_err() {
                            break;
                        }
                    },
                    AggregatedMessage::Pong(_) | AggregatedMessage::Binary(_) => {
                        // Pong (answer to our keepalive) and Binary are no-ops.
                    },
                    AggregatedMessage::Close(_) => break,
                }
            },

            // A subscription produced a frame. The `if` guard disables this arm
            // while the map is empty, since `StreamMap::next` resolves to `None`
            // immediately on an empty map.
            Some((_id, frame)) = streams.next(), if !streams.is_empty() => {
                if session.text(frame).await.is_err() {
                    break;
                }
            },

            // Keepalive + idle reaping.
            _ = keepalive.tick() => {
                if last_seen.elapsed() > IDLE_TIMEOUT {
                    let _ = session.close(None).await;
                    break;
                }
                if session.ping(b"").await.is_err() {
                    break;
                }
            },
        }
    }
}

/// Handle one inbound text frame. Returns `false` when the socket is gone and
/// the loop should stop; protocol-level problems are reported as `error` frames
/// and return `true`.
async fn handle_text(
    text: &str,
    session: &mut Session,
    streams: &mut StreamMap<u64, FrameStream>,
    app_ctx: &FullContext,
    http_details: &HttpRequestDetails,
    conn_limiter: &ConnectionLimiter,
) -> bool {
    let message = match serde_json::from_str::<ClientMessage>(text) {
        Ok(message) => message,
        Err(err) => return send(session, error_frame(None, "badRequest", &err.to_string())).await,
    };

    match message {
        ClientMessage::Subscribe { id, subscription } => {
            // Subscription-scoped errors ride this channel + `id`, mirroring the
            // data frames the subscription would have produced.
            let channel = subscription.channel();

            if streams.contains_key(&id) {
                return send(
                    session,
                    channel_error(channel, id, "badRequest", "id already in use"),
                )
                .await;
            }

            // Reserve a subscription slot; the guard rides inside the stream and
            // releases the slot when the stream is dropped (unsubscribe or
            // disconnect).
            let guard = match conn_limiter.try_acquire() {
                Ok(guard) => Arc::new(guard),
                Err(err) => {
                    return send(
                        session,
                        channel_error(channel, id, "tooManyRequests", &err.message),
                    )
                    .await;
                },
            };

            match open_stream(id, &subscription, guard, app_ctx).await {
                Ok(frames) => {
                    streams.insert(id, frames);
                    send(session, ack(id, "subscribe", Some(channel))).await
                },
                Err(SubscribeError::BadRequest(message)) => {
                    send(session, channel_error(channel, id, "badRequest", &message)).await
                },
                Err(SubscribeError::Resync(message)) => {
                    send(session, channel_error(channel, id, "resync", &message)).await
                },
                Err(SubscribeError::Unavailable(message)) => {
                    send(session, channel_error(channel, id, "unavailable", &message)).await
                },
            }
        },
        ClientMessage::Unsubscribe { id } => {
            if streams.remove(&id).is_some() {
                send(session, ack(id, "unsubscribe", None)).await
            } else {
                send(
                    session,
                    error_frame(Some(id), "unknownSubscription", "no such id"),
                )
                .await
            }
        },
        ClientMessage::Ping { id } => send(session, control(&ServerControl::Pong { id })).await,
        ClientMessage::Broadcast { id, tx } => {
            // Reject an `id` already bound to a live subscription on this socket,
            // so the client can demultiplex the `broadcast` reply unambiguously.
            if streams.contains_key(&id) {
                return send(
                    session,
                    channel_error("broadcast", id, "badRequest", "id already in use"),
                )
                .await;
            }

            // Awaited inline: `broadcast_tx` is a fast mempool admission, so
            // briefly pausing this connection's other arms is acceptable. A
            // mempool rejection comes back as `Ok` (carried in
            // `check_tx.result`); only a transport failure is an `error` frame.
            match crate::broadcast::broadcast_tx(app_ctx, http_details, tx).await {
                Ok(outcome) => send(session, data_frame("broadcast", id, &outcome)).await,
                Err(err) => {
                    send(
                        session,
                        channel_error("broadcast", id, "broadcastFailed", &err.to_string()),
                    )
                    .await
                },
            }
        },
        ClientMessage::Query { id, query } => {
            // Reject an `id` already bound to a live subscription on this socket,
            // so the client can demultiplex the `query` reply unambiguously.
            if streams.contains_key(&id) {
                return send(
                    session,
                    channel_error("query", id, "badRequest", "id already in use"),
                )
                .await;
            }

            // Awaited inline: a query is a fast, in-process state read. Unlike
            // `broadcast`, whose payload type carries its own rejection, a
            // `QueryResponse` is success-only, so a failed query is an `error`
            // frame — the WS mirror of the REST route's `400`. The reply drops
            // the block height `query_app` reports, matching REST `/query`.
            match CoreQuery::_query_app(&app_ctx.base, query).await {
                Ok(res) => send(session, data_frame("query", id, &res.response)).await,
                Err(err) => {
                    send(
                        session,
                        channel_error("query", id, "queryFailed", &err.message),
                    )
                    .await
                },
            }
        },
    }
}

/// Why a subscription could not be opened. Mapped to the co-located error
/// frame's `code` at the `handle_text` call site.
enum SubscribeError {
    /// The request itself is invalid (e.g. a zero `interval`).
    BadRequest(String),

    /// The requested `since` predates the retained window.
    Resync(String),

    /// A server-side dependency is not ready — e.g. the contract addresses
    /// cannot be resolved because the chain has not committed its genesis
    /// state yet. The WS analogue of the REST 503; resubscribing retries.
    Unavailable(String),
}

/// Open the validator stream for a subscription and project it to tagged JSON
/// text frames. The `guard` is moved into the projected stream so it lives
/// exactly as long as the subscription. On a connect-time resync (the requested
/// `since` predates the retained window) the guard is dropped and the resync
/// message returned as `Err`.
async fn open_stream(
    id: u64,
    subscription: &Subscription,
    guard: Arc<crate::subscription_limiter::SubscriptionGuard>,
    app_ctx: &FullContext,
) -> Result<FrameStream, SubscribeError> {
    let stream_ctx = &app_ctx.stream_context;

    match subscription {
        Subscription::PerpsEvents {
            since,
            event_types,
            pair_ids,
            users,
            order_ids,
            client_order_ids,
        } => {
            let filter = make_perps_filter(
                event_types.clone(),
                pair_ids.clone(),
                users.clone(),
                order_ids.clone(),
                client_order_ids.clone(),
            );

            let raw = stream_ctx
                .perps()
                .subscribe(*since, filter)
                .map_err(|resync| SubscribeError::Resync(resync.to_string()))?;
            let frames = guard_subscription_stream(raw, Some(guard))
                .map(move |block| data_frame("perpsEvents", id, &block));

            Ok(with_terminal("perpsEvents", id, frames))
        },
        Subscription::BlockInfo { since } => {
            let raw = stream_ctx
                .blocks()
                .subscribe(*since, |block| Some(block.block.info))
                .map_err(|resync| SubscribeError::Resync(resync.to_string()))?;
            let frames = guard_subscription_stream(raw, Some(guard))
                .map(move |info| data_frame("blockInfo", id, &info));

            Ok(with_terminal("blockInfo", id, frames))
        },
        Subscription::Block { since } => {
            let raw = stream_ctx
                .blocks()
                .subscribe(*since, |block| Some(block.block.clone()))
                .map_err(|resync| SubscribeError::Resync(resync.to_string()))?;
            let frames = guard_subscription_stream(raw, Some(guard))
                .map(move |block| data_frame("block", id, &block));

            Ok(with_terminal("block", id, frames))
        },
        Subscription::FullBlock { since } => {
            let raw = stream_ctx
                .blocks()
                .subscribe(*since, |block| Some(block.clone()))
                .map_err(|resync| SubscribeError::Resync(resync.to_string()))?;
            let frames = guard_subscription_stream(raw, Some(guard))
                .map(move |block| data_frame("fullBlock", id, &block));

            Ok(with_terminal("fullBlock", id, frames))
        },
        Subscription::Query { query, interval } => open_standing_query(
            id,
            query.clone(),
            *interval,
            "query",
            Projection::Verbatim,
            guard,
            app_ctx,
        ),
        Subscription::PerpsPairState { params, interval } => {
            let query = desugar_perps_query(app_ctx, &params.to_pair_state_msg()).await?;

            open_standing_query(
                id,
                query,
                *interval,
                "perpsPairState",
                Projection::UnwrapWasmSmart,
                guard,
                app_ctx,
            )
        },
        Subscription::PerpsUserState { params, interval } => {
            let query = desugar_perps_query(app_ctx, &params.to_query_msg()).await?;

            open_standing_query(
                id,
                query,
                *interval,
                "perpsUserState",
                Projection::UnwrapWasmSmart,
                guard,
                app_ctx,
            )
        },
        Subscription::PerpsOrdersByUser { params, interval } => {
            let query = desugar_perps_query(app_ctx, &params.to_query_msg()).await?;

            open_standing_query(
                id,
                query,
                *interval,
                "perpsOrdersByUser",
                Projection::UnwrapWasmSmart,
                guard,
                app_ctx,
            )
        },
        Subscription::PerpsLiquidityDepth { params, interval } => {
            let query = desugar_perps_query(app_ctx, &params.to_query_msg()).await?;

            open_standing_query(
                id,
                query,
                *interval,
                "perpsLiquidityDepth",
                Projection::UnwrapWasmSmart,
                guard,
                app_ctx,
            )
        },
    }
}

/// Resolve the perps contract address and wrap an alias subscription's query
/// message into the `wasm_smart` query it desugars to. The resolved query is
/// byte-identical to what an equivalent raw `query` subscription would carry,
/// so the two share memo entries.
async fn desugar_perps_query(
    app_ctx: &FullContext,
    msg: &perps::QueryMsg,
) -> Result<Query, SubscribeError> {
    let contract = app_ctx.base.perps_address().await.map_err(|err| {
        SubscribeError::Unavailable(format!(
            "failed to resolve the perps contract address: {err}"
        ))
    })?;

    Query::wasm_smart(contract, msg).map_err(|err| SubscribeError::BadRequest(err.to_string()))
}

/// Open a standing query: a memo-backed initial snapshot at subscribe time,
/// then a re-run once per block whose height is a multiple of `interval`,
/// projected to frames on `channel`. Shared by the raw `query` subscription
/// and the alias subscriptions that desugar to it.
fn open_standing_query(
    id: u64,
    query: Query,
    interval: u64,
    channel: &'static str,
    projection: Projection,
    guard: Arc<crate::subscription_limiter::SubscriptionGuard>,
    app_ctx: &FullContext,
) -> Result<FrameStream, SubscribeError> {
    if interval == 0 {
        return Err(SubscribeError::BadRequest(
            "`interval` must be >= 1".to_string(),
        ));
    }

    let blocks = app_ctx.stream_context.blocks();

    // The initial snapshot is keyed by the ring tip, so simultaneous
    // identical subscriptions (e.g. a reconnect storm) share one
    // execution. Height 0 covers the empty-ring case, before the first
    // block is published.
    let tip = blocks.tip().unwrap_or(0);

    // The interval filter lives in the ring projection: non-matching
    // blocks are suppressed (the subscriber's watermark still advances
    // over them) and matching ones are projected to their height. The
    // alignment is absolute — `height % interval == 0` — so every
    // subscription with the same query and interval ticks at the same
    // heights, which is what lets the memo collapse them.
    let raw = blocks
        .subscribe(None, move |block| {
            let height = block.block.info.height;
            height.is_multiple_of(interval).then_some(height)
        })
        .map_err(|resync| SubscribeError::Resync(resync.to_string()))?;

    let memo = app_ctx.query_memo.clone();
    let base = app_ctx.base.clone();

    let initial = {
        let memo = memo.clone();
        let base = base.clone();
        let query = query.clone();

        stream::once(async move { memo.query_at(tip, query, base).await })
    };
    let ticks = raw.then(move |height| {
        let memo = memo.clone();
        let base = base.clone();
        let query = query.clone();

        async move { memo.query_at(height, query, base).await }
    });

    let items = guard_subscription_stream(initial.chain(ticks), Some(guard));

    Ok(query_frames(channel, id, items, projection))
}

/// Append a terminal `error` frame so that when a subscription ends on its own
/// — the validator window evicted the next block the subscriber needed, or the
/// feed closed — the client learns to resync instead of seeing the feed go
/// silent. An explicit `unsubscribe` removes the stream from the map before this
/// runs, so it never fires spuriously.
fn with_terminal<S>(channel: &'static str, id: u64, frames: S) -> FrameStream
where
    S: stream::Stream<Item = String> + Send + 'static,
{
    let terminal = stream::once(async move {
        channel_error(
            channel,
            id,
            "resync",
            "subscription ended; reconnect with a newer `since`",
        )
    });

    Box::pin(frames.chain(terminal))
}

/// How a standing query's executions are rendered into data frames.
#[derive(Clone, Copy)]
enum Projection {
    /// The full [`QueryFrame`], its `response` being the enveloped
    /// `QueryResponse` — the raw `query` subscription's format.
    Verbatim,

    /// The `response` unwrapped to the raw contract response inside the
    /// `wasm_smart` envelope — the alias subscriptions' format, mirroring
    /// what their REST twins return. The memoized value stays enveloped, so
    /// coalescing with raw subscriptions is unaffected.
    UnwrapWasmSmart,
}

/// The alias subscriptions' frame: the same `{blockHeight, response}` shape
/// as [`QueryFrame`], with `response` borrowing the unwrapped contract
/// response out of the memo's shared frame — unwrapping never copies the
/// payload.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AliasQueryFrame<'a> {
    block_height: u64,
    response: &'a Json,
}

/// Serialize one standing-query execution into a data frame, per the
/// subscription's projection.
fn standing_query_frame(
    channel: &'static str,
    id: u64,
    frame: &QueryFrame,
    projection: Projection,
) -> String {
    match (projection, &frame.response) {
        (Projection::UnwrapWasmSmart, QueryResponse::WasmSmart(inner)) => data_frame(
            channel,
            id,
            &AliasQueryFrame {
                block_height: frame.block_height,
                response: inner,
            },
        ),
        // A non-`wasm_smart` response is unreachable for desugared alias
        // queries; fall back to the verbatim envelope rather than dropping
        // the frame.
        (Projection::UnwrapWasmSmart, _) | (Projection::Verbatim, _) => {
            data_frame(channel, id, frame)
        },
    }
}

/// Project a standing query's executions to tagged JSON text frames on
/// `channel`, enforcing the per-subscriber delivery contract:
///
/// - Each frame's `blockHeight` strictly exceeds the previous frame's. A tick
///   whose result would duplicate an already-delivered height — possible when
///   execution lags a full interval behind the ring, since a query always
///   reads the *latest* state — is skipped, not re-sent.
/// - A failed execution is a single terminal `queryFailed` error frame; the
///   subscription ends without a trailing `resync`.
/// - A feed that ends on its own (the ring outpaced the subscriber, or
///   shutdown) closes with the standard terminal `resync` frame; there is no
///   `since` replay, so "resync" here simply means resubscribe for a fresh
///   snapshot.
fn query_frames<S>(channel: &'static str, id: u64, items: S, projection: Projection) -> FrameStream
where
    S: stream::Stream<Item = Result<Arc<QueryFrame>, String>> + Send + 'static,
{
    enum Tick {
        Item(Result<Arc<QueryFrame>, String>),
        Ended,
    }

    let tagged = items
        .map(Tick::Item)
        .chain(stream::once(async { Tick::Ended }));

    let frames = tagged
        .scan((None::<u64>, false), move |(last_height, done), tick| {
            let out = if *done {
                // A terminal frame has been emitted; end the stream. This is
                // what keeps the chained `Ended` from adding a `resync` after
                // a `queryFailed`.
                None
            } else {
                Some(match tick {
                    // The first frame is always delivered (a fresh chain
                    // legitimately serves height 0); later ones must advance.
                    Tick::Item(Ok(frame))
                        if last_height.is_none_or(|last| frame.block_height > last) =>
                    {
                        *last_height = Some(frame.block_height);
                        Some(standing_query_frame(
                            channel,
                            id,
                            frame.as_ref(),
                            projection,
                        ))
                    },
                    // A lag-induced repeat of an already-delivered height: the
                    // state is unchanged, so the content is identical — skip.
                    Tick::Item(Ok(_)) => None,
                    Tick::Item(Err(message)) => {
                        *done = true;
                        Some(channel_error(channel, id, "queryFailed", &message))
                    },
                    Tick::Ended => {
                        *done = true;
                        Some(channel_error(
                            channel,
                            id,
                            "resync",
                            "subscription ended; resubscribe for a fresh snapshot",
                        ))
                    },
                })
            };

            std::future::ready(out)
        })
        .filter_map(std::future::ready);

    Box::pin(frames)
}

// ---- serialization helpers ----

async fn send(session: &mut Session, text: String) -> bool {
    session.text(text).await.is_ok()
}

fn control(message: &ServerControl) -> String {
    serde_json::to_string(message).unwrap_or_else(|_| {
        r#"{"channel":"error","error":{"code":"internal","message":"serialization failed"}}"#
            .to_string()
    })
}

fn ack(id: u64, method: &str, channel: Option<&str>) -> String {
    control(&ServerControl::SubscriptionResponse {
        id,
        data: Ack { method, channel },
    })
}

fn error_frame(id: Option<u64>, code: &str, message: &str) -> String {
    control(&ServerControl::Error {
        id,
        error: ErrorData { code, message },
    })
}

/// Serialize an error scoped to a subscription, delivered on that subscription's
/// own `channel` and `id` — the co-located counterpart to [`data_frame`].
fn channel_error(channel: &'static str, id: u64, code: &str, message: &str) -> String {
    serde_json::to_string(&ErrorFrame {
        channel,
        id,
        error: ErrorData { code, message },
    })
    .unwrap_or_else(|_| error_frame(Some(id), "internal", "serialization failed"))
}

fn data_frame<T>(channel: &'static str, id: u64, data: &T) -> String
where
    T: Serialize,
{
    serde_json::to_string(&DataFrame { channel, id, data })
        .unwrap_or_else(|err| error_frame(Some(id), "internal", &err.to_string()))
}

// ----------------------------------- Tests -----------------------------------

#[cfg(test)]
mod tests {
    use {super::*, serde_json::json};

    #[test]
    fn deserializes_subscribe_perps_events_with_filters() {
        let message: ClientMessage = serde_json::from_str(
            r#"{"method":"subscribe","id":1,"subscription":{"type":"perpsEvents","since":42,"eventTypes":["order_filled"],"pairIds":["perp/btcusd"]}}"#,
        )
        .unwrap();

        let ClientMessage::Subscribe { id, subscription } = message else {
            panic!("expected a subscribe message");
        };

        assert_eq!(id, 1);
        assert_eq!(subscription.channel(), "perpsEvents");

        let Subscription::PerpsEvents {
            since,
            event_types,
            pair_ids,
            users,
            order_ids,
            client_order_ids,
        } = subscription
        else {
            panic!("expected a perpsEvents subscription");
        };

        assert_eq!(since, Some(42));
        assert_eq!(
            event_types,
            Some(HashSet::from(["order_filled".to_string()]))
        );
        assert_eq!(pair_ids, Some(HashSet::from(["perp/btcusd".to_string()])));
        // Absent filters deserialize to `None` (match-all).
        assert_eq!(users, None);
        assert_eq!(order_ids, None);
        assert_eq!(client_order_ids, None);
    }

    #[test]
    fn empty_filter_is_match_nothing_absent_is_match_all() {
        let message: ClientMessage = serde_json::from_str(
            r#"{"method":"subscribe","id":1,"subscription":{"type":"perpsEvents","pairIds":[]}}"#,
        )
        .unwrap();

        let ClientMessage::Subscribe { subscription, .. } = message else {
            panic!("expected a subscribe message");
        };

        let Subscription::PerpsEvents {
            pair_ids, users, ..
        } = subscription
        else {
            panic!("expected a perpsEvents subscription");
        };

        // An empty set matches nothing; an absent one is `None` (match-all).
        assert_eq!(pair_ids, Some(HashSet::new()));
        assert_eq!(users, None);
    }

    #[test]
    fn deserializes_full_block_unsubscribe_and_ping() {
        assert!(matches!(
            serde_json::from_str::<ClientMessage>(
                r#"{"method":"subscribe","id":2,"subscription":{"type":"fullBlock"}}"#,
            )
            .unwrap(),
            ClientMessage::Subscribe { id: 2, .. }
        ));

        assert!(matches!(
            serde_json::from_str::<ClientMessage>(r#"{"method":"unsubscribe","id":2}"#).unwrap(),
            ClientMessage::Unsubscribe { id: 2 }
        ));

        assert!(matches!(
            serde_json::from_str::<ClientMessage>(r#"{"method":"ping","id":9}"#).unwrap(),
            ClientMessage::Ping { id: Some(9) }
        ));

        // `id` is optional on ping.
        assert!(matches!(
            serde_json::from_str::<ClientMessage>(r#"{"method":"ping"}"#).unwrap(),
            ClientMessage::Ping { id: None }
        ));
    }

    #[test]
    fn deserializes_block_info_and_block() {
        // `blockInfo`, with a `since` cursor.
        let message: ClientMessage = serde_json::from_str(
            r#"{"method":"subscribe","id":3,"subscription":{"type":"blockInfo","since":42}}"#,
        )
        .unwrap();

        let ClientMessage::Subscribe { id, subscription } = message else {
            panic!("expected a subscribe message");
        };

        assert_eq!(id, 3);
        assert_eq!(subscription.channel(), "blockInfo");
        assert!(matches!(
            subscription,
            Subscription::BlockInfo { since: Some(42) }
        ));

        // `block`, without `since` (live-only).
        let message: ClientMessage = serde_json::from_str(
            r#"{"method":"subscribe","id":4,"subscription":{"type":"block"}}"#,
        )
        .unwrap();

        let ClientMessage::Subscribe { id, subscription } = message else {
            panic!("expected a subscribe message");
        };

        assert_eq!(id, 4);
        assert_eq!(subscription.channel(), "block");
        assert!(matches!(subscription, Subscription::Block { since: None }));
    }

    #[test]
    fn serializes_control_frames() {
        let parse = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();

        assert_eq!(
            parse(&ack(1, "subscribe", Some("perpsEvents"))),
            json!({"channel": "subscriptionResponse", "id": 1, "data": {"method": "subscribe", "type": "perpsEvents"}}),
        );

        // The unsubscribe ack omits the channel `type`.
        assert_eq!(
            parse(&ack(2, "unsubscribe", None)),
            json!({"channel": "subscriptionResponse", "id": 2, "data": {"method": "unsubscribe"}}),
        );

        assert_eq!(
            parse(&control(&ServerControl::Pong { id: Some(9) })),
            json!({"channel": "pong", "id": 9}),
        );

        // A null `id` is omitted, not serialized.
        assert_eq!(
            parse(&control(&ServerControl::Pong { id: None })),
            json!({"channel": "pong"}),
        );

        assert_eq!(
            parse(&error_frame(Some(3), "resync", "stale")),
            json!({"channel": "error", "id": 3, "error": {"code": "resync", "message": "stale"}}),
        );
    }

    #[test]
    fn serializes_data_frame_with_subscription_id() {
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&data_frame(
                "perpsEvents",
                1,
                &json!({"blockHeight": 5}),
            ))
            .unwrap(),
            json!({"channel": "perpsEvents", "id": 1, "data": {"blockHeight": 5}}),
        );
    }

    #[test]
    fn serializes_colocated_error_on_its_subscription_channel() {
        // A subscription-scoped error rides that subscription's own channel and
        // `id`, as an `error`-keyed sibling of its `data`-keyed frames.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&channel_error(
                "perpsEvents",
                1,
                "resync",
                "stale",
            ))
            .unwrap(),
            json!({"channel": "perpsEvents", "id": 1, "error": {"code": "resync", "message": "stale"}}),
        );
    }

    #[test]
    fn deserializes_broadcast_with_tx() {
        let message: ClientMessage = serde_json::from_str(
            r#"{"method":"broadcast","id":5,"tx":{"sender":"0x33361de42571d6aa20c37daa6da4b5ab67bfaad9","gas_limit":1000000,"msgs":[{"transfer":{"0x01bba610cbbfe9df0c99b8862f3ad41b2f646553":{"hyp/all/btc":"100"}}}],"data":{"chain_id":"dev-1","nonce":1,"username":"owner"},"credential":{}}}"#,
        )
        .unwrap();

        assert!(matches!(message, ClientMessage::Broadcast { id: 5, .. }));
    }

    #[test]
    fn serializes_broadcast_success_frame() {
        // A successful broadcast rides a `data` frame on the `broadcast`
        // channel, exactly like a subscription's data frame.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&data_frame(
                "broadcast",
                5,
                &json!({"tx_hash": "0xabc", "check_tx": {"result": {"Ok": null}}}),
            ))
            .unwrap(),
            json!({"channel": "broadcast", "id": 5, "data": {"tx_hash": "0xabc", "check_tx": {"result": {"Ok": null}}}}),
        );
    }

    #[test]
    fn serializes_broadcast_error_frame() {
        // A transport failure to the consensus node is an `error` frame on the
        // `broadcast` channel (a mempool rejection would be a `data` frame).
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&channel_error(
                "broadcast",
                5,
                "broadcastFailed",
                "connection refused",
            ))
            .unwrap(),
            json!({"channel": "broadcast", "id": 5, "error": {"code": "broadcastFailed", "message": "connection refused"}}),
        );
    }

    #[test]
    fn deserializes_query() {
        // A parameterless query variant.
        let message: ClientMessage =
            serde_json::from_str(r#"{"method":"query","id":5,"query":{"config":{}}}"#).unwrap();

        let ClientMessage::Query { id, query } = message else {
            panic!("expected a query message");
        };

        assert_eq!(id, 5);
        assert!(matches!(query, Query::Config(_)));

        // A variant with fields, proving the nested payload parses.
        let message: ClientMessage = serde_json::from_str(
            r#"{"method":"query","id":6,"query":{"balance":{"address":"0x33361de42571d6aa20c37daa6da4b5ab67bfaad9","denom":"hyp/all/btc"}}}"#,
        )
        .unwrap();

        assert!(matches!(
            message,
            ClientMessage::Query {
                id: 6,
                query: Query::Balance(_)
            }
        ));
    }

    #[test]
    fn deserializes_query_subscription() {
        let message: ClientMessage = serde_json::from_str(
            r#"{"method":"subscribe","id":7,"subscription":{"type":"query","query":{"config":{}},"interval":5}}"#,
        )
        .unwrap();

        let ClientMessage::Subscribe { id, subscription } = message else {
            panic!("expected a subscribe message");
        };

        assert_eq!(id, 7);
        assert_eq!(subscription.channel(), "query");

        let Subscription::Query { query, interval } = subscription else {
            panic!("expected a query subscription");
        };

        assert!(matches!(query, Query::Config(_)));
        assert_eq!(interval, 5);

        // An absent `interval` defaults to 10 (every tenth block).
        let message: ClientMessage = serde_json::from_str(
            r#"{"method":"subscribe","id":8,"subscription":{"type":"query","query":{"config":{}}}}"#,
        )
        .unwrap();

        let ClientMessage::Subscribe { subscription, .. } = message else {
            panic!("expected a subscribe message");
        };

        assert!(matches!(
            subscription,
            Subscription::Query { interval: 10, .. }
        ));
    }

    #[test]
    fn query_frame_stream_is_monotonic_with_terminal_error() {
        use dango_primitives::QueryResponse;

        let frame = |height: u64| {
            Ok(Arc::new(QueryFrame {
                block_height: height,
                response: QueryResponse::WasmRaw(None),
            }))
        };

        // A lag-induced duplicate height is skipped; a failed execution is a
        // single terminal `queryFailed` frame with no trailing `resync`.
        let items = stream::iter(vec![frame(5), frame(5), frame(6), Err("boom".to_string())]);
        let got = futures::executor::block_on(
            query_frames("query", 9, items, Projection::Verbatim).collect::<Vec<_>>(),
        );

        assert_eq!(got.len(), 3);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&got[0]).unwrap(),
            json!({"channel": "query", "id": 9, "data": {"blockHeight": 5, "response": {"wasm_raw": null}}}),
        );
        assert!(got[1].contains(r#""blockHeight":6"#));
        assert_eq!(got[2], channel_error("query", 9, "queryFailed", "boom"));

        // A feed that ends on its own closes with the terminal `resync` frame.
        let items = stream::iter(vec![frame(5)]);
        let got = futures::executor::block_on(
            query_frames("query", 9, items, Projection::Verbatim).collect::<Vec<_>>(),
        );

        assert_eq!(got.len(), 2);
        assert!(got[1].contains(r#""code":"resync""#), "got: {:?}", got[1]);

        // The first frame is delivered whatever its height — a fresh chain
        // legitimately serves height 0 — and only repeats are skipped.
        let items = stream::iter(vec![frame(0), frame(0), frame(1)]);
        let got = futures::executor::block_on(
            query_frames("query", 9, items, Projection::Verbatim).collect::<Vec<_>>(),
        );

        assert_eq!(got.len(), 3, "got: {got:?}"); // heights 0 and 1, then resync
        assert!(got[0].contains(r#""blockHeight":0"#));
        assert!(got[1].contains(r#""blockHeight":1"#));
    }

    #[test]
    fn serializes_query_frames() {
        // A successful query rides a `data` frame on the `query` channel,
        // carrying the raw `QueryResponse`.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&data_frame(
                "query",
                5,
                &json!({"balance": {"denom": "hyp/all/btc", "amount": "12345"}}),
            ))
            .unwrap(),
            json!({"channel": "query", "id": 5, "data": {"balance": {"denom": "hyp/all/btc", "amount": "12345"}}}),
        );

        // A failed query is an `error` frame on the `query` channel — unlike
        // `broadcast`, whose payload type carries its own rejection.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&channel_error(
                "query",
                5,
                "queryFailed",
                "data not found",
            ))
            .unwrap(),
            json!({"channel": "query", "id": 5, "error": {"code": "queryFailed", "message": "data not found"}}),
        );
    }

    #[test]
    fn deserializes_perps_alias_subscriptions() {
        use dango_primitives::{Addr, JsonSerExt};

        // Snake_case wire params inside the camelCase envelope, explicit
        // `interval`.
        let message: ClientMessage = serde_json::from_str(
            r#"{"method":"subscribe","id":5,"subscription":{"type":"perpsLiquidityDepth","pair_id":"perp/ethusd","bucket_size":"10","limit":5,"interval":1}}"#,
        )
        .unwrap();

        let ClientMessage::Subscribe {
            id: 5,
            subscription,
        } = message
        else {
            panic!("expected a subscribe message with id 5");
        };

        assert_eq!(subscription.channel(), "perpsLiquidityDepth");

        let Subscription::PerpsLiquidityDepth { params, interval } = subscription else {
            panic!("expected a perpsLiquidityDepth subscription");
        };

        assert_eq!(interval, 1);

        // The desugared wire message is exactly what an equivalent raw
        // `query` subscription would embed — the memo-coalescing guarantee,
        // pinned against an explicit literal.
        assert_eq!(
            params.to_query_msg().to_json_value().unwrap(),
            dango_primitives::json!({
                "liquidity_depth": {
                    "pair_id": "perp/ethusd",
                    "bucket_size": "10",
                    "limit": 5,
                },
            }),
        );

        // `interval` and the `include_*` flags default when absent.
        let user = Addr::mock(1);
        let message: ClientMessage = serde_json::from_str(&format!(
            r#"{{"method":"subscribe","id":6,"subscription":{{"type":"perpsUserState","user":"{user}","include_all":true}}}}"#,
        ))
        .unwrap();

        let ClientMessage::Subscribe { subscription, .. } = message else {
            panic!("expected a subscribe message");
        };

        assert_eq!(subscription.channel(), "perpsUserState");

        let Subscription::PerpsUserState { params, interval } = subscription else {
            panic!("expected a perpsUserState subscription");
        };

        assert_eq!(interval, default_query_interval());
        assert_eq!(
            params.to_query_msg().to_json_value().unwrap(),
            dango_primitives::json!({
                "user_state_extended": {
                    "user": user,
                    "include_equity": false,
                    "include_available_margin": false,
                    "include_maintenance_margin": false,
                    "include_unrealized_pnl": false,
                    "include_unrealized_funding": false,
                    "include_liquidation_price": false,
                    "include_all": true,
                },
            }),
        );

        // The two remaining aliases, briefly.
        let message: ClientMessage = serde_json::from_str(
            r#"{"method":"subscribe","id":7,"subscription":{"type":"perpsPairState","pair_id":"perp/ethusd"}}"#,
        )
        .unwrap();

        let ClientMessage::Subscribe { subscription, .. } = message else {
            panic!("expected a subscribe message");
        };

        assert_eq!(subscription.channel(), "perpsPairState");

        let Subscription::PerpsPairState { params, .. } = subscription else {
            panic!("expected a perpsPairState subscription");
        };

        assert_eq!(
            params.to_pair_state_msg().to_json_value().unwrap(),
            dango_primitives::json!({"pair_state": {"pair_id": "perp/ethusd"}}),
        );

        let message: ClientMessage = serde_json::from_str(&format!(
            r#"{{"method":"subscribe","id":8,"subscription":{{"type":"perpsOrdersByUser","user":"{user}"}}}}"#,
        ))
        .unwrap();

        let ClientMessage::Subscribe { subscription, .. } = message else {
            panic!("expected a subscribe message");
        };

        assert_eq!(subscription.channel(), "perpsOrdersByUser");

        let Subscription::PerpsOrdersByUser { params, .. } = subscription else {
            panic!("expected a perpsOrdersByUser subscription");
        };

        assert_eq!(
            params.to_query_msg().to_json_value().unwrap(),
            dango_primitives::json!({"orders_by_user": {"user": user}}),
        );
    }

    #[test]
    fn alias_frames_unwrap_the_wasm_smart_envelope() {
        let frame = QueryFrame {
            block_height: 7,
            response: QueryResponse::WasmSmart(dango_primitives::json!({"long_oi": "5"})),
        };

        // The alias projection unwraps the `wasm_smart` envelope, so the
        // frame's `response` is what the REST twin returns.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&standing_query_frame(
                "perpsPairState",
                3,
                &frame,
                Projection::UnwrapWasmSmart,
            ))
            .unwrap(),
            json!({
                "channel": "perpsPairState",
                "id": 3,
                "data": {"blockHeight": 7, "response": {"long_oi": "5"}},
            }),
        );

        // The raw `query` projection keeps the envelope.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&standing_query_frame(
                "query",
                3,
                &frame,
                Projection::Verbatim,
            ))
            .unwrap(),
            json!({
                "channel": "query",
                "id": 3,
                "data": {"blockHeight": 7, "response": {"wasm_smart": {"long_oi": "5"}}},
            }),
        );
    }
}
