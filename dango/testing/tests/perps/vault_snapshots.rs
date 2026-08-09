use {
    crate::register_oracle_prices,
    dango_math::{NumberConst, Uint128},
    dango_order_book::{UsdValue, round_to_day},
    dango_primitives::{Coins, Duration, QuerierExt, ResultExt, Timestamp},
    dango_testing::{TestOption, setup_test_naive},
    dango_types::{
        constants::usdc,
        perps::{self, VaultSnapshot},
    },
    std::collections::BTreeMap,
};

/// Verifies that the perps cron writes one `(equity, share_supply)` snapshot
/// per day and the `VaultSnapshots` query returns them in ascending order with
/// inclusive bounds.
///
/// Note: the perps cron is scheduled with a 1-minute interval (see
/// `dango/exchange/genesis/src/builder.rs`), so back-to-back tx blocks (250ms apart)
/// do not trigger it. We rely on `increase_time(1 day)` to push past the
/// scheduling boundary and produce one snapshot per call.
#[tokio::test]
async fn vault_snapshots_accrue_daily() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // LP deposits collateral and adds liquidity. None of these short-interval
    // blocks trigger the perps cron (interval = 1 min, block time = 250ms),
    // so no snapshots have been written yet.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Vault(perps::VaultMsg::AddLiquidity {
                amount: UsdValue::new_int(5_000),
                min_shares_to_mint: None,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let snapshots: BTreeMap<Timestamp, VaultSnapshot> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryVaultSnapshotsRequest {
                min: None,
                max: None,
            },
        )
        .should_succeed();

    assert!(
        snapshots.is_empty(),
        "no snapshot should be written before the first cron tick fires"
    );

    // ---------------------------------------------------------------------
    // Cross day 1: cron catches up, writes one snapshot reflecting LP
    // deposit ($5,000 equity, share_supply > 0).
    // ---------------------------------------------------------------------

    suite.increase_time(Duration::from_days(1)).await;
    let day_1 = round_to_day(suite.block.timestamp);

    let snapshots: BTreeMap<Timestamp, VaultSnapshot> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryVaultSnapshotsRequest {
                min: None,
                max: None,
            },
        )
        .should_succeed();

    assert_eq!(snapshots.len(), 1);

    let day_1_snapshot = snapshots
        .get(&day_1)
        .expect("a snapshot must exist for day_1");

    assert_eq!(
        day_1_snapshot.equity,
        UsdValue::new_int(5_000),
        "vault equity = $5,000 after AddLiquidity"
    );

    assert!(
        day_1_snapshot.share_supply > Uint128::ZERO,
        "share supply must be positive after AddLiquidity"
    );

    // ---------------------------------------------------------------------
    // Cross day 2 and day 3: one snapshot each.
    // ---------------------------------------------------------------------

    suite.increase_time(Duration::from_days(1)).await;
    let day_2 = round_to_day(suite.block.timestamp);
    assert_eq!(day_2, day_1 + Duration::from_days(1));

    suite.increase_time(Duration::from_days(1)).await;
    let day_3 = round_to_day(suite.block.timestamp);
    assert_eq!(day_3, day_2 + Duration::from_days(1));

    let snapshots: BTreeMap<Timestamp, VaultSnapshot> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryVaultSnapshotsRequest {
                min: None,
                max: None,
            },
        )
        .should_succeed();
    let keys: Vec<_> = snapshots.keys().copied().collect();
    assert_eq!(keys, vec![day_1, day_2, day_3]);

    // ---------------------------------------------------------------------
    // Bounded queries — both bounds inclusive.
    // ---------------------------------------------------------------------

    // `min == max == day_2` → one entry.
    let snapshots: BTreeMap<Timestamp, VaultSnapshot> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryVaultSnapshotsRequest {
                min: Some(day_2),
                max: Some(day_2),
            },
        )
        .should_succeed();
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots.contains_key(&day_2));

    // Range covering day_2..=day_3 → two entries.
    let snapshots: BTreeMap<Timestamp, VaultSnapshot> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryVaultSnapshotsRequest {
                min: Some(day_2),
                max: Some(day_3),
            },
        )
        .should_succeed();
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.contains_key(&day_2));
    assert!(snapshots.contains_key(&day_3));
}
