use {
    dango_primitives::{Inner, Lengthy, NonEmpty, btree_map},
    dango_pyth_client::PythClientTrait,
    dango_pyth_types::{
        LeEcdsaMessage, PayloadData, PayloadPropertyValue, PythLazerSubscriptionDetails,
    },
    std::{collections::BTreeMap, fmt::Debug, time::Duration},
    tokio_stream::StreamExt,
};

struct VaasChecker {
    values: BTreeMap<u32, BTreeMap<&'static str, i64>>,
}

impl VaasChecker {
    pub fn new<I, P>(ids: I, _client: &P) -> Self
    where
        I: IntoIterator<Item = PythLazerSubscriptionDetails>,
        P: PythClientTrait + Debug,
    {
        let mut values = BTreeMap::new();
        for id in ids.into_iter() {
            values.insert(
                id.id,
                btree_map! {
                    "price" => 0,
                    "publish_time" => 0,
                    // Set to -1 since the first iteration will increase the counter
                    // (price and publish time are 0).
                    "price_change" => -1,
                    "publish_time_change" => -1,
                },
            );
        }
        Self { values }
    }

    pub fn update_values(&mut self, msgs: Vec<LeEcdsaMessage>) {
        assert!(!msgs.is_empty(), "No data found");

        for msg in msgs.into_iter() {
            let data = PayloadData::deserialize_slice_le(&msg.payload).unwrap();

            let new_publish_time = data.timestamp_us.as_micros() as i64;

            for price_feed in data.feeds {
                let mut new_price = None;
                for property in price_feed.properties {
                    // Update price if the property is Price.
                    if let PayloadPropertyValue::Price(price) = property {
                        new_price = price;
                    }
                }

                let new_price = new_price.unwrap().mantissa_i64();

                let element = self.values.get_mut(&price_feed.feed_id.0).unwrap();

                // Update the price and publish time.
                let old_price = element.insert("price", new_price).unwrap();
                let old_publish_time = element.insert("publish_time", new_publish_time).unwrap();

                assert!(new_publish_time >= old_publish_time, "Time has decreased");

                // Increase counter if the price has changed.
                element.entry("price_change").and_modify(|price_change| {
                    if new_price != old_price {
                        *price_change += 1;
                    }
                });

                // Increase counter if the publish time has changed.
                element
                    .entry("publish_time_change")
                    .and_modify(|publish_time_change| {
                        if new_publish_time != old_publish_time {
                            *publish_time_change += 1;
                        }
                    });
            }
        }
    }

    pub fn assert_changes(&self) {
        // Assert that the prices and publish times have changed at least once.
        for (id, element) in self.values.iter() {
            assert!(
                element.get("price_change").unwrap() > &0,
                "No price change for ID: {id}"
            );
            assert!(
                element.get("publish_time_change").unwrap() > &0,
                "No publish time change for ID: {id}"
            );
        }
    }
}

// Test for streaming vaas.
pub async fn test_stream<P, I>(mut client: P, ids1: I, ids2: I)
where
    P: PythClientTrait + Debug,
    P::Error: Debug,
    I: IntoIterator<Item = PythLazerSubscriptionDetails> + Clone + Lengthy + Send + 'static,
{
    let mut vaas_checker = VaasChecker::new(ids1.clone(), &client);
    let mut stream = client.stream(NonEmpty::new_unchecked(ids1)).await.unwrap();
    let mut not_none_vaas = 0;

    tokio::time::sleep(Duration::from_secs(2)).await;

    while not_none_vaas < 5 {
        if let Some(price_update) = stream.next().await {
            not_none_vaas += 1;
            vaas_checker.update_values(price_update.into_inner());
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    // Assert that the prices and publish times have changed at least once.
    vaas_checker.assert_changes();

    // Close the stream.
    client.close();

    // Read the stream; there could be one remaining value.
    stream.next().await;

    // Assert that the stream is closed.
    for _ in 0..5 {
        assert!(stream.next().await.is_none(), "Stream is not closed");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Open a new connection with the second ids.
    let mut vaas_checker = VaasChecker::new(ids2.clone(), &client);
    let mut stream = client.stream(NonEmpty::new_unchecked(ids2)).await.unwrap();
    let mut not_none_vaas = 0;

    tokio::time::sleep(Duration::from_secs(2)).await;

    while not_none_vaas < 5 {
        if let Some(price_update) = stream.next().await {
            not_none_vaas += 1;
            vaas_checker.update_values(price_update.into_inner());
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    // Assert that the prices and publish times have changed at least once.
    vaas_checker.assert_changes();

    // Close the stream.
    client.close();

    // Read the stream; there could be one remaining value.
    stream.next().await;

    // Assert that the stream is closed.
    for _ in 0..5 {
        assert!(stream.next().await.is_none(), "Stream is not closed");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
