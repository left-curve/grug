use {
    crate::remote::BlockRangeClient,
    anyhow::{Context, anyhow, bail},
    async_trait::async_trait,
    dango_archive_types::{AnyResult, BlockData},
    futures::{SinkExt, StreamExt, channel::mpsc, stream::BoxStream},
    reqwest::IntoUrl,
    std::time::Duration,
    tokio_tungstenite::{connect_async, tungstenite::Message},
    url::Url,
};

/// Per-request timeout for the REST range calls. The subscription is long-lived
/// and sets none; the fetcher loop additionally wraps each range call in its own
/// timeout.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// App-level keepalive cadence for the `/ws` subscription. The node reaps a
/// socket idle for 60s, so a quiet `fullBlock` feed (no new blocks) is kept
/// alive by sending a `ping` well within that window.
const WS_KEEPALIVE: Duration = Duration::from_secs(20);

/// Cap on establishing the `/ws` subscription — the TCP/WS handshake, the
/// subscribe frame, and the acknowledgement wait combined. Without it a
/// half-open connection (a LB or NAT silently dropping packets) hangs
/// [`HttpdClient::subscribe_full_blocks`] forever — the keepalive loop does not
/// exist yet at that stage — and with it the caller's live tail: the source
/// would freeze without an error for its reconnect loop to react to. With the
/// cap, the hang surfaces as an error and the caller re-subscribes.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Thin client against a dango node's `httpd` — the multiplexed WebSocket `/ws`
/// endpoint for the `fullBlock` subscription, HTTP for the `/block/full/range`
/// backfill endpoint.
///
/// Used directly by both block sources (no trait): there is one node-backed
/// implementation. The local source points it at the in-process `dango-httpd`,
/// the remote source at a sentinel — only the base URL differs. It is the live
/// path (`subscribe_full_blocks`) and, via [`BlockRangeClient`], the backfill
/// path the [`SentinelBlockFetcher`] drives.
///
/// [`SentinelBlockFetcher`]: crate::SentinelBlockFetcher
#[derive(Debug, Clone)]
pub struct HttpdClient {
    inner: reqwest::Client,
    /// `{base}/block/full/range`, joined once at construction.
    range_url: Url,
    /// `{base}/ws` as a `ws`/`wss` URL, joined once at construction.
    ws_url: Url,
}

impl HttpdClient {
    /// Construct from the node's base URL (e.g. `http://localhost:8080`). The
    /// `/ws` and `/block/full/range` paths are joined internally; the WebSocket
    /// scheme is derived from the base (`http`→`ws`, `https`→`wss`).
    pub fn new<U>(base_url: U) -> AnyResult<Self>
    where
        U: IntoUrl,
    {
        let base = base_url.into_url()?;
        let range_url = base.join("block/full/range")?;

        let mut ws_url = base.join("ws")?;
        match ws_url.scheme() {
            "http" => ws_url
                .set_scheme("ws")
                .map_err(|_| anyhow!("failed to set ws scheme"))?,
            "https" => ws_url
                .set_scheme("wss")
                .map_err(|_| anyhow!("failed to set wss scheme"))?,
            "ws" | "wss" => {},
            scheme => bail!("invalid node URL scheme: {scheme}"),
        }

        let inner = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;

        Ok(Self {
            inner,
            range_url,
            ws_url,
        })
    }

