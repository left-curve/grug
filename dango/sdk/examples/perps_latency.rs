//! Measure perps round-trip latency over one shared native `/ws` socket.
//!
//! The Rust twin of `sdk/python/examples/native_perps_latency.py`. It uses a
//! single long-lived [`WsConnection`] for **everything** — the `perps_events`
//! subscription *and* both the place and cancel broadcasts ride the same socket,
//! which is exactly what a latency-sensitive bot wants (no second connection, no
//! per-broadcast handshake). Orders are signed locally with a [`SingleSigner`]
//! and sent with `conn.broadcast(tx).await`.
//!
//! Each cycle places a resting limit buy 1% below the index price (so it never
//! crosses the book), cancels it the instant its `order_persisted` event
//! arrives, and records two latencies per action, both anchored at the instant
//! before the broadcast call:
//!
//! * `mempool` — when `conn.broadcast` returns (the tx is admitted to the
//!   mempool). An explicit `gas_limit` skips the pre-broadcast `simulate`, so
//!   this is just signing plus the `/ws` broadcast hop.
//! * `confirm` — when the matching lifecycle event (`order_persisted` for the
//!   place, `order_removed` for the cancel) arrives back over the same
//!   `perps_events` subscription: broadcast → block inclusion → indexer → push.
//!
//! So `confirm >= mempool` always, since both share the same start anchor.
//!
//! Reads two env vars (mirroring the Python example's `examples/.env`):
//! `DANGO_SECRET_KEY` (32-byte secp256k1 secret, hex) and `DANGO_ACCOUNT_ADDRESS`
//! (the funded account). The account must have perps margin on the target chain.
//!
//! Run with:
//!
//! ```sh
//! DANGO_SECRET_KEY=... DANGO_ACCOUNT_ADDRESS=0x... \
//!   cargo run -p dango-sdk --example native_perps_latency
//! ```

use {
    anyhow::{Result, anyhow, bail},
    dango_math::Uint64,
    dango_order_book::{
        ClientOrderId, Dimensionless, OrderKind, OrderPersisted, OrderRemoved, Quantity,
        TimeInForce, UsdPrice,
    },
    dango_primitives::{Addr, Coins, EventName, Message, NonEmpty, QueryClientExt, Signer},
    dango_sdk::{
        HttpClient, PerpsEventsBatch, Secp256k1, Secret, SingleSigner, Subscription, WsConnection,
    },
    dango_types::{
        config::AppConfig,
        constants::perp_btc,
        perps::{self, PairParam, PairState},
    },
    futures::StreamExt,
    std::{
        env,
        str::FromStr,
        time::{Duration, Instant},
    },
};

// --- Configuration -----------------------------------------------------------

/// HTTP base URL (testnet), used by `HttpClient` for the index-price / tick
/// queries.
const API_URL: &str = "https://api-testnet.dango.zone";

/// Native `/ws` endpoint (testnet), used by `WsConnection` for the subscription
/// and both broadcasts. Must be a `ws://` / `wss://` URL.
const WS_URL: &str = "wss://api-testnet.dango.zone/ws";

/// Number of place + cancel cycles to sample.
const ITERATIONS: usize = 30;

/// Order size (0.001 BTC). If the chain rejects it on a minimum-size / notional
/// rule, increase it.
const SIZE_PERMILLE: i128 = 1;

/// Explicit gas limit so the SDK path never simulates — the whole point when
/// measuring latency. Must cover the tx, else the chain rejects it.
const GAS_LIMIT: u64 = 200_000;

/// Max wait for a lifecycle event before bailing on a cycle.
const EVENT_TIMEOUT: Duration = Duration::from_secs(30);

/// Let the first subscription go live before the first order.
const SETTLE: Duration = Duration::from_secs(1);

/// Brief breather between cycles.
const PAUSE_BETWEEN: Duration = Duration::from_millis(250);

// --- Event correlation -------------------------------------------------------

/// When and where an awaited lifecycle event arrived.
struct EventMatch {
    /// Monotonic arrival time, for latency math.
    at: Instant,
    /// The block the event was emitted in.
    block_height: u64,
}

/// Block until an `event_type` event carrying `coid` arrives on `sub`; return
/// its arrival info. Bails with a timeout if none arrives within `EVENT_TIMEOUT`.
async fn await_event(
    sub: &mut Subscription<PerpsEventsBatch>,
    event_type: &str,
    coid: &str,
) -> Result<EventMatch> {
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut observed = 0usize;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        let batch = match tokio::time::timeout(remaining, sub.next()).await {
            Ok(Some(item)) => item?,
            Ok(None) => bail!("subscription ended before {event_type}"),
            Err(_) => break,
        };

        // Stamp arrival the instant the batch is delivered.
        let at = Instant::now();
        for event in &batch.events {
            observed += 1;
            // The subscription is already filtered to this coid server-side; the
            // check is defensive (and guards the block-of-mixed-events case).
            if event.event_type == event_type && event.client_order_id.as_deref() == Some(coid) {
                return Ok(EventMatch {
                    at,
                    block_height: batch.block_height,
                });
            }
        }
    }

    bail!(
        "timed out after {EVENT_TIMEOUT:?} waiting for {event_type} (client_order_id={coid}); observed {observed} event(s)"
    )
}

