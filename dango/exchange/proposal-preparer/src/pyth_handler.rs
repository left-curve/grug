use {
    dango_primitives::{
        Addr, Coins, Json, JsonSerExt, Lengthy, Message, NonEmpty, QuerierExt, QuerierWrapper,
        Shared, StdError, StdResult, Tx,
    },
    dango_pyth_client::{PythClient, PythClientCache, PythClientTrait},
    dango_pyth_types::{PriceUpdate, PythLazerSubscriptionDetails},
    dango_types::{
        config::AppConfig,
        oracle::{ExecuteMsg, QueryPriceSourcesRequest},
        perps,
    },
    prost::bytes::Bytes,
    reqwest::IntoUrl,
    std::{
        fmt::Debug,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::Duration,
    },
    tokio::{runtime::Runtime, time::sleep},
    tokio_stream::StreamExt,
    tracing::{error, warn},
};
#[cfg(feature = "metrics")]
use {
    metrics::{describe_histogram, histogram},
    std::time::Instant,
};

/// Number of attempts to connect to the Pyth stream before giving up.
const CONNECT_ATTEMPTS: usize = 3;

/// Gas limit for the oracle price-feed transaction injected at the top of
/// each block.
const GAS_LIMIT: u64 = 50_000_000;

/// Handler for the PythClient to be used in the ProposalPreparer, used to
/// keep all code related to Pyth for PP in a single structure.
pub struct PythHandler<P>
where
    P: PythClientTrait,
{
    /// `None` when Pyth feeding is disabled (no token / no endpoints).
    inner: Option<Mutex<PythHandlerInner<P>>>,
}

struct PythHandlerInner<P>
where
    P: PythClientTrait,
{
    client: P,
    shared_vaas: Shared<Option<PriceUpdate>>,
    current_ids: Vec<PythLazerSubscriptionDetails>,
    stoppable_thread: Option<(Arc<AtomicBool>, thread::JoinHandle<()>)>,
}

impl PythHandler<PythClient> {
    pub fn new<V, U, T>(endpoints: V, access_token: T) -> Self
    where
        V: IntoIterator<Item = U> + Lengthy,
        U: IntoUrl,
        T: ToString,
    {
        #[cfg(feature = "metrics")]
        init_metrics();

        if access_token.to_string().is_empty() {
            warn!("Pyth Lazer access token is empty! Oracle feeding is disabled");
            return Self { inner: None };
        }

        if endpoints.length() == 0 {
            warn!("Pyth Lazer endpoints not provided! Oracle feeding is disabled");
            return Self { inner: None };
        }

        // Infallible: `endpoints.length() == 0` ruled out above.
        let endpoints = NonEmpty::new(endpoints).unwrap();

        // Fail loudly on construction error. Silently producing a disabled
        // handler would mean every block runs without fresh oracle prices,
        // the market-making vault never re-quotes, and liquidation logic
        // uses stale inputs — far worse than a noisy startup failure.
        let client = PythClient::new(endpoints, access_token).unwrap();

        Self {
            inner: Some(Mutex::new(PythHandlerInner::new(client))),
        }
    }
}

impl PythHandler<PythClientCache> {
    pub fn new_with_cache<V, U, T>(endpoints: NonEmpty<V>, access_token: T) -> Self
    where
        V: IntoIterator<Item = U> + Lengthy,
        U: IntoUrl,
        T: ToString,
    {
        #[cfg(feature = "metrics")]
        init_metrics();

        // Fail loudly on construction error — same rationale as
        // `PythHandler::<PythClient>::new`.
        let client = PythClientCache::new(endpoints, access_token).unwrap();

        Self {
            inner: Some(Mutex::new(PythHandlerInner::new(client))),
        }
    }
}