    /// Open a WebSocket subscription to the node's `fullBlock` channel and return
    /// a stream of fully-assembled [`BlockData`], **always starting at the live
    /// tip** (only blocks newer than the current tip). Each data frame's `data`
    /// is the node's `{block, outcome}` — a `FullBlock`, which `BlockData` (an
    /// alias of it) deserializes directly.
    ///
    /// There is deliberately **no `since`**. The node serves this feed from a
    /// small in-memory ring, so a `since` below that window fails the
    /// subscription with a `resync` error — which is exactly where the resume
    /// point sits during a backfill or after any non-trivial downtime. Resuming
    /// at the tip and backfilling the gap below it by other means — the remote
    /// source's healer via `/block/full/range`, the local source's on-disk `get`
    /// — is the only reconnect strategy that cannot wedge. See the callers in
    /// `remote::drain_live` and `LocalBlockSource::run`.
    ///
    /// The **shared live path**: both block sources call it — the local one
    /// against the in-process `dango-httpd`, the remote one against a sentinel.
    /// `pub` so the live tail can be exercised in isolation (see the
    /// `live_subscriber` integration test), without a block source in front of
    /// it that could mask a broken subscription behind its REST healer.
    pub async fn subscribe_full_blocks(
        &self,
    ) -> AnyResult<BoxStream<'static, AnyResult<BlockData>>> {
        // The whole establishment is capped by `SUBSCRIBE_TIMEOUT` so a
        // half-open connection surfaces as a retryable error instead of
        // hanging the caller's live tail forever.
        tokio::time::timeout(SUBSCRIBE_TIMEOUT, self.open_full_block_subscription())
            .await
            .map_err(|_| {
                anyhow!(
                    "fullBlock subscribe to {} timed out after {}s",
                    self.ws_url,
                    SUBSCRIBE_TIMEOUT.as_secs()
                )
            })?
    }

    /// The un-timed establishment behind [`subscribe_full_blocks`](Self::subscribe_full_blocks):
    /// connect, subscribe, await the acknowledgement, then hand the socket to a
    /// background task that yields decoded blocks.
    async fn open_full_block_subscription(
        &self,
    ) -> AnyResult<BoxStream<'static, AnyResult<BlockData>>> {
        let (ws, _response) = connect_async(self.ws_url.as_str())
            .await
            .map_err(|err| anyhow!("websocket connection to {} failed: {err}", self.ws_url))?;
        let (mut sink, mut stream) = ws.split();

        // Open the `fullBlock` channel at the live tip (no `since`).
        let subscribe = serde_json::json!({
            "method": "subscribe",
            "id": 1,
            "subscription": { "type": "fullBlock" },
        });
        sink.send(Message::text(subscribe.to_string()))
            .await
            .map_err(|err| anyhow!("failed to send fullBlock subscribe: {err}"))?;

        // Await the acknowledgement (no data frame precedes it); a `resync` /
        // limit error frame becomes a connect-time error.
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(text))) => match classify_frame(&text)? {
                    Frame::Ack => break,
                    Frame::Error { code, message } => {
                        bail!("fullBlock subscribe rejected ({code}): {message}")
                    },
                    Frame::Data(_) | Frame::Other => {},
                },
                Some(Ok(Message::Ping(data))) => {
                    let _ = sink.send(Message::Pong(data)).await;
                },
                Some(Ok(Message::Close(frame))) => {
                    bail!("websocket closed before acknowledgement: {frame:?}")
                },
                Some(Ok(_)) => {},
                Some(Err(err)) => bail!("websocket error before acknowledgement: {err}"),
                None => bail!("websocket closed before acknowledgement"),
            }
        }

        // Drive the socket on a background task: forward decoded blocks, answer
        // control pings, and send an app-level ping on a fixed schedule so an
        // idle feed is not reaped by the server's idle timeout.
        let (tx, rx) = mpsc::unbounded::<AnyResult<BlockData>>();
        tokio::spawn(async move {
            let mut keepalive = tokio::time::interval(WS_KEEPALIVE);
            keepalive.tick().await; // the first tick is immediate; skip it.
            let ping = serde_json::json!({ "method": "ping" }).to_string();

            loop {
                tokio::select! {
                    _ = keepalive.tick() => {
                        if sink.send(Message::text(ping.clone())).await.is_err() {
                            break;
                        }
                    },
                    message = stream.next() => match message {
                        Some(Ok(Message::Text(text))) => match classify_frame(&text) {
                            Ok(Frame::Data(data)) => {
                                let block = serde_json::from_value::<BlockData>(data)
                                    .map_err(|err| anyhow!("failed to decode fullBlock frame: {err}"));
                                if tx.unbounded_send(block).is_err() {
                                    break;
                                }
                            },
                            Ok(Frame::Error { code, message }) => {
                                let _ = tx.unbounded_send(Err(anyhow!(
                                    "fullBlock feed error ({code}): {message}"
                                )));
                                break;
                            },
                            Ok(Frame::Ack | Frame::Other) => {},
                            Err(err) => {
                                let _ = tx.unbounded_send(Err(err));
                                break;
                            },
                        },
                        Some(Ok(Message::Ping(data))) => {
                            if sink.send(Message::Pong(data)).await.is_err() {
                                break;
                            }
                        },
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        Some(Ok(_)) => {},
                    },
                }
            }

            let _ = sink.close().await;
        });

        Ok(Box::pin(rx))
    }
}

/// A classified `/ws` server text frame (discriminated by its `channel` tag).
enum Frame {
    /// `subscriptionResponse` — the subscribe acknowledgement.
    Ack,
    /// A `fullBlock` data frame's `data` payload (the `{block, outcome}` value).
    Data(serde_json::Value),
    /// An error frame — connection-level (the `error` channel) or a
    /// subscription-scoped failure co-located on the `fullBlock` channel (e.g.
    /// `resync`, `tooManyRequests`). Both carry an `error` object.
    Error { code: String, message: String },
    /// `pong` or any other channel — ignored.
    Other,
}

