use {
    dango_order_book::{Dimensionless, UsdPrice},
    dango_primitives::{Coins, Denom, Duration, QuerierExt, ResultExt, Timestamp, btree_map},
    dango_pyth_types::{Channel, MarketSession},
    dango_testing::setup_test_naive,
    dango_types::oracle::{
        ExecuteMsg, Fixing, PriceConfig, PriceSource, QueryPriceRequest, RollState,
    },
    std::str::FromStr,
};

fn source(id: u32) -> PriceSource {
    PriceSource {
        id,
        channel: Channel::RealTime,
    }
}

/// A futures-roll denom blends `current` and `next` by the weight in force at
/// the block timestamp: 100% `current` before the first fixing, the weighted
/// mean in between, 100% `next` after the last fixing.
#[tokio::test]
async fn roll_price_blends_over_the_schedule() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());
    let oracle = contracts.oracle;
    suite.block_time = Duration::from_seconds(1);

    let denom = Denom::from_str("perp/oil").unwrap();
    let t0 = suite.block.timestamp;

    // 100% current until t0+100, 40% on next from t0+100, 100% next from t0+200.
    suite
        .execute(
            &mut accounts.owner,
            oracle,
            &ExecuteMsg::RegisterPriceSources(btree_map! {
               denom.clone() => PriceConfig::Roll(RollState {
                   current: source(9001),
                   next: source(9002),
                   fixings: vec![
                       Fixing {
                           at: t0 + Duration::from_seconds(100),
                           next_weight: Dimensionless::new_percent(40),
                       },
                       Fixing {
                           at: t0 + Duration::from_seconds(200),
                           next_weight: Dimensionless::ONE,
                       },
                   ],
               }),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Feed both contracts: current = $100, next = $200.
    suite
        .do_oracle_actions(
            &mut accounts.owner,
            None,
            Some((
                &[
                    (9001, UsdPrice::new_int(100), MarketSession::Regular),
                    (9002, UsdPrice::new_int(200), MarketSession::Regular),
                ],
                Timestamp::MAX,
            )),
            false,
            false,
        )
        .await;

    // Before the first fixing: 100% current = $100.
    let price = suite
        .query_wasm_smart(
            oracle,
            QueryPriceRequest {
                denom: denom.clone(),
            },
        )
        .should_succeed();
    assert_eq!(price.humanized_price, UsdPrice::new_int(100));

    // Into the 40% window (~t0+152): 100*0.6 + 200*0.4 = $140.
    suite.increase_time(Duration::from_seconds(150)).await;
    let price = suite
        .query_wasm_smart(
            oracle,
            QueryPriceRequest {
                denom: denom.clone(),
            },
        )
        .should_succeed();
    assert_eq!(price.humanized_price, UsdPrice::new_int(140));

    // After the last fixing (~t0+252): 100% next = $200.
    suite.increase_time(Duration::from_seconds(100)).await;
    let price = suite
        .query_wasm_smart(
            oracle,
            QueryPriceRequest {
                denom: denom.clone(),
            },
        )
        .should_succeed();
    assert_eq!(price.humanized_price, UsdPrice::new_int(200));
}