impl<P> PythHandler<P>
where
    P: PythClientTrait + QueryPythId,
{
    pub fn fetch_latest_price_update(&self) -> Option<PriceUpdate> {
        // Retrieve the VAAs from the shared memory and consume them in order
        // to avoid pushing the same VAAs again.
        //
        // Panic on a poisoned mutex: it means a prior caller panicked while
        // holding the lock — that's a real bug, and silently swallowing it
        // would let the chain produce many blocks without fresh oracle
        // prices.
        let inner = self.inner.as_ref()?.lock().unwrap();
        inner.shared_vaas.replace(None)
    }

    pub fn close_stream(&self) {
        if let Some(inner) = &self.inner {
            // Panic on a poisoned mutex — same rationale as
            // `fetch_latest_price_update`.
            inner.lock().unwrap().close_stream();
        }
    }
}

impl<P> PythHandler<P>
where
    P: PythClientTrait + QueryPythId + Send + 'static,
    P::Error: Debug,
{
    pub fn update_stream(&self, querier: QuerierWrapper, oracle: Addr) -> StdResult<()> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };

        // Panic on a poisoned mutex — same rationale as
        // `fetch_latest_price_update`.
        inner.lock().unwrap().update_stream(querier, oracle)
    }
}

impl<P> Clone for PythHandler<P>
where
    P: PythClientTrait,
{
    /// Cloning produces a "dud" copy with no inner state — the streaming
    /// thread is never duplicated.
    fn clone(&self) -> Self {
        Self { inner: None }
    }
}

impl<P> Drop for PythHandler<P>
where
    P: PythClientTrait,
{
    fn drop(&mut self) {
        if let Some(inner) = &self.inner
            && let Ok(mut inner) = inner.lock()
        {
            inner.close_stream();
        }
    }
}

impl<P> dango_app::ProposalPreparer for PythHandler<P>
where
    P: PythClientTrait + QueryPythId + Send + 'static,
    P::Error: Debug,
{
    type Error = StdError;

    fn prepare_proposal(
        &self,
        querier: QuerierWrapper,
        mut txs: Vec<Bytes>,
        _max_tx_bytes: usize,
    ) -> Result<Vec<Bytes>, Self::Error> {
        #[cfg(feature = "metrics")]
        let start = Instant::now();

        // Disabled handler — pass through.
        if self.inner.is_none() {
            return Ok(txs);
        }

        let cfg: AppConfig = querier.query_app_config()?;

        // Update the Pyth stream if the PythIds in the oracle have changed.
        if let Err(err) = self.update_stream(querier, cfg.addresses.oracle) {
            error!("Failed to update Pyth stream: {:?}", err);
        }

        // Retrieve the PriceUpdate.
        let Some(price_update) = self.fetch_latest_price_update() else {
            return Ok(txs);
        };

        // Build the oracle price-feed tx and insert it at the front of the block.
        {
            let oracle_tx = Tx {
                sender: cfg.addresses.oracle,
                gas_limit: GAS_LIMIT,
                msgs: NonEmpty::new_unchecked(vec![Message::execute(
                    cfg.addresses.oracle,
                    &ExecuteMsg::FeedPrices(price_update),
                    Coins::new(),
                )?]),
                data: Json::null(),
                credential: Json::null(),
            };

            txs.insert(0, oracle_tx.to_json_vec()?.into());
        }

        // Once the exchange is wound down there are no positions to mark and
        // no vault margin to quote with, so stop injecting the two perps
        // maintenance transactions. The oracle price feed above keeps running,
        // since other contracts still consume it.
        let trading_enabled = querier
            .query_wasm_smart(cfg.addresses.perps, perps::QueryParamRequest {})?
            .trading_enabled;

        if !trading_enabled {
            #[cfg(feature = "metrics")]
            histogram!("proposal_preparer.prepare_proposal.duration",)
                .record(start.elapsed().as_secs_f64());

            return Ok(txs);
        }

        // Insert perps index-price refresh transaction.
        {
            let index_tx = Tx {
                sender: cfg.addresses.perps,
                gas_limit: GAS_LIMIT,
                msgs: NonEmpty::new_unchecked(vec![Message::execute(
                    cfg.addresses.perps,
                    &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshIndexPrices {}),
                    Coins::new(),
                )?]),
                data: Json::null(),
                credential: Json::null(),
            };

            txs.insert(1, index_tx.to_json_vec()?.into());
        }

        // Insert perps vault-order refresh transaction.
        {
            let vault_tx = Tx {
                sender: cfg.addresses.perps,
                gas_limit: GAS_LIMIT,
                msgs: NonEmpty::new_unchecked(vec![Message::execute(
                    cfg.addresses.perps,
                    &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshVaultOrders {}),
                    Coins::new(),
                )?]),
                data: Json::null(),
                credential: Json::null(),
            };

            txs.insert(2, vault_tx.to_json_vec()?.into());
        }

        #[cfg(feature = "metrics")]
        histogram!("proposal_preparer.prepare_proposal.duration",)
            .record(start.elapsed().as_secs_f64());

        Ok(txs)
    }
}

