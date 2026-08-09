use {
    assertor::*,
    dango_app::{Indexer, NaiveProposalPreparer},
    dango_genesis::Contracts,
    dango_indexer_clickhouse::{
        entities::{
            CandleInterval,
            perps_candle::PerpsCandle,
            perps_candle_query::{PerpsCandleQueryBuilder, PerpsCandleResult},
            perps_pair_price::PerpsPairPrice,
        },
        indexer::perps_candles::cache::{PerpsCandleCache, PerpsCandleCacheKey},
    },
    dango_math::{NumberConst, Udec128_6, Uint128},
    dango_order_book::{Dimensionless, OrderKind, Quantity, TimeInForce, UsdPrice},
    dango_primitives::{
        BlockInfo, Coins, Denom, Duration, Hash256, ResultExt, Timestamp, btree_map,
    },
    dango_testing::{
        OracleTestEntry, Preset, TestAccounts, TestOption, TestSuiteNaiveWithIndexer,
        create_perps_fill, pair_id, setup_perps_env, setup_test_naive_with_indexer,
        setup_test_with_indexer_pp_and_custom_genesis,
    },
    dango_types::{
        constants::usdc,
        perps::{self, PairParam, RateSchedule},
    },
    std::collections::HashMap,
};

/// Place a resting limit ask for user2 (no immediate fill).
async fn place_limit_ask(
    suite: &mut TestSuiteNaiveWithIndexer,
    accounts: &mut TestAccounts,
    contracts: &Contracts,
    price: u128,
    size: u128,
) {
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id(),
                size: Quantity::new_int(-(size as i128)),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(price as i128),
                    time_in_force: TimeInForce::PostOnly,
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
}

/// Submit a market buy for user1 (crosses resting asks).
async fn market_buy(
    suite: &mut TestSuiteNaiveWithIndexer,
    accounts: &mut TestAccounts,
    contracts: &Contracts,
    size: u128,
) {
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id(),
                size: Quantity::new_int(size as i128),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();
}

/// Fetch candles for the test pair.
async fn query_candles(
    clickhouse_client: &clickhouse::Client,
    interval: CandleInterval,
) -> anyhow::Result<PerpsCandleResult> {
    Ok(
        PerpsCandleQueryBuilder::new(interval, pair_id().to_string())
            .fetch_all(clickhouse_client)
            .await?,
    )
}

/// Fetch the latest `PerpsPairPrice` for the test pair.
async fn find_pair_price(clickhouse_client: &clickhouse::Client) -> anyhow::Result<PerpsPairPrice> {
    let pair_prices = PerpsPairPrice::latest_prices(clickhouse_client, 10).await?;
    Ok(pair_prices
        .into_iter()
        .find(|p| p.pair_id == pair_id().to_string())
        .expect("Should find pair price for perp/ethusd"))
}

