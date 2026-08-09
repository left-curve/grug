#[cfg(feature = "metrics")]
use metrics::{counter, describe_counter, describe_histogram, histogram};
use {
    crate::PythClientTrait,
    anyhow::bail,
    async_stream::stream,
    dango_primitives::{Inner, Lengthy, NonEmpty},
    dango_pyth_types::{ExponentialBackoff, PriceUpdate, PythLazerSubscriptionDetails},
    pyth_lazer_client::{
        stream_client::{PythLazerStreamClient, PythLazerStreamClientBuilder},
        ws_connection::AnyResponse,
    },
    pyth_lazer_protocol::{
        PriceFeedId, PriceFeedProperty,
        api::{
            Channel, DeliveryFormat, Format, JsonBinaryEncoding, SubscribeRequest, SubscriptionId,
            SubscriptionParams, SubscriptionParamsRepr, WsResponse,
        },
        message::Message,
    },
    reqwest::IntoUrl,
    std::{
        collections::{HashMap, HashSet},
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    },
    tokio::{sync::mpsc::Receiver, time::sleep},
    tracing::{debug, error, info, warn},
    url::Url,
};

pub const RESUBSCRIBE_ATTEMPTS: u32 = 5;

/// Timeout in milliseconds that we wait for receiving data before trying to resubscribe.
pub const DATA_RECEIVE_TIMEOUT_MS: u64 = 200;
/// Timeout in milliseconds for subscription requests.
pub const SUBSCRIPTION_TIMEOUT_MS: u64 = 1000;

#[derive(Clone, Debug)]
pub struct PythClient {
    endpoints: Vec<Url>,
    access_token: String,
    keep_running: Arc<AtomicBool>,
    last_subscription_id: u64,
}

impl PythClient {
    pub fn new<V, U, T>(endpoints: NonEmpty<V>, access_token: T) -> Result<Self, anyhow::Error>
    where
        V: IntoIterator<Item = U> + Lengthy,
        U: IntoUrl,
        T: ToString,
    {
        #[cfg(feature = "metrics")]
        init_metrics();

        Ok(PythClient {
            endpoints: endpoints
                .into_inner()
                .into_iter()
                .map(|url| url.into_url())
                .collect::<Result<Vec<_>, _>>()?,
            access_token: access_token.to_string(),
            keep_running: Arc::new(AtomicBool::new(false)),
            last_subscription_id: 0,
        })
    }

    /// Create subscription parameters for the given price feed IDs.
    fn subscription_params(
        price_feed_ids: Vec<PriceFeedId>,
        channel: Channel,
    ) -> Result<SubscriptionParams, anyhow::Error> {
        SubscriptionParams::new(SubscriptionParamsRepr {
            price_feed_ids: Some(price_feed_ids),
            symbols: None,
            properties: vec![
                PriceFeedProperty::Price,
                PriceFeedProperty::Exponent,
                PriceFeedProperty::MarketSession,
            ],
            formats: vec![Format::LeEcdsa],
            delivery_format: DeliveryFormat::Binary,
            json_binary_encoding: JsonBinaryEncoding::Base64,
            parsed: false,
            channel,
            ignore_invalid_feeds: true,
        })
        .map_err(|e| anyhow::anyhow!(e))
    }

    // Subscribe to the price feeds.
    // The subscribe function return error if at least one subscription fails.
    // Ignore the error here and analyze the data received from the stream: in case
    // we don't receive any data for a subscription, we will resubscribe.
    async fn connect(
        client: &mut PythLazerStreamClient,
        subscribe_requests: Vec<SubscribeRequest>,
    ) {
        #[cfg(feature = "metrics")]
        counter!(dango_pyth_types::metrics::PYTH_RECONNECTION_ATTEMPTS).increment(1);

        info!(
            subscribe_requests = subscribe_requests.len(),
            "Sending subscription requests",
        );

        for subscription in subscribe_requests {
            match tokio::time::timeout(
                Duration::from_millis(SUBSCRIPTION_TIMEOUT_MS),
                client.subscribe(subscription.clone()),
            )
            .await
            {
                Ok(Ok(_)) => {
                    info!(
                        subscription_id = subscription.subscription_id.0,
                        "Subscription request succeeded"
                    );
                },
                Ok(Err(err)) => {
                    warn!(
                        subscription_id = subscription.subscription_id.0,
                        error = %err,
                        "Subscription request failed"
                    );
                },
                Err(_) => {
                    warn!(
                        subscription_id = subscription.subscription_id.0,
                        timeout_ms = SUBSCRIPTION_TIMEOUT_MS,
                        "Sending subscription request timed out"
                    );
                },
            }
        }
    }