// --- Helpers -----------------------------------------------------------------

/// Build a signed `submit_order` tx: a resting GTC limit buy at `limit_price`,
/// carrying `coid`. Signing advances the signer's local nonce.
fn sign_submit_order(
    signer: &mut SingleSigner<Secp256k1>,
    chain_id: &str,
    perps: Addr,
    pair_id: &dango_order_book::PairId,
    limit_price: UsdPrice,
    coid: ClientOrderId,
) -> Result<dango_primitives::Tx> {
    let msg = Message::execute(
        perps,
        &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
            pair_id: pair_id.clone(),
            size: Quantity::new_permille(SIZE_PERMILLE),
            kind: OrderKind::Limit {
                limit_price,
                time_in_force: TimeInForce::GoodTilCanceled,
                client_order_id: Some(coid),
            },
            reduce_only: false,
            tp: None,
            sl: None,
        })),
        Coins::new(),
    )?;

    Ok(signer.sign_transaction(NonEmpty::new_unchecked(vec![msg]), chain_id, GAS_LIMIT)?)
}

/// Build a signed `cancel_order` tx that cancels the order carrying `coid`.
fn sign_cancel_order(
    signer: &mut SingleSigner<Secp256k1>,
    chain_id: &str,
    perps: Addr,
    coid: ClientOrderId,
) -> Result<dango_primitives::Tx> {
    let msg = Message::execute(
        perps,
        &perps::ExecuteMsg::Trade(perps::TraderMsg::CancelOrder(
            perps::CancelOrderRequest::OneByClientOrderId(coid),
        )),
        Coins::new(),
    )?;

    Ok(signer.sign_transaction(NonEmpty::new_unchecked(vec![msg]), chain_id, GAS_LIMIT)?)
}

/// Current oracle index price for `pair_id`.
async fn index_price(
    client: &HttpClient,
    perps: Addr,
    pair_id: &dango_order_book::PairId,
) -> Result<UsdPrice> {
    let state: Option<PairState> = client
        .query_wasm_smart(
            perps,
            perps::QueryPairStateRequest {
                pair_id: pair_id.clone(),
            },
        )
        .await?;

    Ok(state
        .ok_or_else(|| anyhow!("no pair_state for {pair_id}"))?
        .index_price)
}

/// Static price tick size for `pair_id`; limit prices must be a multiple of it.
async fn tick_size(
    client: &HttpClient,
    perps: Addr,
    pair_id: &dango_order_book::PairId,
) -> Result<UsdPrice> {
    let param: Option<PairParam> = client
        .query_wasm_smart(
            perps,
            perps::QueryPairParamRequest {
                pair_id: pair_id.clone(),
            },
        )
        .await?;

    Ok(param
        .ok_or_else(|| anyhow!("no pair_param for {pair_id}"))?
        .tick_size)
}

/// 95th percentile by nearest-rank on the sorted samples.
fn p95(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let rank = ((0.95 * sorted.len() as f64).ceil() as usize).max(1);
    sorted[rank - 1]
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    sorted[sorted.len() / 2]
}

fn print_summary(metrics: &[(&str, Vec<f64>)]) {
    println!("\n=== round-trip latency (ms) ===");
    println!("  mempool = conn.broadcast returned (tx admitted to mempool over /ws)");
    println!("  confirm = lifecycle event received over perps_events (on-chain + indexed)\n");
    println!(
        "{:<16}{:>4}{:>9}{:>9}{:>9}{:>9}{:>9}",
        "metric", "n", "min", "mean", "median", "p95", "max"
    );
    println!("{}", "-".repeat(65));

    for (name, xs) in metrics {
        if xs.is_empty() {
            println!("{name:<16}{:>4}{:>45}", 0, "no samples");
            continue;
        }
        let min = xs.iter().copied().fold(f64::INFINITY, f64::min);
        let max = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        println!(
            "{name:<16}{:>4}{:>9.1}{:>9.1}{:>9.1}{:>9.1}{:>9.1}",
            xs.len(),
            min,
            mean(xs),
            median(xs),
            p95(xs),
            max,
        );
    }
}