/// Classify a `/ws` server text frame (see the node's `routes::ws`).
///
/// Errors are **co-located on the channel they concern**: a subscription-scoped
/// failure (`resync`, `tooManyRequests`, or the terminal "subscription ended")
/// rides its own channel — `fullBlock` — as an `error`-keyed sibling of its
/// `data`-keyed frames, while a connection-level error uses the dedicated
/// `error` channel. Both carry an `error` object, so its presence is tested
/// first — otherwise the channel match would read an absent `data` off a
/// `fullBlock` error frame and mislabel it.
fn classify_frame(text: &str) -> AnyResult<Frame> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|err| anyhow!("malformed websocket frame: {err}"))?;

    // An `error` object — subscription-scoped (on `fullBlock`) or connection
    // level (on `error`) — is an error frame first, whatever its channel.
    if let Some(error) = value.get("error") {
        let code = error
            .get("code")
            .and_then(|c| c.as_str())
            .unwrap_or("error")
            .to_string();
        let message = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string();
        return Ok(Frame::Error { code, message });
    }

    match value.get("channel").and_then(|c| c.as_str()) {
        Some("subscriptionResponse") => Ok(Frame::Ack),
        Some("fullBlock") => Ok(Frame::Data(
            value
                .get("data")
                .cloned()
                .context("fullBlock frame missing `data`")?,
        )),
        _ => Ok(Frame::Other),
    }
}

#[async_trait]
impl BlockRangeClient for HttpdClient {
    async fn fetch_block_range(&self, from: u64, to: u64) -> AnyResult<Vec<BlockData>> {
        // GET /block/full/range?from=&to=. The query string is built with the
        // `url` crate (this reqwest build has no RequestBuilder::query) and the
        // body decoded with serde_json (no `json` feature). `BlockData` decodes
        // the `{ block, outcome }` items directly.
        //
        // With reqwest's `gzip` feature the call advertises
        // `Accept-Encoding: gzip`, the node's actix `Compress` middleware
        // obliges (~4x smaller for block JSON), and reqwest inflates
        // transparently — `bytes()` below always sees the raw JSON. Asking
        // ourselves also keeps an intermediary from doing it for us: a Go
        // proxy in front of the node (traefik) otherwise injects its own
        // `Accept-Encoding: gzip` and re-inflates the response before
        // forwarding, shipping the payload uncompressed on our leg of the
        // wire.
        let mut url = self.range_url.clone();
        url.query_pairs_mut()
            .append_pair("from", &from.to_string())
            .append_pair("to", &to.to_string());

        let bytes = self
            .inner
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;

        Ok(serde_json::from_slice::<Vec<BlockData>>(&bytes)?)
    }
}

// ---- frame classification ----

#[cfg(test)]
mod tests {
    use super::{Frame, classify_frame};

    #[test]
    fn subscription_response_is_an_ack() {
        assert!(matches!(
            classify_frame(r#"{"channel":"subscriptionResponse","id":1,"type":"fullBlock"}"#)
                .unwrap(),
            Frame::Ack,
        ));
    }

    #[test]
    fn full_block_data_frame_yields_its_payload() {
        let frame =
            classify_frame(r#"{"channel":"fullBlock","id":1,"data":{"block":{},"outcome":{}}}"#)
                .unwrap();
        match frame {
            Frame::Data(data) => {
                assert!(
                    data.get("block").is_some(),
                    "the `data` payload is forwarded"
                );
            },
            _ => panic!("expected a data frame"),
        }
    }

    #[test]
    fn subscription_scoped_error_rides_the_full_block_channel() {
        // The regression this guards: since main co-located errors, a `resync`
        // on the `fullBlock` channel carries an `error` (not `data`) key. It must
        // classify as an error, not fall through the channel match as missing
        // data.
        let frame = classify_frame(
            r#"{"channel":"fullBlock","id":1,"error":{"code":"resync","message":"stale"}}"#,
        )
        .unwrap();
        match frame {
            Frame::Error { code, message } => {
                assert_eq!(code, "resync");
                assert_eq!(message, "stale");
            },
            _ => panic!("expected an error frame"),
        }
    }

    #[test]
    fn connection_level_error_is_read_from_the_error_key() {
        let frame = classify_frame(
            r#"{"channel":"error","id":3,"error":{"code":"tooManyRequests","message":"slow down"}}"#,
        )
        .unwrap();
        match frame {
            Frame::Error { code, message } => {
                assert_eq!(code, "tooManyRequests");
                assert_eq!(message, "slow down");
            },
            _ => panic!("expected an error frame"),
        }
    }

    #[test]
    fn pong_and_unknown_channels_are_ignored() {
        assert!(matches!(
            classify_frame(r#"{"channel":"pong","id":1}"#).unwrap(),
            Frame::Other,
        ));
    }
}