    /// Subscribe to the price feeds and ensure that we are receiving data for all subscriptions.
    async fn subscribe(
        client: &mut PythLazerStreamClient,
        receiver: &mut Receiver<AnyResponse>,
        ids_per_channel: HashMap<Channel, (u64, Vec<PriceFeedId>)>,
    ) -> Result<(), anyhow::Error> {
        // Collect all subscription IDs in a vector to easily check if we are receiving data for all subscriptions.
        let mut subscription_ids = Vec::with_capacity(ids_per_channel.len());

        // Create the subscribe requests.
        let subscribe_requests = ids_per_channel
            .into_iter()
            .map(|(channel, (subscription_id, feed_ids))| {
                info!(
                    subscription_id,
                    channel=%channel,
                    feed_ids=?feed_ids,
                    "Subscription sent to Pyth Lazer",
                );

                let params = Self::subscription_params(feed_ids, channel)?;

                subscription_ids.push(subscription_id);

                Ok(SubscribeRequest {
                    subscription_id: SubscriptionId(subscription_id),
                    params,
                })
            })
            .collect::<Result<Vec<_>, anyhow::Error>>()?;

        // Subscribe to the price feeds.
        Self::connect(client, subscribe_requests.clone()).await;

        // Ensure that we are receiving data for all subscriptions.
        let mut data_per_ids = HashMap::new();
        for id in subscription_ids.clone() {
            data_per_ids.insert(id, false);
        }

        let mut to_resubscribe = false;
        let mut last_check = Instant::now();

        let buffer_capacity = 1000;
        let mut buffer = Vec::with_capacity(buffer_capacity);
        let timeout = Duration::from_secs(3);

        let mut backoff = ExponentialBackoff::new(
            Duration::from_millis(100),
            Duration::from_secs(2),
            2,
            Some(RESUBSCRIBE_ATTEMPTS),
        );

        loop {
            // If after few seconds we haven't received data for all subscriptions,
            // try to reconnect.
            if last_check.elapsed() > Duration::from_secs(2) {
                warn!("Not all subscriptions received data");
                to_resubscribe = true;
            }

            // Check if we need to resubscribe.
            if to_resubscribe {
                to_resubscribe = false;

                let maybe_next_delay = backoff.next_delay();

                // If we have reached the maximum number of resubscription attempts, return an error.
                let Some(next_delay) = maybe_next_delay else {
                    bail!("failed to connect to Pyth Lazer after {RESUBSCRIBE_ATTEMPTS} attempts");
                };

                // Reset all received data to false for each subscription ID, just to be sure that
                // everything works fine.
                for value in data_per_ids.values_mut() {
                    *value = false;
                }

                warn!(
                    "Attempting to resubscribe... (attempt {}/{})",
                    backoff.attempts(),
                    RESUBSCRIBE_ATTEMPTS
                );

                sleep(next_delay).await;
                Self::connect(client, subscribe_requests.clone()).await;

                last_check = Instant::now();
            }

            // Retrieve the data.
            buffer.clear();

            // Retrieve the data. If no data is received, try to resubscribe.
            let Some(data_count) =
                Self::retrieve_data(receiver, &mut buffer, buffer_capacity, timeout).await
            else {
                to_resubscribe = true;
                continue;
            };

            // If the number of data received is zero, it means the channel is closed and we need to resubscribe.
            if data_count == 0 {
                error!("Pyth Lazer connection closed");
                to_resubscribe = true;
                continue;
            }

            debug!("Received {} messages from Pyth Lazer", data_count);

            for data in buffer.drain(..) {
                match data {
                    AnyResponse::Binary(update) => {
                        // We have received data for this subscription ID.
                        if let Some(entry) = data_per_ids.get_mut(&update.subscription_id.0) {
                            *entry = true;

                            // Check if we have received data for all subscription IDs.
                            if data_per_ids.values().all(|&v| v) {
                                info!("Received data from all price feeds");
                                return Ok(());
                            }
                        }
                    },
                    AnyResponse::Json(WsResponse::Error(error_response)) => {
                        error!("Subscription failed: {}", error_response.error);

                        // In this error there is no information about which subscription failed.
                        // So we will try to resubscribe to all subscriptions.
                        // If a connection is already established, the server will send a
                        // SubscriptionError response.
                        to_resubscribe = true;
                    },
                    AnyResponse::Json(WsResponse::SubscriptionError(
                        subscription_error_response,
                    )) => {
                        // Ignore duplicate subscription ID errors.
                        if subscription_error_response.error != "duplicate subscription id" {
                            error!(
                                "Subscription error for id {}: {}",
                                subscription_error_response.subscription_id.0,
                                subscription_error_response.error
                            );
                        }
                    },
                    AnyResponse::Json(WsResponse::Subscribed(subscription_response)) => {
                        info!(
                            subscription_id = subscription_response.subscription_id.0,
                            "Subscription confirmed",
                        );
                    },
                    AnyResponse::Json(WsResponse::SubscribedWithInvalidFeedIdsIgnored(
                        subscription_response,
                    )) => {
                        // Log a warning if some feed ids were ignored
                        // (this means the Id or the combination Id - channel is not supported).
                        if subscription_response
                            .ignored_invalid_feed_ids
                            .unknown_ids
                            .is_empty()
                        {
                            info!(
                                subscription_id = subscription_response.subscription_id.0,
                                "Subscription confirmed",
                            );
                        } else {
                            warn!(
                                subscription_id = subscription_response.subscription_id.0,
                                ignored_feeds = ?subscription_response.ignored_invalid_feed_ids,
                                "Subscription confirmed, but some feed ids were ignored",
                            );
                        }
                    },
                    AnyResponse::Json(WsResponse::Unsubscribed(unsubscribed_response)) => {
                        // If this is a response from a previous connection, it can be ignored.
                        if subscription_ids.contains(&unsubscribed_response.subscription_id.0) {
                            error!(
                                subscription_id = unsubscribed_response.subscription_id.0,
                                "Received unsubscribe response during connection",
                            );
                        }
                    },
                    AnyResponse::Json(WsResponse::StreamUpdated(_)) => {
                        error!("Received Lazer data in json format, only support Binary");
                    },
                }
            }
        }
    }