// --- Main --------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    // Credentials from the environment (mirrors the Python `examples/.env`).
    let secret = {
        let secret_hex = env::var("DANGO_SECRET_KEY")
            .map_err(|_| anyhow!("set DANGO_SECRET_KEY to the 32-byte secp256k1 secret (hex)"))?;
        let secret_bytes: [u8; 32] = hex::decode(secret_hex.trim().trim_start_matches("0x"))?
            .try_into()
            .map_err(|_| anyhow!("DANGO_SECRET_KEY must be exactly 32 bytes of hex"))?;
        Secp256k1::from_bytes(secret_bytes)?
    };
    let address = {
        let hex = env::var("DANGO_ACCOUNT_ADDRESS")
            .map_err(|_| anyhow!("set DANGO_ACCOUNT_ADDRESS to the funded account address"))?;
        Addr::from_str(hex.trim())?
    };

    let client = HttpClient::new(API_URL)?;

    // Resolve chain_id and the perps contract from the chain.
    let chain_id = client.query_status().await?.chain_id;
    let perps = client
        .query_app_config::<AppConfig>()
        .await?
        .addresses
        .perps;
    let pair_id = perp_btc::DENOM.clone();

    // Resolve the signer's user_index and next nonce, then it can sign locally.
    let mut signer = SingleSigner::new(address, secret)
        .with_query_user_index(&client)
        .await?
        .with_query_nonce(&client)
        .await?;

    // Tick size is a static pair parameter; fetch it once. A limit price must be
    // an integer multiple of it or the chain rejects the order.
    let tick = tick_size(&client, perps, &pair_id).await?;

    println!("account {address}");
    println!("measuring {ITERATIONS} place/cancel cycles on {pair_id} via {API_URL}");
    println!("one WsConnection carries the perps_events feed AND both broadcasts\n");

    // One socket for the whole run: subscriptions AND broadcasts.
    let conn = WsConnection::connect(WS_URL).await?;

    // Per-run client_order_id base (ms since epoch); each cycle adds its index.
    let coid_base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;

    let mut place_mempool = Vec::new();
    let mut place_confirm = Vec::new();
    let mut cancel_mempool = Vec::new();
    let mut cancel_confirm = Vec::new();

    for i in 0..ITERATIONS {
        let coid_int = coid_base + i as u64;
        let coid: ClientOrderId = Uint64::new(coid_int);
        let coid_str = coid_int.to_string();

        // (a) Subscribe to this order's lifecycle, filtered server-side to its
        // client_order_id (plus the pair and the two lifecycle events).
        let mut sub = conn.subscribe_perps_events(
            None,
            Some(vec![
                OrderPersisted::EVENT_NAME.to_string(),
                OrderRemoved::EVENT_NAME.to_string(),
            ]),
            Some(vec![pair_id.to_string()]),
            None,
            None,
            Some(vec![coid_str.clone()]),
        );

        if i == 0 {
            tokio::time::sleep(SETTLE).await;
        }

        // Re-read the index each cycle so the 1%-away price stays valid as the
        // oracle drifts; floor to the tick so the chain accepts it.
        let index = index_price(&client, perps, &pair_id).await?;
        let limit_price = index
            .checked_mul(Dimensionless::new_percent(99))?
            .checked_floor_multiple(tick)?;

        // (b) Place: sign locally, broadcast over the SAME socket.
        let place_tx =
            sign_submit_order(&mut signer, &chain_id, perps, &pair_id, limit_price, coid)?;
        let place_start = Instant::now();
        conn.broadcast(place_tx).await?;
        let place_mempool_ms = ms_since(place_start);

        // (c) On order_persisted, cancel immediately.
        let persisted = await_event(&mut sub, OrderPersisted::EVENT_NAME, &coid_str).await?;
        let place_confirm_ms = duration_ms(place_start, persisted.at);

        let cancel_tx = sign_cancel_order(&mut signer, &chain_id, perps, coid)?;
        let cancel_start = Instant::now();
        conn.broadcast(cancel_tx).await?;
        let cancel_mempool_ms = ms_since(cancel_start);

        // (d) On order_removed, record.
        let removed = await_event(&mut sub, OrderRemoved::EVENT_NAME, &coid_str).await?;
        let cancel_confirm_ms = duration_ms(cancel_start, removed.at);

        place_mempool.push(place_mempool_ms);
        place_confirm.push(place_confirm_ms);
        cancel_mempool.push(cancel_mempool_ms);
        cancel_confirm.push(cancel_confirm_ms);

        let gap = removed.block_height as i64 - persisted.block_height as i64;
        println!(
            "cycle {:>2}/{ITERATIONS}  index={:>12.2}  place[mempool={place_mempool_ms:7.1} confirm={place_confirm_ms:8.1}]  cancel[mempool={cancel_mempool_ms:7.1} confirm={cancel_confirm_ms:8.1}]  (ms)  [+{gap} block(s)]",
            i + 1,
            index.to_f64(),
        );

        // Dropping `sub` here fires an `unsubscribe` for this coid's stream.
        drop(sub);

        tokio::time::sleep(PAUSE_BETWEEN).await;
    }

    print_summary(&[
        ("place_mempool", place_mempool),
        ("place_confirm", place_confirm),
        ("cancel_mempool", cancel_mempool),
        ("cancel_confirm", cancel_confirm),
    ]);

    Ok(())
}

/// Milliseconds elapsed since `start`.
fn ms_since(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Milliseconds between `start` and `end`.
fn duration_ms(start: Instant, end: Instant) -> f64 {
    end.duration_since(start).as_secs_f64() * 1000.0
}
