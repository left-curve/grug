#[cfg(feature = "metrics")]
use metrics::{counter, gauge, histogram};
use {
    crate::entities::{
        CandleInterval,
        perps_candle::PerpsCandle,
        perps_candle_query::{MAX_ITEMS, PerpsCandleQueryBuilder},
        perps_pair_price::PerpsPairPrice,
    },
    chrono::{DateTime, Utc},
    futures::future::join_all,
    itertools::Itertools,
    std::{
        collections::{HashMap, HashSet},
        time::Instant,
    },
    strum::IntoEnumIterator,
};

/// Returns the earliest current-bucket start across all `CandleInterval`
/// variants at `now`. Used as the lower bound for the startup rebuild
/// of in-progress candles: any pair_price before this timestamp cannot
/// contribute to any interval's current bucket.
pub fn earliest_current_bucket_start(now: DateTime<Utc>) -> DateTime<Utc> {
    CandleInterval::iter()
        .map(|interval| interval.interval_start(now))
        .min()
        .expect("CandleInterval has at least one variant")
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct PerpsCandleCacheKey {
    pub pair_id: String,
    pub interval: CandleInterval,
}

impl PerpsCandleCacheKey {
    pub fn new(pair_id: String, interval: CandleInterval) -> Self {
        Self { pair_id, interval }
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct PerpsCandleCache {
    pub candles: HashMap<PerpsCandleCacheKey, Vec<PerpsCandle>>,
    pub pair_prices: HashMap<u64, Vec<PerpsPairPrice>>,
    pub completed_candles: HashSet<PerpsCandle>,
    pair_ids: HashSet<String>,
}

impl PerpsCandleCache {
    pub fn pair_price_for_block(&self, block_height: u64) -> Option<&Vec<PerpsPairPrice>> {
        self.pair_prices.get(&block_height)
    }

    /// Returns true if there are gaps in the cached pair prices
    pub fn has_gaps(&self) -> bool {
        let mut heights: Vec<u64> = self.pair_prices.keys().copied().collect();
        heights.sort_unstable();

        for window in heights.windows(2) {
            if let [a, b] = window
                && *b != *a + 1
            {
                return true;
            }
        }

        false
    }

    pub fn add_pair_prices(
        &mut self,
        block_height: u64,
        created_at: DateTime<Utc>,
        pair_prices: Vec<PerpsPairPrice>,
    ) -> Vec<PerpsCandle> {
        #[cfg(feature = "tracing")]
        tracing::debug!(
            block_height,
            pair_prices_count = pair_prices.len(),
            "Adding perps pair_prices",
        );

        let mut result = Vec::new();

        let mut seen_pair_ids: HashSet<String> = HashSet::new();

        for pair_price in pair_prices.iter() {
            for candle_interval in CandleInterval::iter() {
                seen_pair_ids.insert(pair_price.pair_id.clone());

                let key = PerpsCandleCacheKey::new(pair_price.pair_id.clone(), candle_interval);

                if let Some(candle) = self.update_or_create_candle(
                    key,
                    block_height,
                    created_at,
                    Some(pair_price.clone()),
                ) {
                    result.push(candle);
                }
            }
        }

        // Keep creating candles for pairs without new prices in this block
        for pair_id in self.pair_ids.clone() {
            if seen_pair_ids.contains(&pair_id) {
                continue;
            }

            for candle_interval in CandleInterval::iter() {
                let key = PerpsCandleCacheKey::new(pair_id.clone(), candle_interval);

                if let Some(candle) =
                    self.update_or_create_candle(key, block_height, created_at, None)
                {
                    result.push(candle);
                }
            }
        }

        self.pair_ids.extend(seen_pair_ids);

        self.pair_prices
            .entry(block_height)
            .or_default()
            .extend(pair_prices);

        result
    }

    pub fn update_or_create_candle(
        &mut self,
        key: PerpsCandleCacheKey,
        block_height: u64,
        created_at: DateTime<Utc>,
        pair_price: Option<PerpsPairPrice>,
    ) -> Option<PerpsCandle> {
        let time_start = key.interval.interval_start(created_at);
        let interval = key.interval;
        let candles = self.candles.entry(key.clone()).or_default();

        // No previous existing candles, we can just push a new candle
        if candles.is_empty() {
            #[allow(clippy::question_mark)]
            let Some(pair_price) = pair_price else {
                #[cfg(feature = "tracing")]
                tracing::debug!(
                    %created_at,
                    %interval,
                    "Perps candles is empty, no pair_price, can not create candle",
                );

                return None;
            };

            let candle =
                PerpsCandle::new_with_pair_price(&pair_price, interval, time_start, block_height);

            candles.push(candle.clone());

            return Some(candle);
        }

        let current_index = candles
            .iter()
            .rposition(|candle| candle.time_start == time_start);

        let Some(current_index) = current_index else {
            // Find correct position to maintain time order
            let insert_pos = candles
                .iter()
                .position(|c| c.time_start > time_start)
                .unwrap_or(candles.len());

            let Some(pair_price) = pair_price else {
                let previous_candle = insert_pos
                    .checked_sub(1)
                    .and_then(|idx| candles.get(idx))
                    .cloned()?;

                let candle = PerpsCandle::new_with_previous_candle(
                    &previous_candle,
                    interval,
                    time_start,
                    block_height,
                );

                candles.insert(insert_pos, candle.clone());

                self.completed_candles.replace(previous_candle);

                return Some(candle);
            };

            let mut candle =
                PerpsCandle::new_with_pair_price(&pair_price, interval, time_start, block_height);

            // Set open price from previous candle close
            if let Some(previous_candle) = insert_pos
                .checked_sub(1)
                .and_then(|idx| candles.get(idx))
                .cloned()
            {
                candle.open = previous_candle.close;
                candle.set_high_low(previous_candle.close, previous_candle.close);

                self.completed_candles.replace(previous_candle);
            }

            candles.insert(insert_pos, candle.clone());

            // Update the open price of the next candle, if exists.
            if let Some(next_candle) = candles.get_mut(insert_pos + 1) {
                next_candle.open = pair_price.close;
                next_candle.set_high_low(pair_price.close, pair_price.close);
            }

            return Some(candle);
        };

        if block_height == candles[current_index].max_block_height {
            #[cfg(feature = "tracing")]
            tracing::error!(
                %interval,
                %block_height,
                "Seeing the same perps pair_price for the same block_height",
            );
        }

        if let Some(pair_price) = pair_price {
            candles[current_index].volume += pair_price.volume;
            candles[current_index].volume_usd += pair_price.volume_usd;

            candles[current_index].set_high_low(pair_price.high, pair_price.low);

            if block_height < candles[current_index].min_block_height
                && current_index.checked_sub(1).is_none()
            {
                candles[current_index].open = pair_price.close;
            }

            // Set close price from latest block
            if block_height > candles[current_index].max_block_height {
                candles[current_index].close = pair_price.close;

                if let Some(next_candle) = candles.get_mut(current_index + 1) {
                    next_candle.open = pair_price.close;
                    next_candle.set_high_low(pair_price.close, pair_price.close);
                }
            }
        }

        if block_height < candles[current_index].min_block_height {
            candles[current_index].min_block_height = block_height;
        }

        if block_height > candles[current_index].max_block_height {
            candles[current_index].max_block_height = block_height;
        }

        Some(candles[current_index].clone())
    }

    /// Does the cache have all candles for the given dates?
    pub fn date_interval_available(
        &self,
        key: &PerpsCandleCacheKey,
        earlier_than: Option<DateTime<Utc>>,
        later_than: Option<DateTime<Utc>>,
    ) -> bool {
        if let Some(candles) = self.candles.get(key) {
            if candles.is_empty() {
                return false;
            }

            if let Some(earlier_than) = earlier_than
                && candles.last().is_some_and(|c| c.time_start > earlier_than)
            {
                return false;
            }

            if let Some(later_than) = later_than
                && candles.first().is_some_and(|c| c.time_start < later_than)
            {
                return false;
            }

            return true;
        }

        false
    }

    pub fn get_candles(&self, key: &PerpsCandleCacheKey) -> Option<&Vec<PerpsCandle>> {
        self.candles.get(key)
    }

    pub fn get_last_candle(&self, key: &PerpsCandleCacheKey) -> Option<&PerpsCandle> {
        let _start = Instant::now();

        let result = self.candles.get(key).and_then(|candles| candles.last());

        #[cfg(feature = "metrics")]
        {
            if result.is_some() {
                counter!("indexer.clickhouse.perps_candles.cache.hits").increment(1);
            } else {
                counter!("indexer.clickhouse.perps_candles.cache.misses").increment(1);
            }

            let duration = _start.elapsed();
            histogram!("indexer.clickhouse.perps_candles.cache.lookup.duration.seconds")
                .record(duration.as_secs_f64());
        }

        result
    }

    /// Preloads candles in parallel.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn preload_pairs(
        &mut self,
        pair_ids: &[String],
        clickhouse_client: &clickhouse::Client,
    ) -> crate::error::Result<()> {
        let last_prices = PerpsPairPrice::latest_prices(clickhouse_client, MAX_ITEMS).await?;

        let Some(highest_block_height) = last_prices.last().map(|price| price.block_height) else {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "No perps last prices found, skipping perps candle preload since it's all empty."
            );
            return Ok(());
        };

        self.pair_prices.extend(last_prices.into_iter().fold(
            HashMap::new(),
            |mut acc: HashMap<u64, Vec<PerpsPairPrice>>, price| {
                acc.entry(price.block_height).or_default().push(price);
                acc
            },
        ));

        // Add all pair_ids
        let all_pair_ids = PerpsPairPrice::all_pair_ids(clickhouse_client).await?;
        self.pair_ids.extend(all_pair_ids);

        // Create all fetch tasks
        let fetch_tasks = pair_ids
            .iter()
            .flat_map(|pair_id| {
                CandleInterval::iter().map(move |interval| {
                    let key = PerpsCandleCacheKey::new(pair_id.clone(), interval);

                    async move {
                        let query_builder =
                            PerpsCandleQueryBuilder::new(key.interval, key.pair_id.clone())
                                .with_limit(MAX_ITEMS);

                        let mut candles = query_builder.fetch_all(clickhouse_client).await?.candles;

                        if let Some(candle) = candles.first()
                            && candle.max_block_height < highest_block_height
                        {
                            #[cfg(feature = "tracing")]
                            tracing::info!(
                                %candle.max_block_height,
                                %highest_block_height,
                                %key.pair_id,
                                %key.interval,
                                "Perps candle is older than latest price."
                            );
                        }

                        candles.reverse();

                        Ok::<_, crate::error::IndexerError>((key, candles))
                    }
                })
            })
            .collect::<Vec<_>>();

        let results = join_all(fetch_tasks).await;

        for result in results {
            match result {
                Ok((key, candles)) => {
                    *self.candles.entry(key).or_default() = candles;
                },
                Err(_err) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(err = %_err, "Failed to preload perps candles");
                },
            }
        }

        // Rebuild the in-progress perps candle for each interval from raw
        // pair_prices. See the spot equivalent in `indexer::candles::cache`
        // for the full rationale: the current bucket lives only in memory
        // until it closes or a graceful shutdown flushes it, and an
        // ungraceful shutdown (e.g. the panic that stops the chain at an
        // upgrade block) loses that aggregation.
        //
        // All pairs must be replayed in a single call: the rebuild drops
        // every in-progress candle in the cache (across all pairs) before
        // replaying, so a per-pair loop would have each iteration overwrite
        // the rebuild produced by the previous iteration.
        let now = Utc::now();
        let earliest_start = earliest_current_bucket_start(now);

        let replay_tasks = pair_ids.iter().map(|pair_id| {
            let pair_id = pair_id.clone();
            async move { PerpsPairPrice::since(clickhouse_client, &pair_id, earliest_start).await }
        });

        let mut all_prices: Vec<PerpsPairPrice> = Vec::new();
        for result in join_all(replay_tasks).await {
            match result {
                Ok(prices) => all_prices.extend(prices),
                Err(_err) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(
                        err = %_err,
                        "Failed to fetch perps pair_prices for in-progress candle rebuild",
                    );
                },
            }
        }

        #[cfg(feature = "tracing")]
        let replayed = all_prices.len();
        self.rebuild_in_progress_from_prices(&all_prices, now);

        #[cfg(feature = "tracing")]
        tracing::info!(replayed, "Preloaded all perps candles");

        self.update_metrics();

        Ok(())
    }

    /// Drop any cached perps candles whose `time_start` falls inside the
    /// current bucket (i.e. the in-progress candle for each interval) and
    /// rebuild them by replaying `pair_prices` through
    /// `update_or_create_candle`. Pair prices are filtered per-interval to
    /// only touch the interval's current bucket, so past completed candles
    /// already in the cache are left untouched.
    pub fn rebuild_in_progress_from_prices(
        &mut self,
        pair_prices: &[PerpsPairPrice],
        now: DateTime<Utc>,
    ) {
        let current_bucket_starts: HashMap<CandleInterval, DateTime<Utc>> = CandleInterval::iter()
            .map(|interval| (interval, interval.interval_start(now)))
            .collect();

        for (key, candles) in self.candles.iter_mut() {
            let start = current_bucket_starts[&key.interval];
            candles.retain(|c| c.time_start < start);
        }

        for pair_price in pair_prices {
            for interval in CandleInterval::iter() {
                let start = current_bucket_starts[&interval];
                if pair_price.created_at < start {
                    continue;
                }

                let key = PerpsCandleCacheKey::new(pair_price.pair_id.clone(), interval);
                self.update_or_create_candle(
                    key,
                    pair_price.block_height,
                    pair_price.created_at,
                    Some(pair_price.clone()),
                );
            }
        }
    }

    // Keep last N candles, store the rest on Clickhouse
    pub fn compact_keep_n(&mut self, n: usize) {
        self.candles.retain(|_key, candles| {
            if candles.is_empty() {
                false
            } else {
                let start = candles.len().saturating_sub(n);
                let _drained: Vec<_> = candles.drain(..start).collect();

                true
            }
        });

        let top_n_keys: HashSet<u64> = self
            .pair_prices
            .keys()
            .copied()
            .sorted_by(|a, b| b.cmp(a))
            .take(n)
            .collect();

        self.pair_prices.retain(|k, _| top_n_keys.contains(k));
    }

    fn update_metrics(&self) {
        #[cfg(feature = "metrics")]
        {
            gauge!("indexer.clickhouse.perps_candles.cache.size.entries")
                .set(self.candles.len() as f64);

            let total_candles: usize = self.candles.values().map(Vec::len).sum();
            gauge!("indexer.clickhouse.perps_candles.cache.size.candles").set(total_candles as f64);

            gauge!("indexer.clickhouse.perps_pair_prices.cache.size.entries")
                .set(self.pair_prices.len() as f64);

            let total_pair_prices: usize = self.pair_prices.values().map(Vec::len).sum();
            gauge!("indexer.clickhouse.perps_pair_prices.cache.size.pair_prices")
                .set(total_pair_prices as f64);
        }
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
impl PerpsCandleCache {
    pub fn add_multi_block_pair_prices(
        &mut self,
        pair_prices: Vec<PerpsPairPrice>,
    ) -> crate::error::Result<()> {
        for pair_price in pair_prices {
            let block_height = pair_price.block_height;
            self.add_pair_prices(block_height, pair_price.created_at, vec![pair_price]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assertor::*,
        chrono::NaiveDateTime,
        dango_math::{NumberConst, Udec128_6},
        itertools::Itertools,
        std::{collections::VecDeque, str::FromStr},
    };

    fn parse_timestamp(s: &str) -> crate::error::Result<DateTime<Utc>> {
        let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.3f")?;
        Ok(DateTime::from_naive_utc_and_offset(naive, Utc))
    }

    fn perps_pair_price(
        pair_id: &str,
        high: u128,
        low: u128,
        close: u128,
        volume: u128,
        volume_usd: u128,
        created_at: &str,
        block_height: u64,
    ) -> crate::error::Result<PerpsPairPrice> {
        Ok(PerpsPairPrice {
            pair_id: pair_id.to_string(),
            high: Udec128_6::raw(dango_math::Int::new(high)),
            low: Udec128_6::raw(dango_math::Int::new(low)),
            close: Udec128_6::raw(dango_math::Int::new(close)),
            volume: Udec128_6::raw(dango_math::Int::new(volume)),
            volume_usd: Udec128_6::raw(dango_math::Int::new(volume_usd)),
            created_at: NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S%.f")?
                .and_utc(),
            block_height,
        })
    }

    fn parsed_pair_prices() -> crate::error::Result<Vec<PerpsPairPrice>> {
        let pair_id = "perp/ethusd";

        Ok(vec![
            perps_pair_price(
                pair_id,
                2000_000000,
                2000_000000,
                2000_000000,
                0,
                0,
                "2025-08-13 17:36:00.038565",
                1277884,
            )?,
            perps_pair_price(
                pair_id,
                2000_000000,
                2000_000000,
                2000_000000,
                0,
                0,
                "2025-08-13 17:36:01.086616",
                1277885,
            )?,
            perps_pair_price(
                pair_id,
                2010_000000,
                1990_000000,
                1995_000000,
                5_000000,
                9975_000000,
                "2025-08-13 17:36:02.134583",
                1277886,
            )?,
            perps_pair_price(
                pair_id,
                2005_000000,
                1998_000000,
                2002_000000,
                3_000000,
                6006_000000,
                "2025-08-13 17:36:03.182534",
                1277887,
            )?,
            perps_pair_price(
                pair_id,
                2002_000000,
                2002_000000,
                2002_000000,
                0,
                0,
                "2025-08-13 17:36:04.230527",
                1277888,
            )?,
        ])
    }

    #[tokio::test]
    async fn create_candles() -> crate::error::Result<()> {
        let mut candle_cache = PerpsCandleCache::default();

        candle_cache.add_multi_block_pair_prices(parsed_pair_prices()?)?;

        let cache_key =
            PerpsCandleCacheKey::new("perp/ethusd".to_string(), CandleInterval::OneSecond);

        let mut candles: VecDeque<PerpsCandle> = candle_cache
            .get_candles(&cache_key)
            .expect("No candles found")
            .clone()
            .into();

        let mut previous_candle = candles.pop_front().expect("No previous candle found");

        while let Some(candle) = candles.pop_front() {
            assert!(
                candle.time_start > previous_candle.time_start,
                "Candle time_start is not greater than previous candle"
            );
            assert!(
                candle.max_block_height >= previous_candle.max_block_height,
                "Candle max_block_height is not greater than or equal to previous candle"
            );
            assert_eq!(
                previous_candle.close, candle.open,
                "Candle close price does not match next candle open price"
            );

            previous_candle = candle;
        }

        Ok(())
    }

    #[tokio::test]
    async fn when_pair_prices_are_not_in_order() -> crate::error::Result<()> {
        let mut candle_cache = PerpsCandleCache::default();

        let pair_id = "perp/ethusd";

        let pair_prices = vec![
            perps_pair_price(
                pair_id,
                2000_000000,
                2000_000000,
                2000_000000,
                0,
                0,
                "2025-08-13 17:36:01.038565",
                4,
            )?,
            perps_pair_price(
                pair_id,
                2000_000000,
                2000_000000,
                2000_000000,
                0,
                0,
                "2025-08-13 17:36:00.086616",
                2,
            )?,
            perps_pair_price(
                pair_id,
                2010_000000,
                1990_000000,
                1995_000000,
                5_000000,
                9975_000000,
                "2025-08-13 17:36:01.734583",
                6,
            )?,
        ];

        candle_cache.add_multi_block_pair_prices(pair_prices)?;

        let cache_key = PerpsCandleCacheKey::new(pair_id.to_string(), CandleInterval::OneSecond);

        assert_that!(candle_cache.get_candles(&cache_key).cloned().unwrap()).has_length(2);

        Ok(())
    }

    #[tokio::test]
    async fn correct_candles_when_pair_prices_are_not_in_order() -> crate::error::Result<()> {
        let pair_id = "perp/ethusd";

        let pair_prices = vec![
            perps_pair_price(
                pair_id,
                25_000000,
                25_000000,
                25_000000,
                25_000000,
                625_000000,
                "1971-01-01 00:00:01.500",
                6,
            )?,
            perps_pair_price(
                pair_id,
                27_500000,
                27_500000,
                27_500000,
                25_000000,
                687_500000,
                "1971-01-01 00:00:00.500",
                2,
            )?,
            perps_pair_price(
                pair_id,
                25_000000,
                25_000000,
                25_000000,
                25_000000,
                625_000000,
                "1971-01-01 00:00:01.000",
                4,
            )?,
        ];

        let len = pair_prices.len();
        let all_pair_prices: Vec<Vec<PerpsPairPrice>> =
            pair_prices.into_iter().permutations(len).collect();

        for pair_prices in all_pair_prices {
            let mut candle_cache = PerpsCandleCache::default();

            candle_cache.add_multi_block_pair_prices(pair_prices)?;

            let cache_key =
                PerpsCandleCacheKey::new(pair_id.to_string(), CandleInterval::OneMinute);

            let cached_candles = candle_cache
                .get_candles(&cache_key)
                .expect("no candles found");

            let expected_candle = PerpsCandle {
                pair_id: pair_id.to_string(),
                interval: CandleInterval::OneMinute,
                close: Udec128_6::from_str("25").unwrap(),
                high: Udec128_6::from_str("27.5").unwrap(),
                low: Udec128_6::from_str("25").unwrap(),
                open: Udec128_6::from_str("27.5").unwrap(),
                volume: Udec128_6::from_str("75").unwrap(),
                volume_usd: Udec128_6::from_str("1937.5").unwrap(),
                max_block_height: 6,
                min_block_height: 2,
                time_start: parse_timestamp("1971-01-01 00:00:00.000")?,
            };

            assert_that!(cached_candles.first().unwrap()).is_equal_to(&expected_candle);

            let cache_key =
                PerpsCandleCacheKey::new(pair_id.to_string(), CandleInterval::OneSecond);

            let cached_candles = candle_cache
                .get_candles(&cache_key)
                .expect("no candles found");

            let expected_candles = vec![
                PerpsCandle {
                    pair_id: pair_id.to_string(),
                    interval: CandleInterval::OneSecond,
                    close: Udec128_6::from_str("27.5").unwrap(),
                    high: Udec128_6::from_str("27.5").unwrap(),
                    low: Udec128_6::from_str("27.5").unwrap(),
                    open: Udec128_6::from_str("27.5").unwrap(),
                    volume: Udec128_6::from_str("25").unwrap(),
                    volume_usd: Udec128_6::from_str("687.5").unwrap(),
                    min_block_height: 2,
                    max_block_height: 2,
                    time_start: parse_timestamp("1971-01-01 00:00:00.000")?,
                },
                PerpsCandle {
                    pair_id: pair_id.to_string(),
                    interval: CandleInterval::OneSecond,
                    close: Udec128_6::from_str("25").unwrap(),
                    high: Udec128_6::from_str("27.5").unwrap(),
                    low: Udec128_6::from_str("25").unwrap(),
                    open: Udec128_6::from_str("27.5").unwrap(),
                    volume: Udec128_6::from_str("50").unwrap(),
                    volume_usd: Udec128_6::from_str("1250").unwrap(),
                    min_block_height: 4,
                    max_block_height: 6,
                    time_start: parse_timestamp("1971-01-01 00:00:01.000")?,
                },
            ];

            assert_that!(cached_candles).is_equal_to(&expected_candles);
        }

        Ok(())
    }

    #[tokio::test]
    async fn close_price_is_correct() -> crate::error::Result<()> {
        let mut candle_cache = PerpsCandleCache::default();

        let pair_id = "perp/ethusd";

        let pair_prices = vec![
            perps_pair_price(
                pair_id,
                27_500000,
                27_500000,
                27_500000,
                5_000000,
                137_500000,
                "1971-01-01 00:00:00.500",
                2,
            )?,
            perps_pair_price(
                pair_id,
                27_500000,
                27_500000,
                27_500000,
                5_000000,
                137_500000,
                "1971-01-01 00:00:01.000",
                4,
            )?,
            perps_pair_price(
                pair_id,
                25_000000,
                25_000000,
                25_000000,
                5_000000,
                125_000000,
                "1971-01-01 00:00:01.500",
                6,
            )?,
        ];

        candle_cache.add_multi_block_pair_prices(pair_prices)?;

        let cache_key = PerpsCandleCacheKey::new(pair_id.to_string(), CandleInterval::OneSecond);

        let candles = candle_cache.get_candles(&cache_key).cloned().unwrap();

        let first_candle = candles.first().unwrap();

        assert_that!(first_candle.open).is_equal_to(Udec128_6::from_str("27.5").unwrap());
        assert_that!(first_candle.close).is_equal_to(Udec128_6::from_str("27.5").unwrap());

        let last_candle = candles.last().unwrap();
        assert_that!(last_candle.open).is_equal_to(Udec128_6::from_str("27.5").unwrap());
        assert_that!(last_candle.close).is_equal_to(Udec128_6::from_str("25").unwrap());

        Ok(())
    }

    #[tokio::test]
    async fn missing_pair_prices_creates_candles() -> crate::error::Result<()> {
        let mut candle_cache = PerpsCandleCache::default();

        let pair_id = "perp/ethusd";

        let pair_prices = vec![
            perps_pair_price(
                pair_id,
                27_500000,
                27_500000,
                27_500000,
                25_000000,
                625_000000,
                "1971-01-01 00:00:00.500",
                2,
            )?,
            perps_pair_price(
                pair_id,
                27_500000,
                27_500000,
                27_500000,
                25_000000,
                625_000000,
                "1971-01-01 00:00:01.500",
                4,
            )?,
        ];

        candle_cache.add_multi_block_pair_prices(pair_prices)?;

        candle_cache.add_pair_prices(3, parse_timestamp("1971-01-01 00:00:01.000")?, vec![]);
        candle_cache.add_pair_prices(5, parse_timestamp("1971-01-01 00:00:02.000")?, vec![]);

        let cache_key = PerpsCandleCacheKey::new(pair_id.to_string(), CandleInterval::OneSecond);

        let cached_candles = candle_cache
            .get_candles(&cache_key)
            .expect("no candles found");

        let expected_candles = vec![
            PerpsCandle {
                pair_id: pair_id.to_string(),
                interval: CandleInterval::OneSecond,
                close: Udec128_6::from_str("27.5").unwrap(),
                high: Udec128_6::from_str("27.5").unwrap(),
                low: Udec128_6::from_str("27.5").unwrap(),
                open: Udec128_6::from_str("27.5").unwrap(),
                volume: Udec128_6::from_str("25").unwrap(),
                volume_usd: Udec128_6::from_str("625").unwrap(),
                min_block_height: 2,
                max_block_height: 2,
                time_start: parse_timestamp("1971-01-01 00:00:00.000")?,
            },
            PerpsCandle {
                pair_id: pair_id.to_string(),
                interval: CandleInterval::OneSecond,
                close: Udec128_6::from_str("27.5").unwrap(),
                high: Udec128_6::from_str("27.5").unwrap(),
                low: Udec128_6::from_str("27.5").unwrap(),
                open: Udec128_6::from_str("27.5").unwrap(),
                volume: Udec128_6::from_str("25").unwrap(),
                volume_usd: Udec128_6::from_str("625").unwrap(),
                min_block_height: 3,
                max_block_height: 4,
                time_start: parse_timestamp("1971-01-01 00:00:01.000")?,
            },
            PerpsCandle {
                pair_id: pair_id.to_string(),
                interval: CandleInterval::OneSecond,
                close: Udec128_6::from_str("27.5").unwrap(),
                high: Udec128_6::from_str("27.5").unwrap(),
                low: Udec128_6::from_str("27.5").unwrap(),
                open: Udec128_6::from_str("27.5").unwrap(),
                volume: Udec128_6::ZERO,
                volume_usd: Udec128_6::ZERO,
                min_block_height: 5,
                max_block_height: 5,
                time_start: parse_timestamp("1971-01-01 00:00:02.000")?,
            },
        ];

        assert_that!(cached_candles).is_equal_to(&expected_candles);

        Ok(())
    }

    /// Mirrors the spot-side scenario: a chain upgrade panics, the in-memory
    /// candle cache is wiped, but pair_prices are already persisted. Replay
    /// must rebuild the 1d in-progress candle with the full volume, and the
    /// new candle's `open` must bridge from the previous day's `close`.
    #[tokio::test]
    async fn rebuild_from_prices_rebuilds_current_bucket() -> crate::error::Result<()> {
        let pair_id = "perp/ethusd";
        let mut cache = PerpsCandleCache::default();

        let now = parse_timestamp("2026-04-20 14:05:00.000")?;

        let key = PerpsCandleCacheKey::new(pair_id.to_string(), CandleInterval::OneDay);
        let yesterday = PerpsCandle {
            pair_id: pair_id.to_string(),
            interval: CandleInterval::OneDay,
            close: Udec128_6::from_str("48").unwrap(),
            high: Udec128_6::from_str("52").unwrap(),
            low: Udec128_6::from_str("47").unwrap(),
            open: Udec128_6::from_str("49").unwrap(),
            volume: Udec128_6::from_str("1000").unwrap(),
            volume_usd: Udec128_6::from_str("49500").unwrap(),
            max_block_height: 500,
            min_block_height: 100,
            time_start: parse_timestamp("2026-04-19 00:00:00.000")?,
        };
        cache
            .candles
            .entry(key.clone())
            .or_default()
            .push(yesterday.clone());

        let pair_prices = vec![
            perps_pair_price(
                pair_id,
                51_000000,
                49_000000,
                50_000000,
                10_000000,
                500_000000,
                "2026-04-20 01:00:00.000",
                501,
            )?,
            perps_pair_price(
                pair_id,
                53_000000,
                49_500000,
                52_000000,
                5_000000,
                260_000000,
                "2026-04-20 10:00:00.000",
                600,
            )?,
        ];

        cache.rebuild_in_progress_from_prices(&pair_prices, now);

        let candles = cache.get_candles(&key).cloned().expect("no 1d candles");
        assert_that!(candles.clone()).has_length(2);
        // Yesterday's candle untouched.
        assert_that!(candles[0].clone()).is_equal_to(yesterday.clone());
        // Today's candle bridges from yesterday's close.
        let today = &candles[1];
        assert_that!(today.time_start).is_equal_to(parse_timestamp("2026-04-20 00:00:00.000")?);
        assert_that!(today.open).is_equal_to(yesterday.close);
        assert_that!(today.close).is_equal_to(Udec128_6::from_str("52").unwrap());
        assert_that!(today.volume).is_equal_to(Udec128_6::from_str("15").unwrap());
        assert_that!(today.volume_usd).is_equal_to(Udec128_6::from_str("760").unwrap());
        assert_that!(today.max_block_height).is_equal_to(600);

        Ok(())
    }

    /// Regression test for a multi-pair bug: the rebuild drops every
    /// in-progress candle in the cache before replaying, so the preload
    /// must replay all pairs' prices in a single call. If prices from
    /// multiple pairs are replayed in one call, every pair must end up
    /// with its rebuilt in-progress candle.
    #[tokio::test]
    async fn rebuild_from_prices_rebuilds_all_pairs_in_one_call() -> crate::error::Result<()> {
        let eth_pair = "perp/ethusd";
        let btc_pair = "perp/btcusd";
        let mut cache = PerpsCandleCache::default();

        let now = parse_timestamp("2026-04-20 14:05:00.000")?;

        let eth_key = PerpsCandleCacheKey::new(eth_pair.to_string(), CandleInterval::OneDay);
        let btc_key = PerpsCandleCacheKey::new(btc_pair.to_string(), CandleInterval::OneDay);

        let eth_yesterday = PerpsCandle {
            pair_id: eth_pair.to_string(),
            interval: CandleInterval::OneDay,
            close: Udec128_6::from_str("48").unwrap(),
            high: Udec128_6::from_str("52").unwrap(),
            low: Udec128_6::from_str("47").unwrap(),
            open: Udec128_6::from_str("49").unwrap(),
            volume: Udec128_6::from_str("1000").unwrap(),
            volume_usd: Udec128_6::from_str("49500").unwrap(),
            max_block_height: 500,
            min_block_height: 100,
            time_start: parse_timestamp("2026-04-19 00:00:00.000")?,
        };
        let btc_yesterday = PerpsCandle {
            pair_id: btc_pair.to_string(),
            interval: CandleInterval::OneDay,
            close: Udec128_6::from_str("60000").unwrap(),
            high: Udec128_6::from_str("61000").unwrap(),
            low: Udec128_6::from_str("59000").unwrap(),
            open: Udec128_6::from_str("60000").unwrap(),
            volume: Udec128_6::from_str("100").unwrap(),
            volume_usd: Udec128_6::from_str("6000000").unwrap(),
            max_block_height: 500,
            min_block_height: 100,
            time_start: parse_timestamp("2026-04-19 00:00:00.000")?,
        };
        cache
            .candles
            .entry(eth_key.clone())
            .or_default()
            .push(eth_yesterday.clone());
        cache
            .candles
            .entry(btc_key.clone())
            .or_default()
            .push(btc_yesterday.clone());

        let pair_prices = vec![
            perps_pair_price(
                eth_pair,
                51_000000,
                49_000000,
                50_000000,
                10_000000,
                500_000000,
                "2026-04-20 01:00:00.000",
                501,
            )?,
            perps_pair_price(
                btc_pair,
                62_000_000_000,
                61_000_000_000,
                61_500_000_000,
                5_000_000,
                307_500_000_000,
                "2026-04-20 02:00:00.000",
                502,
            )?,
        ];

        cache.rebuild_in_progress_from_prices(&pair_prices, now);

        let eth_candles = cache.get_candles(&eth_key).cloned().expect("no eth 1d");
        let btc_candles = cache.get_candles(&btc_key).cloned().expect("no btc 1d");

        assert_that!(eth_candles.clone()).has_length(2);
        assert_that!(btc_candles.clone()).has_length(2);
        assert_that!(eth_candles[0].clone()).is_equal_to(eth_yesterday.clone());
        assert_that!(btc_candles[0].clone()).is_equal_to(btc_yesterday.clone());

        let eth_today = &eth_candles[1];
        assert_that!(eth_today.time_start).is_equal_to(parse_timestamp("2026-04-20 00:00:00.000")?);
        assert_that!(eth_today.open).is_equal_to(eth_yesterday.close);
        assert_that!(eth_today.close).is_equal_to(Udec128_6::from_str("50").unwrap());
        assert_that!(eth_today.volume).is_equal_to(Udec128_6::from_str("10").unwrap());

        let btc_today = &btc_candles[1];
        assert_that!(btc_today.time_start).is_equal_to(parse_timestamp("2026-04-20 00:00:00.000")?);
        assert_that!(btc_today.open).is_equal_to(btc_yesterday.close);
        assert_that!(btc_today.close).is_equal_to(Udec128_6::from_str("61500").unwrap());
        assert_that!(btc_today.volume).is_equal_to(Udec128_6::from_str("5").unwrap());

        Ok(())
    }
}