    /// Analyze the data received from the Pyth Lazer stream.
    fn analyze_data(
        received_data: &mut Vec<AnyResponse>,
        subscription_ids: &Vec<u64>,
        subscriptions_data: &mut HashMap<u64, dango_pyth_types::LeEcdsaMessage>,
        last_data_received: &mut HashMap<u64, Instant>,
    ) {
        for data in received_data.drain(..) {
            match data {
                AnyResponse::Binary(update) => {
                    // Check the update is the current subscription ID.
                    if !subscription_ids.contains(&update.subscription_id.0) {
                        warn!(
                            "Received update for a different subscription ID: {}. Expected: {:?}",
                            update.subscription_id.0, subscription_ids
                        );
                    } else {
                        // Update the last time we received data for this subscription ID.
                        last_data_received.insert(update.subscription_id.0, Instant::now());
                    }

                    #[cfg(feature = "metrics")]
                    histogram!(dango_pyth_types::metrics::PYTH_MESSAGES_RECEIVED)
                        .record(update.messages.len() as f64);

                    // Analyze the messages received.
                    for msg in update.messages {
                        match msg {
                            Message::LeEcdsa(le_ecdsa_message) => {
                                // Since there are multiple subscriptions, we need to keep the last data for each subscription.
                                subscriptions_data
                                    .insert(update.subscription_id.0, le_ecdsa_message.into());
                            },
                            _ => error!("Received non-ECDSA message: {msg:#?}"),
                        }
                    }
                },
                AnyResponse::Json(WsResponse::Error(error_response)) => {
                    error!("Received error: {error_response:#?}");
                },
                AnyResponse::Json(WsResponse::StreamUpdated(_)) => {
                    error!("Received Lazer data in json format, only support Binary");
                },
                AnyResponse::Json(response) => {
                    warn!("Received json response: {response:#?}");
                },
            }
        }
    }

    /// Retrieve data from the Pyth Lazer client.
    /// If no data is received within the timeout, log a warning and return None.
    /// If data is received, return the number of data received.
    async fn retrieve_data(
        receiver: &mut Receiver<AnyResponse>,
        buffer: &mut Vec<AnyResponse>,
        buffer_capacity: usize,
        timeout: Duration,
    ) -> Option<usize> {
        match tokio::time::timeout(timeout, receiver.recv_many(buffer, buffer_capacity)).await {
            Ok(data_count) => Some(data_count),
            Err(_) => {
                warn!(
                    "No new data received for {} milliseconds",
                    timeout.as_millis()
                );

                None
            },
        }
    }