/// Assert candles are ordered newest-first and have open/close price continuity.
fn assert_candle_continuity(candles: &[PerpsCandle]) {
    for window in candles.windows(2) {
        assert!(
            window[0].time_start > window[1].time_start,
            "Candles should be ordered newest first"
        );
        assert_eq!(
            window[0].open, window[1].close,
            "Candle open should equal previous candle's close"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn index_perps_candles_basic() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, _, _, clickhouse_context, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 100_000).await;

    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 2_000, 5).await;

    suite.app.indexer.wait_for_finish().await?;

    let ch = clickhouse_context.clickhouse_client();

    // Verify pair price
    let pp = find_pair_price(ch).await?;

    let expected_price = Udec128_6::new(2_000);
    assert_that!(pp.close).is_equal_to(expected_price);
    assert_that!(pp.high).is_equal_to(expected_price);
    assert_that!(pp.low).is_equal_to(expected_price);
    // Only one side of each fill is counted (positive fill_size).
    assert_that!(pp.volume).is_equal_to(Udec128_6::new(5));
    // volume_usd = 5 * 2000 = 10000
    assert_that!(pp.volume_usd).is_equal_to(Udec128_6::new(10_000));

    // Verify 1-minute candle
    let candle = query_candles(ch, CandleInterval::OneMinute)
        .await?
        .candles
        .into_iter()
        .next()
        .expect("Should have a 1-minute candle");

    assert_that!(candle.pair_id).is_equal_to(pair_id().to_string());
    assert_that!(candle.open).is_equal_to(expected_price);
    assert_that!(candle.high).is_equal_to(expected_price);
    assert_that!(candle.low).is_equal_to(expected_price);
    assert_that!(candle.close).is_equal_to(expected_price);
    assert_that!(candle.volume).is_equal_to(Udec128_6::new(5));
    assert_that!(candle.volume_usd).is_equal_to(Udec128_6::new(10_000));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn index_perps_candles_multiple_fills_same_block() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, _, _, clickhouse_context, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 100_000).await;

    // Place two limit asks at different prices, then a large market buy that
    // fills both in the same block.
    place_limit_ask(&mut suite, &mut accounts, &contracts, 2_000, 3).await;
    place_limit_ask(&mut suite, &mut accounts, &contracts, 2_100, 2).await;
    market_buy(&mut suite, &mut accounts, &contracts, 5).await;

    suite.app.indexer.wait_for_finish().await?;

    let ch = clickhouse_context.clickhouse_client();

    let pp = find_pair_price(ch).await?;

    // high = max(2000, 2100), low = min(2000, 2100)
    assert_that!(pp.high).is_equal_to(Udec128_6::new(2_100));
    assert_that!(pp.low).is_equal_to(Udec128_6::new(2_000));
    // volume = 3 + 2 = 5 (one side per fill)
    assert_that!(pp.volume).is_equal_to(Udec128_6::new(5));
    // volume_usd = 3*2000 + 2*2100 = 6000 + 4200 = 10200
    assert_that!(pp.volume_usd).is_equal_to(Udec128_6::new(10_200));

    let candle = query_candles(ch, CandleInterval::OneMinute)
        .await?
        .candles
        .into_iter()
        .next()
        .expect("Should have a 1-minute candle");

    assert_that!(candle.high).is_equal_to(Udec128_6::new(2_100));
    assert_that!(candle.low).is_equal_to(Udec128_6::new(2_000));
    assert_that!(candle.volume).is_equal_to(Udec128_6::new(5));
    assert_that!(candle.volume_usd).is_equal_to(Udec128_6::new(10_200));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn index_perps_candles_changing_prices() -> anyhow::Result<()> {
    // Default block time is 250ms, so all fills land in the same second/minute.
    let (mut suite, mut accounts, _, contracts, _, _, _, clickhouse_context, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 50_000).await;

    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 2_000, 1).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 1_999, 1).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 2_001, 1).await;

    suite.app.indexer.wait_for_finish().await?;

    let result = query_candles(
        clickhouse_context.clickhouse_client(),
        CandleInterval::OneMinute,
    )
    .await?;

    assert_that!(result.candles).has_length(1);
    let candle = &result.candles[0];

    // OHLC: open from first fill, close from last fill, high/low from extremes
    assert_that!(candle.open).is_equal_to(Udec128_6::new(2_000));
    assert_that!(candle.close).is_equal_to(Udec128_6::new(2_001));
    assert_that!(candle.high).is_equal_to(Udec128_6::new(2_001));
    assert_that!(candle.low).is_equal_to(Udec128_6::new(1_999));
    // 3 fills * 1 event (positive side) * 1 ETH = 3
    assert_that!(candle.volume).is_equal_to(Udec128_6::new(3));

    Ok(())
}