impl<P> PythHandlerInner<P>
where
    P: PythClientTrait,
{
    fn new(client: P) -> Self {
        Self {
            client,
            shared_vaas: Shared::new(None),
            current_ids: vec![],
            stoppable_thread: None,
        }
    }

    fn close_stream(&mut self) {
        if let Some((keep_running, _handle)) = self.stoppable_thread.take() {
            keep_running.store(false, Ordering::SeqCst);
        }

        // Closing any potentially connected earlier stream.
        self.client.close();
    }
}

impl<P> PythHandlerInner<P>
where
    P: PythClientTrait + QueryPythId + Send + 'static,
    P::Error: Debug,
{
    fn connect_stream<I>(&mut self, ids: NonEmpty<I>)
    where
        I: IntoIterator<Item = PythLazerSubscriptionDetails> + Lengthy + Send + Clone + 'static,
    {
        self.close_stream();

        let shared_data = self.shared_vaas.clone();
        let keep_running = Arc::new(AtomicBool::new(true));
        let mut client = self.client.clone();

        self.stoppable_thread = Some((
            keep_running.clone(),
            thread::spawn(move || {
                let rt = match Runtime::new() {
                    Ok(rt) => rt,
                    Err(err) => {
                        error!(error = err.to_string(), "Failed to create Tokio runtime");
                        keep_running.store(false, Ordering::SeqCst);
                        return;
                    },
                };

                rt.block_on(async {
                    let mut attempts = 0;

                    // Try to create the stream, retrying up to CONNECT_ATTEMPTS times if it fails.
                    let mut stream = loop {
                        match client.stream(ids.clone()).await {
                            Ok(stream) => {
                                break stream;
                            },
                            Err(err) => {
                                attempts += 1;

                                if attempts < CONNECT_ATTEMPTS {
                                    error!(error = err.to_string(), "Failed to create Pyth stream; attempts: {attempts}");
                                    sleep(Duration::from_millis(100)).await;
                                } else {
                                    error!("Failed to create Pyth stream after {attempts} attempts, stop retrying");
                                    keep_running.store(false, Ordering::SeqCst);
                                    return;
                                }
                            },
                        };
                    };

                    loop {
                        tokio::select! {
                            _ = tokio::time::sleep(tokio::time::Duration::from_millis(500)) => {
                                if !keep_running.load(Ordering::Relaxed) {
                                    return;
                                }
                            }
                            data = stream.next() => {
                                if !keep_running.load(Ordering::Acquire) {
                                    return;
                                }

                                if let Some(data) = data {
                                    shared_data.write_with(|mut shared_vaas| *shared_vaas = Some(data));
                                }
                            }
                        }
                    }
                });
            }),
        ));
    }

    fn update_stream(&mut self, querier: QuerierWrapper, oracle: Addr) -> StdResult<()> {
        // Retrieve the Pyth ids from the Oracle contract.
        let pyth_ids = self.client.pyth_ids(querier, oracle)?;

        // Load the state of streaming.
        let is_stream_running = if let Some((keep_running, _)) = self.stoppable_thread.as_ref() {
            keep_running.load(Ordering::Acquire)
        } else {
            false
        };

        // If stream is closed and there are no PythIds, we can return early.
        if !is_stream_running && pyth_ids.is_empty() {
            return Ok(());
        }

        // If the stream is running and the PythIds are the same, we can return early.
        if is_stream_running && self.current_ids == pyth_ids {
            return Ok(());
        }

        // The PythIds have changed or the streaming is closed unexpectedly.
        self.current_ids = pyth_ids.clone();

        self.close_stream();

        if let Ok(pyth_ids) = NonEmpty::new(pyth_ids) {
            self.connect_stream(pyth_ids);
        }

        Ok(())
    }
}