    /// Group subscription details by channel, deduplicating `PriceFeedId` values
    /// within each channel. Returns the grouped map and the list of subscription IDs.
    fn group_ids_by_channel(
        &mut self,
        ids: impl IntoIterator<Item = PythLazerSubscriptionDetails>,
    ) -> (HashMap<Channel, (u64, Vec<PriceFeedId>)>, Vec<u64>) {
        // Use an HashSet in order to dedupe the PriceFeedId, otherwise the Pyth will throw an error.
        let mut ids_per_channel: HashMap<Channel, (u64, HashSet<PriceFeedId>)> = HashMap::new();
        let mut subscription_ids = vec![];

        for value in ids {
            let channel = value.channel;

            match ids_per_channel.get_mut(&channel) {
                Some((_, price_feed)) => {
                    price_feed.insert(PriceFeedId(value.id));
                },
                None => {
                    self.last_subscription_id += 1;

                    subscription_ids.push(self.last_subscription_id);

                    ids_per_channel.insert(
                        channel,
                        (
                            self.last_subscription_id,
                            HashSet::from([PriceFeedId(value.id)]),
                        ),
                    );
                },
            }
        }

        // Return the ids as a `Vec<PriceFeedId>` since in `stream()` function, each time we subscribe
        // we need them as a Vec.
        let ids_per_channel = ids_per_channel
            .into_iter()
            .map(|(channel, (sub_id, feed_ids))| {
                (channel, (sub_id, feed_ids.into_iter().collect()))
            })
            .collect();

        (ids_per_channel, subscription_ids)
    }
}

#[async_trait::async_trait]
impl PythClientTrait for PythClient {
    type Error = anyhow::Error;