/// 20-second block time, fills spanning minute boundaries. Verifies candle
/// creation at interval boundaries with open-price inheritance.
#[tokio::test(flavor = "multi_thread")]
async fn index_perps_candles_across_minute_boundary() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, _, _, clickhouse_context, _db_guard) =
        setup_test_with_indexer_pp_and_custom_genesis(
            NaiveProposalPreparer,
            TestOption {
                block_time: Duration::from_seconds(20),
                genesis_block: BlockInfo {
                    height: 0,
                    timestamp: Timestamp::from_seconds(1),
                    hash: Hash256::ZERO,
                },
                ..Default::default()
            },
            dango_genesis::GenesisOption::preset_test(),
        )
        .await;

    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 50_000).await;

    suite.make_empty_block().await;

    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 2_000, 1).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 1_999, 1).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 2_001, 1).await;

    // Several empty blocks to force candle boundary crossing
    for _ in 0..5 {
        suite.make_empty_block().await;
    }

    suite.app.indexer.wait_for_finish().await?;

    let candles = query_candles(
        clickhouse_context.clickhouse_client(),
        CandleInterval::OneMinute,
    )
    .await?
    .candles;

    // Fills are spread across 120s–200s; empty blocks push past 240s+.
    assert_that!(candles.len()).is_at_least(2);

    assert_candle_continuity(&candles);

    // Oldest candle should contain the first fill
    assert_that!(candles.last().unwrap().volume).is_greater_than(Udec128_6::ZERO);

    // Total volume = 3 fills * 1 event (positive side) * 1 ETH = 3
    let total_volume: Udec128_6 = candles.iter().map(|c| c.volume).sum();
    assert_that!(total_volume).is_equal_to(Udec128_6::new(3));

    // Global high/low across all candles
    let global_high = candles.iter().map(|c| c.high).max().unwrap();
    let global_low = candles.iter().map(|c| c.low).min().unwrap();
    assert_that!(global_high).is_equal_to(Udec128_6::new(2_001));
    assert_that!(global_low).is_equal_to(Udec128_6::new(1_999));

    Ok(())
}

/// Many fills within the same minute, all aggregate into 1 candle.
#[tokio::test(flavor = "multi_thread")]
async fn index_perps_candles_many_fills_one_minute() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, _, _, clickhouse_context, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 50_000).await;

    for _ in 0..10 {
        create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 2_000, 1).await;
    }

    suite.app.indexer.wait_for_finish().await?;

    let ch = clickhouse_context.clickhouse_client();

    // Should have 10+ pair prices
    let perps_pps: Vec<_> = PerpsPairPrice::latest_prices(ch, 100)
        .await?
        .into_iter()
        .filter(|p| p.pair_id == pair_id().to_string())
        .collect();

    assert_that!(perps_pps.len()).is_at_least(5);

    let result = query_candles(ch, CandleInterval::OneMinute).await?;

    assert_that!(result.candles).has_length(1);
    assert_that!(result.has_next_page).is_false();
    assert_that!(result.has_previous_page).is_false();

    let candle = &result.candles[0];
    assert_that!(candle.open).is_equal_to(Udec128_6::new(2_000));
    assert_that!(candle.close).is_equal_to(Udec128_6::new(2_000));
    assert_that!(candle.high).is_equal_to(Udec128_6::new(2_000));
    assert_that!(candle.low).is_equal_to(Udec128_6::new(2_000));
    // 10 fills * 1 event (positive side) * 1 ETH = 10
    assert_that!(candle.volume).is_equal_to(Udec128_6::new(10));
    // 10 fills * 1 * 2000 = 20000
    assert_that!(candle.volume_usd).is_equal_to(Udec128_6::new(20_000));

    Ok(())
}

/// Reloading the cache from ClickHouse produces the same state as the live
/// in-memory cache.
#[tokio::test(flavor = "multi_thread")]
async fn index_perps_candles_cache_consistency() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, _, _, clickhouse_context, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 50_000).await;

    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 2_000, 1).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 2_000, 1).await;

    suite.app.indexer.wait_for_finish().await?;

    let ch = clickhouse_context.clickhouse_client();

    // Load a fresh cache from ClickHouse
    let mut fresh_cache = PerpsCandleCache::default();
    let pair_ids = PerpsPairPrice::all_pair_ids(ch).await?;
    fresh_cache.preload_pairs(&pair_ids, ch).await?;

    // Compare with the live in-memory cache
    let live_cache = clickhouse_context.perps_candle_cache.read().await;

    assert_eq!(
        fresh_cache.pair_prices,
        live_cache
            .pair_prices
            .clone()
            .into_iter()
            .filter(|pp| !pp.1.is_empty())
            .collect::<HashMap<_, _>>()
    );
    assert_eq!(fresh_cache.candles, live_cache.candles);

    Ok(())
}