pub trait QueryPythId: PythClientTrait {
    fn pyth_ids(
        &self,
        querier: QuerierWrapper,
        oracle: Addr,
    ) -> StdResult<Vec<PythLazerSubscriptionDetails>>;
}

impl QueryPythId for PythClient {
    /// Retrieve the Pyth ids from the Oracle contract.
    fn pyth_ids(
        &self,
        querier: QuerierWrapper,
        oracle: Addr,
    ) -> StdResult<Vec<PythLazerSubscriptionDetails>> {
        pyth_ids_lazer(querier, oracle)
    }
}

impl QueryPythId for PythClientCache {
    fn pyth_ids(
        &self,
        querier: QuerierWrapper,
        oracle: Addr,
    ) -> StdResult<Vec<PythLazerSubscriptionDetails>> {
        pyth_ids_lazer(querier, oracle)
    }
}

fn pyth_ids_lazer(
    querier: QuerierWrapper,
    oracle: Addr,
) -> StdResult<Vec<PythLazerSubscriptionDetails>> {
    let new_ids = querier
        .query_wasm_smart(oracle, QueryPriceSourcesRequest {
            start_after: None,
            limit: Some(u32::MAX),
        })?
        .into_values()
        // Each denom maps to a price config; flatten to the feeds the streamer
        // must subscribe to (one for a single source, two for a futures roll —
        // current and next).
        .flat_map(|config| config.feeds())
        .collect();

    Ok(new_ids)
}

#[cfg(feature = "metrics")]
pub fn init_metrics() {
    describe_histogram!(
        "proposal_preparer.prepare_proposal.duration",
        "Duration of the `prepare_proposal` method in seconds",
    );
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_app::{NaiveQuerier, ProposalPreparer as _},
    };

    /// Disabled handler must short-circuit `prepare_proposal` before touching
    /// the querier — passing `NaiveQuerier` (which panics on any query)
    /// proves the early-return path runs.
    #[test]
    fn disabled_handler_when_token_empty_passes_through() {
        let handler: PythHandler<PythClient> = PythHandler::new(["http://example.com"], "");
        assert!(handler.inner.is_none());

        let querier = QuerierWrapper::new(&NaiveQuerier);
        let txs: Vec<Bytes> = vec![Bytes::from_static(b"tx1"), Bytes::from_static(b"tx2")];
        let out = handler
            .prepare_proposal(querier, txs.clone(), usize::MAX)
            .unwrap();
        assert_eq!(out, txs);
    }

    #[test]
    fn disabled_handler_when_endpoints_empty_passes_through() {
        let handler: PythHandler<PythClient> = PythHandler::new(Vec::<&str>::new(), "lazer_token");
        assert!(handler.inner.is_none());

        let querier = QuerierWrapper::new(&NaiveQuerier);
        let txs: Vec<Bytes> = vec![Bytes::from_static(b"tx1")];
        let out = handler
            .prepare_proposal(querier, txs.clone(), usize::MAX)
            .unwrap();
        assert_eq!(out, txs);
    }
}