    async fn stream<I>(
        &mut self,
        ids: NonEmpty<I>,
    ) -> Result<Pin<Box<dyn tokio_stream::Stream<Item = PriceUpdate> + Send>>, Self::Error>
    where
        I: IntoIterator<Item = PythLazerSubscriptionDetails> + Lengthy + Send + Clone,
    {
        // Close the previous connection.
        self.close();

        self.keep_running = Arc::new(AtomicBool::new(true));
        let keep_running = self.keep_running.clone();

        // Divide the ids depending on the channel, deduplicating within each channel.
        let (ids_per_channel, subscription_ids) = self.group_ids_by_channel(ids.into_inner());

        // Build the new client and subscribe to the price feeds.
        let builder = PythLazerStreamClientBuilder::new(self.access_token.clone())
            .with_endpoints(self.endpoints.clone())
            .with_timeout(Duration::from_secs(2));

        let mut client = builder.build()?;
        let mut receiver = client.start().await?;

        Self::subscribe(&mut client, &mut receiver, ids_per_channel.clone()).await?;

        // Since there are multiple subscriptions, we need to keep the last data for each subscriptions.
        let mut subscriptions_data = HashMap::with_capacity(ids_per_channel.len());

        // Create the buffer to pull data from the receiver.
        let buffer_capacity = 1000;
        let mut buffer = Vec::with_capacity(buffer_capacity);
        let timeout = Duration::from_millis(DATA_RECEIVE_TIMEOUT_MS);

        // Flag to indicate if we need to resubscribe.
        let mut to_resubscribe = false;

        // Keep track of the last time we received data for each subscription ID.
        let mut last_data_received = subscription_ids
            .iter()
            .map(|id| (*id, Instant::now()))
            .collect::<HashMap<_, _>>();

        // Keep track of how long the stream works without issues.
        let mut start_uptime = Instant::now();

        // Create the stream.
        let stream = stream! {
            loop {
                // Check if the streaming has to be closed.
                if !keep_running.load(Ordering::Acquire) {
                    info!("Pyth Lazer connection closed");
                    break;
                }

                // Check if we need to resubscribe.
                if to_resubscribe {
                    to_resubscribe = false;

                    let uptime = start_uptime.elapsed();

                    #[cfg(feature = "metrics")]
                    histogram!(dango_pyth_types::metrics::PYTH_UPTIME).record(uptime.as_secs_f64());

                    info!(
                        uptime_s = uptime.as_secs(),
                        "Resubscribing to Pyth Lazer..."
                    );

                    let start_reconnection = Instant::now();
                    let mut backoff = ExponentialBackoff::new(
                        Duration::from_millis(100),
                        Duration::from_secs(5),
                        2,
                        None,
                    );

                    loop {
                        match Self::subscribe(&mut client, &mut receiver, ids_per_channel.clone())
                            .await
                        {
                            Ok(()) => {
                                let reconnection_time = start_reconnection.elapsed();
                                info!(
                                    reconnection_time_ms = reconnection_time.as_millis(),
                                    "Resubscribed successfully",
                                );

                                #[cfg(feature = "metrics")]
                                histogram!(dango_pyth_types::metrics::PYTH_RECONNECTION_TIME)
                                    .record(reconnection_time.as_secs_f64());

                                start_uptime = Instant::now();

                                // Reset the last data received time for each subscription ID.
                                for id in subscription_ids.iter() {
                                    last_data_received.insert(*id, Instant::now());
                                }

                                break;
                            },
                            Err(err) => {
                                // Check if the streaming has to be closed.
                                if !keep_running.load(Ordering::Acquire) {
                                    break;
                                }

                                // If the subscription fails, wait for a while and try again.
                                let next_delay =
                                    backoff.next_delay().unwrap_or(Duration::from_secs(1));

                                error!(
                                    error=%err,
                                    retry_ms=next_delay.as_millis(),
                                    "Failed to reconnect",
                                );
                                sleep(next_delay).await;

                                continue;
                            },
                        }
                    }
                }

                // Retrieve the data. If no data is received, try to resubscribe.
                buffer.clear();

                let Some(data_count) =
                    Self::retrieve_data(&mut receiver, &mut buffer, buffer_capacity, timeout).await
                else {
                    to_resubscribe = true;
                    continue;
                };

                // Connection closed, try to reconnect.
                if data_count == 0 {
                    error!("Pyth Lazer connection closed");
                    to_resubscribe = true;
                    continue;
                };

                #[cfg(feature = "metrics")]
                counter!(dango_pyth_types::metrics::PYTH_DATA_READ).increment(1);

                // Analyze the data received.
                Self::analyze_data(
                    &mut buffer,
                    &subscription_ids,
                    &mut subscriptions_data,
                    &mut last_data_received,
                );

                // Check if we haven't received data for some subscriptions for more than 3 seconds.
                for (id, last_update) in last_data_received.iter() {
                    if last_update.elapsed() > Duration::from_secs(3) {
                        to_resubscribe = true;
                        warn!("No data received for subscription ID {id} for more than 3 seconds");
                    }
                }

                // Yield the current data.
                if !subscriptions_data.is_empty() {
                    let send_data = subscriptions_data.clone().into_values().collect::<Vec<_>>();
                    yield NonEmpty::new_unchecked(send_data);
                }
            }

            // If the code reaches here, it means the stream needs to be closed.
            for subscription_id in subscription_ids {
                match tokio::time::timeout(
                    Duration::from_millis(SUBSCRIPTION_TIMEOUT_MS),
                    client.unsubscribe(SubscriptionId(subscription_id)),
                )
                .await
                {
                    Ok(Ok(_)) =>  {
                        info!(subscription_id, "Unsubscribed stream successfully");
                    },
                    Ok(Err(err)) => {
                        warn!(
                            subscription_id,
                            error = %err,
                            "Failed to unsubscribe stream"
                        );
                    },
                    Err(_) => {
                        warn!(
                            subscription_id,
                            timeout_ms = SUBSCRIPTION_TIMEOUT_MS,
                            "Sending unsubscription request timed out"
                        );
                    },
                };
            }
        };

        Ok(Box::pin(stream))
    }

    fn close(&mut self) {
        self.keep_running.store(false, Ordering::SeqCst);
    }
}