/// One-second interval: 10 fills at 250ms block time span multiple seconds.
#[tokio::test(flavor = "multi_thread")]
async fn index_perps_candles_one_second_interval() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, _, _, clickhouse_context, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 50_000).await;

    for _ in 0..10 {
        create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 2_000, 1).await;
    }

    suite.app.indexer.wait_for_finish().await?;

    let candles = query_candles(
        clickhouse_context.clickhouse_client(),
        CandleInterval::OneSecond,
    )
    .await?
    .candles;

    // Multiple 1-second candles expected
    assert_that!(candles.len()).is_at_least(2);

    assert_candle_continuity(&candles);

    // Total volume across all candles = 10 fills * 1 event (positive side) * 1 ETH = 10
    let total_volume: Udec128_6 = candles.iter().map(|c| c.volume).sum();
    assert_that!(total_volume).is_equal_to(Udec128_6::new(10));

    Ok(())
}

/// Full 10-minute timeline with varying prices.
///
/// Setup: block_time=10s, genesis at t=0.
///   - Blocks 1–3 (10–30s): oracle + deposits
///   - 30 fills (blocks 4–63): fill i lands at t = 50 + i*20 seconds
///   - 10 empty blocks (blocks 64–73) to push past another 5-min boundary
///
/// Fill schedule (each fill = limit_ask block + market_buy block = 20s):
///   Fill 0 at  50s (min 0), Fill 1 at  70s (min 1), …, Fill 29 at 630s (min 10)
///
/// Minute allocation (~3 fills per minute):
///   min 0: 1 fill,  min 1–10: 3 fills each,  min 10: 2 fills
///
/// 5-minute buckets:
///   0–300s:   fills  0–12 (13 fills)
///   300–600s: fills 13–27 (15 fills)
///   600+:     fills 28–29 ( 2 fills)
///
/// 1-second candles: only at block timestamps. Between fills, the limit-ask
/// block (which carries no OrderFilled) produces a **gap candle** with zero
/// volume and flat OHLC inherited from the previous close.
#[tokio::test(flavor = "multi_thread")]
async fn index_perps_candles_full_timeline() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, _, _, clickhouse_context, _db_guard) =
        setup_test_with_indexer_pp_and_custom_genesis(
            NaiveProposalPreparer,
            TestOption {
                block_time: Duration::from_seconds(10),
                genesis_block: BlockInfo {
                    height: 0,
                    timestamp: Timestamp::from_seconds(0),
                    hash: Hash256::ZERO,
                },
                ..Default::default()
            },
            dango_genesis::GenesisOption::preset_test(),
        )
        .await;

    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 50_000).await;

    // 30 fills with varying prices in the $1990–$2020 range.
    // Capped at 14 due to a deadlock in the block processing pipeline.
    // See: https://github.com/left-curve/left-curve/issues/1635
    const NUM_FILLS: usize = 14;
    let offsets: [i128; 10] = [-10, 5, -5, 10, -3, 8, -8, 3, -1, 7];
    let prices: Vec<u128> = (0..NUM_FILLS)
        .map(|i| (2000 + offsets[i % 10]) as u128)
        .collect();

    let expected_high = *prices.iter().max().unwrap();
    let expected_low = *prices.iter().min().unwrap();

    for &price in &prices {
        create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), price, 1).await;
    }

    // Push past another 5-minute boundary so we get an extra empty candle.
    for _ in 0..10 {
        suite.make_empty_block().await;
    }

    suite.app.indexer.wait_for_finish().await?;

    let ch = clickhouse_context.clickhouse_client();

    // Only one side (positive fill_size) is counted per fill.
    let expected_total_volume = Udec128_6::new(NUM_FILLS as u128);

    // =====================================================================
    //  1-SECOND CANDLES
    // =====================================================================
    let candles_1s = query_candles(ch, CandleInterval::OneSecond).await?.candles;

    assert_candle_continuity(&candles_1s);

    // Candles with actual fills vs gap candles (from limit-ask / empty blocks).
    let filled_1s: Vec<_> = candles_1s
        .iter()
        .filter(|c| c.volume > Udec128_6::ZERO)
        .collect();
    let gaps_1s: Vec<_> = candles_1s
        .iter()
        .filter(|c| c.volume == Udec128_6::ZERO)
        .collect();

    // At least NUM_FILLS candles should carry volume.
    assert_that!(filled_1s.len()).is_at_least(NUM_FILLS);

    // Gap candles (if any) must have zero volume_usd.
    for gap in &gaps_1s {
        assert_eq!(
            gap.volume_usd,
            Udec128_6::ZERO,
            "Gap candle has non-zero volume_usd"
        );
    }

    // Total volume across 1s candles equals expected.
    let total_1s: Udec128_6 = candles_1s.iter().map(|c| c.volume).sum();
    assert_that!(total_1s).is_equal_to(expected_total_volume);

    // =====================================================================
    //  1-MINUTE CANDLES
    // =====================================================================
    let candles_1m = query_candles(ch, CandleInterval::OneMinute).await?.candles;

    // Each fill takes 20s → ~3 fills per minute. We need at least a few 1m candles.
    assert_that!(candles_1m.len()).is_at_least(2);
    assert_candle_continuity(&candles_1m);

    let total_1m: Udec128_6 = candles_1m.iter().map(|c| c.volume).sum();
    assert_that!(total_1m).is_equal_to(expected_total_volume);

    // =====================================================================
    //  5-MINUTE CANDLES
    // =====================================================================
    let candles_5m = query_candles(ch, CandleInterval::FiveMinutes)
        .await?
        .candles;

    assert_that!(candles_5m.len()).is_at_least(1);
    assert_candle_continuity(&candles_5m);

    let total_5m: Udec128_6 = candles_5m.iter().map(|c| c.volume).sum();
    assert_that!(total_5m).is_equal_to(expected_total_volume);

    // =====================================================================
    //  CROSS-RESOLUTION CONSISTENCY
    // =====================================================================

    // Global high / low across filled candles must agree at every resolution.
    let high_1s = filled_1s.iter().map(|c| c.high).max().unwrap();
    let high_1m = candles_1m.iter().map(|c| c.high).max().unwrap();
    let high_5m = candles_5m.iter().map(|c| c.high).max().unwrap();
    assert_eq!(high_1s, high_1m, "1s vs 1m global high mismatch");
    assert_eq!(high_1m, high_5m, "1m vs 5m global high mismatch");
    assert_eq!(high_1s, Udec128_6::new(expected_high));

    let low_1s = filled_1s.iter().map(|c| c.low).min().unwrap();
    let low_1m = candles_1m
        .iter()
        .filter(|c| c.volume > Udec128_6::ZERO)
        .map(|c| c.low)
        .min()
        .unwrap();
    let low_5m = candles_5m
        .iter()
        .filter(|c| c.volume > Udec128_6::ZERO)
        .map(|c| c.low)
        .min()
        .unwrap();
    assert_eq!(low_1s, low_1m, "1s vs 1m global low mismatch");
    assert_eq!(low_1m, low_5m, "1m vs 5m global low mismatch");
    assert_eq!(low_1s, Udec128_6::new(expected_low));

    Ok(())
}

/// Two pairs (ETH and BTC) produce independent candles.
#[tokio::test(flavor = "multi_thread")]
async fn index_perps_candles_multi_pair() -> anyhow::Result<()> {
    let (mut suite, mut accounts, _, contracts, _, _, _, clickhouse_context, _db_guard) =
        setup_test_naive_with_indexer(TestOption::default()).await;

    let eth_pair = pair_id(); // perp/ethusd
    let btc_pair: Denom = "perp/btcusd".parse().unwrap();

    // Register oracle prices for both pairs.
    suite
        .seed_oracle_prices(
            &mut accounts.owner,
            btree_map! {
                usdc::DENOM.clone() => OracleTestEntry {
                    pyth_id: 1,
                    humanized_price: UsdPrice::new_int(1),
                },
                eth_pair.clone() => OracleTestEntry {
                    pyth_id: 2,
                    humanized_price: UsdPrice::new_int(2_000),
                },
                btc_pair.clone() => OracleTestEntry {
                    pyth_id: 3,
                    humanized_price: UsdPrice::new_int(60_000),
                },
            },
        )
        .await;

    // Register the BTC pair via MaintainerMsg::Configure (ETH pair already
    // exists from genesis; re-specifying it keeps it unchanged).
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: perps::Param {
                    taker_fee_rates: RateSchedule {
                        base: Dimensionless::new_permille(1),
                        ..Default::default()
                    },
                    protocol_fee_rate: Dimensionless::ZERO,
                    liquidation_fee_rate: Dimensionless::new_permille(10),
                    vault_cooldown_period: Duration::from_days(1),
                    max_unlocks: 10,
                    max_open_orders: 100,
                    funding_period: Duration::from_hours(1),
                    max_action_batch_size: 5,
                    ..Default::default()
                },
                pair_params: btree_map! {
                    eth_pair.clone() => PairParam {
                        initial_margin_ratio: Dimensionless::new_permille(100),
                        maintenance_margin_ratio: Dimensionless::new_permille(50),
                        tick_size: UsdPrice::new_int(1),
                        max_abs_oi: Quantity::new_int(1_000_000),
                        ..PairParam::new_mock()
                    },
                    btc_pair.clone() => PairParam {
                        initial_margin_ratio: Dimensionless::new_permille(100),
                        maintenance_margin_ratio: Dimensionless::new_permille(50),
                        tick_size: UsdPrice::new_int(1),
                        max_abs_oi: Quantity::new_int(1_000_000),
                        ..PairParam::new_mock()
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Deposit margin for both users.
    for account in [&mut accounts.user1, &mut accounts.user2] {
        suite
            .execute(
                account,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
                Coins::one(usdc::DENOM.clone(), Uint128::new(100_000 * 1_000_000)).unwrap(),
            )
            .await
            .should_succeed();
    }

    // Create fills on both pairs.
    create_perps_fill(&mut suite, &mut accounts, &contracts, &eth_pair, 2_000, 3).await;
    create_perps_fill(&mut suite, &mut accounts, &contracts, &btc_pair, 60_000, 1).await;

    suite.app.indexer.wait_for_finish().await?;

    let ch = clickhouse_context.clickhouse_client();

    // Query 1-minute candles for each pair independently.
    let eth_candles = PerpsCandleQueryBuilder::new(CandleInterval::OneMinute, eth_pair.to_string())
        .fetch_all(ch)
        .await?
        .candles;

    let btc_candles = PerpsCandleQueryBuilder::new(CandleInterval::OneMinute, btc_pair.to_string())
        .fetch_all(ch)
        .await?
        .candles;

    assert_that!(eth_candles.len()).is_at_least(1);
    assert_that!(btc_candles.len()).is_at_least(1);

    let eth_candle = &eth_candles[0];
    let btc_candle = &btc_candles[0];

    // ETH candle: pair_id, price at 2000, volume = 3 (one side per fill)
    assert_that!(eth_candle.pair_id.as_str()).is_equal_to(eth_pair.to_string().as_str());
    assert_that!(eth_candle.close).is_equal_to(Udec128_6::new(2_000));
    assert_that!(eth_candle.volume).is_equal_to(Udec128_6::new(3));
    // volume_usd = 3 * 2000 = 6000
    assert_that!(eth_candle.volume_usd).is_equal_to(Udec128_6::new(6_000));

    // BTC candle: pair_id, price at 60000, volume = 1 (one side per fill)
    assert_that!(btc_candle.pair_id.as_str()).is_equal_to(btc_pair.to_string().as_str());
    assert_that!(btc_candle.close).is_equal_to(Udec128_6::new(60_000));
    assert_that!(btc_candle.volume).is_equal_to(Udec128_6::new(1));
    // volume_usd = 1 * 60000 = 60000
    assert_that!(btc_candle.volume_usd).is_equal_to(Udec128_6::new(60_000));

    Ok(())
}

/// Regression test: `preload_pairs` must rebuild the in-progress candle
/// from `perps_pair_prices` in ClickHouse end-to-end. A prior bug in
/// `PerpsPairPrice::since` bound `timestamp_micros()` into
/// `toDateTime64(?, 6)`; ClickHouse treats integer inputs as seconds
/// regardless of scale, so the bind saturated to the DateTime64 max
/// (year 2299) and the predicate silently matched nothing — the rebuild
/// ran on an empty prices vec (`replayed=0`) and the in-progress candle
/// was lost on every restart.
///
/// The test uses a recent genesis timestamp so that block `created_at`
/// values fall inside the current `earliest_current_bucket_start`
/// window; with the 1970-based genesis used by the other tests the
/// `since` filter would drop every row anyway, masking the regression.
#[tokio::test(flavor = "multi_thread")]
async fn index_perps_candles_preload_rebuilds_current_bucket_from_clickhouse() -> anyhow::Result<()>
{
    // One hour ago — comfortably inside every current-bucket window,
    // including the 1d and 1h intervals exercised below.
    let genesis_secs = chrono::Utc::now().timestamp() as u128 - 3_600;

    let (mut suite, mut accounts, _, contracts, _, _, _, clickhouse_context, _db_guard) =
        setup_test_with_indexer_pp_and_custom_genesis(
            NaiveProposalPreparer,
            TestOption {
                genesis_block: BlockInfo {
                    height: 0,
                    timestamp: Timestamp::from_seconds(genesis_secs),
                    hash: Hash256::ZERO,
                },
                ..Default::default()
            },
            dango_genesis::GenesisOption::preset_test(),
        )
        .await;

    setup_perps_env(&mut suite, &mut accounts, &contracts, 2_000, 50_000).await;

    for _ in 0..5 {
        create_perps_fill(&mut suite, &mut accounts, &contracts, &pair_id(), 2_000, 1).await;
    }

    suite.app.indexer.wait_for_finish().await?;

    let ch = clickhouse_context.clickhouse_client();
    let pair_ids = PerpsPairPrice::all_pair_ids(ch).await?;
    assert_that!(pair_ids.is_empty()).is_false();

    // Exactly the path `Context::preload_cache` takes at node startup.
    let mut fresh_cache = PerpsCandleCache::default();
    fresh_cache.preload_pairs(&pair_ids, ch).await?;

    // With the broken `since` binding the 1d in-progress candle was never
    // rebuilt: `replayed=0`, cache empty for the current bucket, and
    // `get_last_candle` would return `None` (or a stale candle from a
    // previous bucket). Assert the rebuild actually populated it.
    for interval in [
        CandleInterval::OneHour,
        CandleInterval::FourHours,
        CandleInterval::OneDay,
    ] {
        let key = PerpsCandleCacheKey::new(pair_id().to_string(), interval);
        let candle = fresh_cache
            .get_last_candle(&key)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "preload_pairs produced no {interval} candle for {}",
                    pair_id()
                )
            });

        // 5 fills * 1 event (positive side) * 1 ETH each.
        assert_eq!(
            candle.volume,
            Udec128_6::new(5),
            "{interval} candle volume mismatch after rebuild",
        );
        assert_eq!(
            candle.volume_usd,
            Udec128_6::new(10_000),
            "{interval} candle volume_usd mismatch after rebuild",
        );
    }

    Ok(())
}