#[cfg(feature = "metrics")]
pub fn init_metrics() {
    describe_counter!(
        dango_pyth_types::metrics::PYTH_RECONNECTION_ATTEMPTS,
        "Number of reconnection attempts made by the Pyth Lazer client"
    );

    describe_histogram!(
        dango_pyth_types::metrics::PYTH_RECONNECTION_TIME,
        "Time (in seconds) it took to reconnect to Pyth Lazer after a disconnection"
    );

    describe_histogram!(
        dango_pyth_types::metrics::PYTH_UPTIME,
        "Uptime (in seconds) of the Pyth Lazer connection without interruptions"
    );

    describe_histogram!(
        dango_pyth_types::metrics::PYTH_MESSAGES_RECEIVED,
        "Number of data messages received from Pyth Lazer"
    );

    describe_counter!(
        dango_pyth_types::metrics::PYTH_DATA_READ,
        "Number of times data was read from Pyth Lazer"
    );
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_pyth_types::PythLazerSubscriptionDetails,
        pyth_lazer_protocol::{api::Channel, time::FixedRate},
    };

    fn test_client() -> PythClient {
        PythClient::new(NonEmpty::new_unchecked(vec!["wss://example.com"]), "token").unwrap()
    }

    #[test]
    fn group_ids_deduplicates_same_id_same_channel() {
        let mut client = test_client();

        let ids = vec![
            PythLazerSubscriptionDetails {
                id: 1,
                channel: Channel::RealTime,
            },
            // Duplicate: same id + same channel.
            PythLazerSubscriptionDetails {
                id: 1,
                channel: Channel::RealTime,
            },
        ];

        let (ids_per_channel, subscription_ids) = client.group_ids_by_channel(ids);

        // Only one channel entry.
        assert_eq!(ids_per_channel.len(), 1);
        // Only one subscription id.
        assert_eq!(subscription_ids.len(), 1);

        let (_, feed_ids) = &ids_per_channel[&Channel::RealTime];
        // The duplicate was collapsed into a single feed id.
        assert_eq!(feed_ids.len(), 1);
        assert!(feed_ids.contains(&PriceFeedId(1)));
    }

    #[test]
    fn group_ids_keeps_distinct_ids_same_channel() {
        let mut client = test_client();

        let ids = vec![
            PythLazerSubscriptionDetails {
                id: 1,
                channel: Channel::RealTime,
            },
            PythLazerSubscriptionDetails {
                id: 2,
                channel: Channel::RealTime,
            },
        ];

        let (ids_per_channel, subscription_ids) = client.group_ids_by_channel(ids);

        assert_eq!(ids_per_channel.len(), 1);
        assert_eq!(subscription_ids.len(), 1);

        let (_, feed_ids) = &ids_per_channel[&Channel::RealTime];
        assert_eq!(feed_ids.len(), 2);
    }

    #[test]
    fn group_ids_separates_channels() {
        let mut client = test_client();

        let ids = vec![
            PythLazerSubscriptionDetails {
                id: 1,
                channel: Channel::RealTime,
            },
            PythLazerSubscriptionDetails {
                id: 44,
                channel: Channel::FixedRate(FixedRate::RATE_200_MS),
            },
        ];

        let (ids_per_channel, subscription_ids) = client.group_ids_by_channel(ids);

        assert_eq!(ids_per_channel.len(), 2);
        assert_eq!(subscription_ids.len(), 2);
    }

    #[test]
    fn group_ids_deduplicates_across_multiple_duplicates() {
        let mut client = test_client();

        // Three entries for id=1 on FixedRate, two for id=2 on FixedRate, one unique on RealTime.
        let ids = vec![
            PythLazerSubscriptionDetails {
                id: 1,
                channel: Channel::FixedRate(FixedRate::RATE_200_MS),
            },
            PythLazerSubscriptionDetails {
                id: 1,
                channel: Channel::FixedRate(FixedRate::RATE_200_MS),
            },
            PythLazerSubscriptionDetails {
                id: 1,
                channel: Channel::FixedRate(FixedRate::RATE_200_MS),
            },
            PythLazerSubscriptionDetails {
                id: 2,
                channel: Channel::FixedRate(FixedRate::RATE_200_MS),
            },
            PythLazerSubscriptionDetails {
                id: 2,
                channel: Channel::FixedRate(FixedRate::RATE_200_MS),
            },
            PythLazerSubscriptionDetails {
                id: 5,
                channel: Channel::RealTime,
            },
        ];

        let (ids_per_channel, subscription_ids) = client.group_ids_by_channel(ids);

        // Two channels.
        assert_eq!(ids_per_channel.len(), 2);
        assert_eq!(subscription_ids.len(), 2);

        // FixedRate channel: only 2 unique feed ids (1 and 2).
        let (_, fixed_ids) = &ids_per_channel[&Channel::FixedRate(FixedRate::RATE_200_MS)];
        assert_eq!(fixed_ids.len(), 2);
        assert!(fixed_ids.contains(&PriceFeedId(1)));
        assert!(fixed_ids.contains(&PriceFeedId(2)));

        // RealTime channel: 1 feed id.
        let (_, rt_ids) = &ids_per_channel[&Channel::RealTime];
        assert_eq!(rt_ids.len(), 1);
        assert!(rt_ids.contains(&PriceFeedId(5)));
    }
}
