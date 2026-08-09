use {
    crate::{
        core::{compute_maintenance_margin, compute_user_equity},
        querier::NoCachePerpQuerier,
    },
    dango_math::MathResult,
    dango_order_book::{PairId, Quantity, UsdPrice, UsdValue},
    dango_types::perps::{PairParam, UserState},
    std::collections::BTreeMap,
};

/// Returns true if the user is eligible for liquidation.
/// Also returns equity and maintenance margin for error logging purpose.
///
/// A user is liquidatable when their equity (collateral + unrealized PnL
/// - accrued funding) falls below their total maintenance margin.
///
/// A user with no open positions is never liquidatable.
pub fn is_liquidatable(
    perp_querier: &NoCachePerpQuerier,
    user_state: &UserState,
) -> anyhow::Result<(bool, UsdValue, UsdValue)> {
    let equity = compute_user_equity(perp_querier, user_state)?;
    let maintenance_margin = compute_maintenance_margin(perp_querier, user_state)?;

    Ok((equity < maintenance_margin, equity, maintenance_margin))
}

/// A policy for selecting which position(s) to close during liquidation.
/// We start from the position that contributes the most to maintenance margin
/// and go down, until the maintenance margin deficit is covered.
///
/// ## Returns
///
/// - A vector of (pair_id, close_size) tuples.
///
/// ## Notes
///
/// **Dust snapping.** Within each per-pair step, after the deficit-driven
/// `close_amount` is, if the *remaining* position notional after the close
/// would fall below the pair's `min_order_size`, we bump `close_amount` up
/// to the full position size.
///
/// This prevents liquidations from leaving behind sub-economic positions that
/// the liquidator bot would otherwise have to keep tracking and re-liquidating.
/// When `min_order_size == 0` (the default for tests and any pair without a
/// configured floor) the snap is a no-op.
pub fn compute_close_schedule(
    user_state: &UserState,
    pair_params: &BTreeMap<PairId, PairParam>,
    oracle_prices: &BTreeMap<PairId, UsdPrice>,
    deficit: UsdValue,
) -> MathResult<Vec<(PairId, Quantity)>> {
    let mut deficit = deficit;

    // Build (mm_contribution, pair_id) list, sorted descending by MM.
    let mut mm_entries = Vec::new();

    for (pair_id, position) in &user_state.positions {
        let oracle_price = oracle_prices[pair_id];
        let pair_param = &pair_params[pair_id];

        let mm_contribution = position
            .size
            .checked_abs()?
            .checked_mul(oracle_price)?
            .checked_mul(pair_param.maintenance_margin_ratio)?;

        mm_entries.push((mm_contribution, pair_id.clone()));
    }

    // Sort by MM contribution descending.
    mm_entries.sort_by_key(|(a, _)| std::cmp::Reverse(*a));

    // Build the close schedule.
    let mut schedule = Vec::new();

    for (_, pair_id) in &mm_entries {
        if deficit <= UsdValue::ZERO {
            break;
        }

        let position = &user_state.positions[pair_id];
        let oracle_price = oracle_prices[pair_id];
        let pair_param = &pair_params[pair_id];
        let abs_size = position.size.checked_abs()?;

        // close_amount = min(ceil(deficit / (P × mmr)), |size|)
        //
        // Ceiling rounding is load-bearing: with floor division, a sub-ULP
        // deficit collapses `close_amount` to zero, leaves the schedule
        // empty, and causes `liquidate` to silently exit with no events.
        // Ceil guarantees at least 1 ULP of progress whenever `deficit > 0`.
        let mut close_amount = {
            let denominator = oracle_price.checked_mul(pair_param.maintenance_margin_ratio)?;
            deficit.checked_div_ceil(denominator)?.min(abs_size)
        };

        // Dust snap: if the post-close remaining notional would fall below
        // the pair's `min_order_size`, close the entire position.
        //
        // Without this, partial liquidations near a position's tail can
        // leave behind a sub-economic remainder (e.g. 0.000001 BTC after a
        // 99.999999 BTC close). The user has no incentive to close it, but
        // the liquidator bot must keep tracking it for re-liquidation. By
        // snapping up here we ensure every position that survives a
        // liquidation has notional >= `min_order_size`.
        //
        // Strict `<` so a remainder exactly at the threshold is kept. When
        // `min_order_size == 0` the comparison is always false and the
        // snap is a no-op — the deficit-only behavior is preserved for
        // pairs that haven't opted in to a dust floor.
        //
        // The increased `close_amount` flows into the deficit decrement
        // below, so an over-snap naturally trips the `deficit <= 0` break
        // on subsequent iterations and we don't touch later pairs
        // unnecessarily.
        let remaining_amount = abs_size.checked_sub(close_amount)?;
        let remaining_notional = remaining_amount.checked_mul(oracle_price)?;
        if remaining_notional < pair_param.min_order_size {
            close_amount = abs_size;
        }

        // close_size = -sign(size) × close_amount
        //
        // Adjust the sign of `close_amount`. It should have the opposite sign
        // of the current position.
        let close_size = if position.size.is_positive() {
            close_amount.checked_neg()?
        } else {
            close_amount
        };

        deficit = {
            let mm_to_remove = close_amount
                .checked_mul(oracle_price)?
                .checked_mul(pair_param.maintenance_margin_ratio)?;
            deficit.checked_sub(mm_to_remove)?.max(UsdValue::ZERO)
        };

        if close_size.is_non_zero() {
            schedule.push((pair_id.clone(), close_size));
        }

        // If the full position is being closed, deficit should be exactly cleared for
        // that contribution. But if we're doing partial, deficit might still be > 0
        // only if there are precision issues. The guard at the top handles this.
    }

    Ok(schedule)
}

/// Compute the user's equity from in-memory state and accumulated PnL/fees.
///
/// This is the "raw" equity computation used during liquidation, where oracle
/// prices are already resolved and funding has been settled into `user_pnl`.
///
/// ```plain
/// equity = margin + user_pnl - user_fees + Σ(size × (oracle - entry))
/// ```
pub fn compute_user_equity_with_pnl(
    user_state: &UserState,
    oracle_prices: &BTreeMap<PairId, UsdPrice>,
    user_pnl: UsdValue,
    user_fees: UsdValue,
) -> MathResult<UsdValue> {
    let mut equity = user_state
        .margin
        .checked_add(user_pnl)?
        .checked_sub(user_fees)?;

    for (pid, pos) in &user_state.positions {
        let oracle_price = oracle_prices[pid];
        let unrealized = pos
            .size
            .checked_mul(oracle_price.checked_sub(pos.entry_price)?)?;

        equity.checked_add_assign(unrealized)?;
    }

    Ok(equity)
}

/// Compute the bankruptcy price of a position during liquidation.
///
/// This is the fill price at which the user's total equity would be exactly
/// zero if the **entire** position were closed at it:
///
/// For longs:  bp = oracle_price - equity / |size|
/// For shorts: bp = oracle_price + equity / |size|
///
/// The divisor is always the full current position size, regardless of how
/// much of the position the close schedule intends to close. A partial close
/// at this price concedes `equity / |size|` per unit closed, so equity after
/// closing `c` units is `equity × (1 - c / |size|)` — non-negative whenever
/// equity is non-negative, and exactly zero when the whole position is
/// closed.
///
/// For a single-position account the offset is bounded: liquidatable means
/// `equity < |size| × oracle × mmr`, hence `equity / |size| < oracle × mmr`
/// and the bankruptcy price stays within the maintenance-margin band of the
/// oracle.
pub fn compute_bankruptcy_price(
    user_state: &UserState,
    pair_id: &PairId,
    oracle_prices: &BTreeMap<PairId, UsdPrice>,
    user_pnl: UsdValue,
    user_fees: UsdValue,
) -> MathResult<UsdPrice> {
    let equity = compute_user_equity_with_pnl(user_state, oracle_prices, user_pnl, user_fees)?;

    let position = &user_state.positions[pair_id];
    let oracle_price = oracle_prices[pair_id];
    let offset = equity.checked_div(position.size.checked_abs()?)?;

    if position.size.is_positive() {
        oracle_price.checked_sub(offset)
    } else {
        oracle_price.checked_add(offset)
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_order_book::{Dimensionless, FundingPerUnit, Quantity, UsdPrice, UsdValue},
        dango_primitives::{btree_map, hash_map},
        dango_types::{
            constants::eth,
            perps::{PairParam, PairState, Position},
        },
    };

    fn pair_btc() -> PairId {
        "perp/btcusd".parse().unwrap()
    }

    fn pair_eth() -> PairId {
        "perp/ethusd".parse().unwrap()
    }

    fn btc_pair_param() -> PairParam {
        PairParam {
            maintenance_margin_ratio: Dimensionless::new_permille(50), // 5%
            ..Default::default()
        }
    }

    fn eth_pair_param() -> PairParam {
        PairParam {
            maintenance_margin_ratio: Dimensionless::new_permille(50), // 5%
            ..Default::default()
        }
    }

    // ------------------------ `is_liquidatable` tests ------------------------

    #[test]
    fn is_liquidatable_no_positions() {
        let user_state = UserState {
            margin: UsdValue::new_int(10_000),
            ..Default::default()
        };
        let perp_querier = NoCachePerpQuerier::new_mock(Default::default(), Default::default());

        assert!(
            !is_liquidatable(&perp_querier, &user_state)
                .map(|(is, ..)| is)
                .unwrap()
        );
    }

    // collateral=10000, ETH long 10 @ entry=2000, oracle=2500, mmr=5%
    // equity = 10000 + 10*(2500-2000) = 15000
    // maint  = |10| * 2500 * 0.05 = 1250
    // 15000 >= 1250 → not liquidatable
    #[test]
    fn is_liquidatable_healthy() {
        let user_state = UserState {
            margin: UsdValue::new_int(10_000),
            positions: btree_map! {
                eth::DENOM.clone() => Position {
                    size: Quantity::new_int(10),
                    entry_price: UsdPrice::new_int(2000),
                    entry_funding_per_unit: FundingPerUnit::new_int(0),
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };
        let perp_querier = NoCachePerpQuerier::new_mock(
            hash_map! {
                eth::DENOM.clone() => PairParam {
                    maintenance_margin_ratio: Dimensionless::new_permille(50),
                    ..Default::default()
                },
            },
            hash_map! {
                eth::DENOM.clone() => PairState {
                    funding_per_unit: FundingPerUnit::new_int(0),
                    index_price: UsdPrice::new_percent(250_000),
                    ..Default::default()
                },
            },
        );

        assert!(
            !is_liquidatable(&perp_querier, &user_state)
                .map(|(is, ..)| is)
                .unwrap()
        );
    }

    // equity exactly equals maintenance margin → not liquidatable (strict <)
    // Need: equity = maint. maint = |10| * 2000 * 0.05 = 1000
    // equity = collateral + pnl - funding = collateral + 0 - 0 = collateral
    // So collateral = 1000
    #[test]
    fn is_liquidatable_at_boundary() {
        let user_state = UserState {
            margin: UsdValue::new_int(1_000),
            positions: btree_map! {
                eth::DENOM.clone() => Position {
                    size: Quantity::new_int(10),
                    entry_price: UsdPrice::new_int(2000),
                    entry_funding_per_unit: FundingPerUnit::new_int(0),
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };
        let perp_querier = NoCachePerpQuerier::new_mock(
            hash_map! {
                eth::DENOM.clone() => PairParam {
                    maintenance_margin_ratio: Dimensionless::new_permille(50),
                    ..Default::default()
                },
            },
            hash_map! {
                eth::DENOM.clone() => PairState {
                    funding_per_unit: FundingPerUnit::new_int(0),
                    index_price: UsdPrice::new_percent(200_000),
                    ..Default::default()
                },
            },
        );

        assert!(
            !is_liquidatable(&perp_querier, &user_state)
                .map(|(is, ..)| is)
                .unwrap()
        );
    }

    // collateral=100, ETH long 10 @ entry=2000, oracle=1500, mmr=5%
    // equity = 100 + 10*(1500-2000) = 100 - 5000 = -4900
    // maint  = |10| * 1500 * 0.05 = 750
    // -4900 < 750 → liquidatable
    #[test]
    fn is_liquidatable_underwater() {
        let user_state = UserState {
            margin: UsdValue::new_int(100),
            positions: btree_map! {
                eth::DENOM.clone() => Position {
                    size: Quantity::new_int(10),
                    entry_price: UsdPrice::new_int(2000),
                    entry_funding_per_unit: FundingPerUnit::new_int(0),
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };
        let perp_querier = NoCachePerpQuerier::new_mock(
            hash_map! {
                eth::DENOM.clone() => PairParam {
                    maintenance_margin_ratio: Dimensionless::new_permille(50),
                    ..Default::default()
                },
            },
            hash_map! {
                eth::DENOM.clone() => PairState {
                    funding_per_unit: FundingPerUnit::new_int(0),
                    index_price: UsdPrice::new_percent(150_000),
                    ..Default::default()
                },
            },
        );

        assert!(
            is_liquidatable(&perp_querier, &user_state)
                .map(|(is, ..)| is)
                .unwrap()
        );
    }

    // Funding can push a user into liquidation territory.
    //
    // Setup: ETH long 10 @ entry=2000, oracle=2000 (no pnl), mmr=5%
    //   funding_per_unit=100, entry=0 → accrued = 10 * 100 = 1000
    //   maint = |10| * 2000 * 0.05 = 1000
    //
    // Case 1: collateral=10000
    //   equity = 10000 + 0 - 1000 = 9000, maint=1000 → not liquidatable
    //
    // Case 2: collateral=900
    //   equity = 900 + 0 - 1000 = -100, maint=1000 → liquidatable
    #[test]
    fn is_liquidatable_funding_pushes_under() {
        let make_fixtures = |collateral: i128| {
            let user_state = UserState {
                margin: UsdValue::new_int(collateral),
                positions: btree_map! {
                    eth::DENOM.clone() => Position {
                        size: Quantity::new_int(10),
                        entry_price: UsdPrice::new_int(2000),
                        entry_funding_per_unit: FundingPerUnit::new_int(0),
                        conditional_order_above: None,
                        conditional_order_below: None,
                    },
                },
                ..Default::default()
            };
            let perp_querier = NoCachePerpQuerier::new_mock(
                hash_map! {
                    eth::DENOM.clone() => PairParam {
                        maintenance_margin_ratio: Dimensionless::new_permille(50),
                        ..Default::default()
                    },
                },
                hash_map! {
                    eth::DENOM.clone() => PairState {
                        funding_per_unit: FundingPerUnit::new_int(100),
                        index_price: UsdPrice::new_percent(200_000),
                        ..Default::default()
                    },
                },
            );
            (user_state, perp_querier)
        };

        // Case 1: healthy despite funding
        let (us, pq) = make_fixtures(10_000);
        assert!(!is_liquidatable(&pq, &us).map(|(is, ..)| is).unwrap());

        // Case 2: funding pushes equity below maintenance margin
        let (us, pq) = make_fixtures(900);
        assert!(is_liquidatable(&pq, &us).map(|(is, ..)| is).unwrap());
    }

    // -------------------- `compute_close_schedule` tests ---------------------

    /// Single long position, deficit exceeds its MM → full close.
    ///
    /// Position: long 10 BTC @ oracle $47,000, MMR = 5%
    /// MM = 10 * 47000 * 0.05 = 23,500
    /// deficit = 30,000 > MM → close all 10 BTC
    #[test]
    fn single_pair_full_close() {
        let user_state = UserState {
            positions: btree_map! {
                pair_btc() => Position {
                    size: Quantity::new_int(10),
                    entry_price: UsdPrice::new_int(50_000),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let pair_params = btree_map! { pair_btc() => btc_pair_param() };
        let oracle_prices = btree_map! { pair_btc() => UsdPrice::new_int(47_000) };

        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_int(30_000),
        )
        .unwrap();

        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule[0].0, pair_btc());
        // Closing a long → negative close_size
        assert_eq!(schedule[0].1, Quantity::new_int(-10));
    }

    /// Two pairs, BTC has larger MM → processed first, both fully closed.
    ///
    /// BTC: long 1 @ oracle $47,000, MMR 5% → MM = 2,350
    /// ETH: long 10 @ oracle $2,800, MMR 5% → MM = 1,400
    /// Total MM = 3,750
    /// deficit = 4,750 > total → close both, BTC first
    #[test]
    fn multi_pair_largest_mm_first() {
        let user_state = UserState {
            positions: btree_map! {
                pair_btc() => Position {
                    size: Quantity::new_int(1),
                    entry_price: UsdPrice::new_int(50_000),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
                pair_eth() => Position {
                    size: Quantity::new_int(10),
                    entry_price: UsdPrice::new_int(3_000),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let pair_params = btree_map! {
            pair_btc() => btc_pair_param(),
            pair_eth() => eth_pair_param(),
        };
        let oracle_prices = btree_map! {
            pair_btc() => UsdPrice::new_int(47_000),
            pair_eth() => UsdPrice::new_int(2_800),
        };

        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_int(4_750),
        )
        .unwrap();

        // Both positions closed.
        assert_eq!(schedule.len(), 2);
        // BTC has larger MM (2350 > 1400) → first.
        assert_eq!(schedule[0].0, pair_btc());
        assert_eq!(schedule[0].1, Quantity::new_int(-1));
        assert_eq!(schedule[1].0, pair_eth());
        assert_eq!(schedule[1].1, Quantity::new_int(-10));
    }

    /// Deficit smaller than one position's full MM → partial close only.
    ///
    /// BTC: long 10 @ oracle $50,000, MMR 5%
    /// MM per unit = 50,000 * 0.05 = 2,500
    /// deficit = 5,000 → close_amount = 5000 / 2500 = 2  (partial)
    #[test]
    fn partial_close() {
        let user_state = UserState {
            positions: btree_map! {
                pair_btc() => Position {
                    size: Quantity::new_int(10),
                    entry_price: UsdPrice::new_int(50_000),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let pair_params = btree_map! { pair_btc() => btc_pair_param() };
        let oracle_prices = btree_map! { pair_btc() => UsdPrice::new_int(50_000) };

        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_int(5_000),
        )
        .unwrap();

        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule[0].0, pair_btc());
        // Only 2 of 10 BTC closed
        assert_eq!(schedule[0].1, Quantity::new_int(-2));
    }

    /// Regression for the silent-exit `liquidate` bug observed on mainnet.
    ///
    /// When the deficit is smaller than one ULP of the denominator
    /// `oracle_price × mmr`, floor division of `deficit / denominator` used
    /// to collapse `close_amount` to zero, leaving `compute_close_schedule`
    /// to return an empty `Vec`. Combined with a liquidatable user that has
    /// no resting orders or conditionals, the outer `liquidate` handler
    /// would then write unchanged state and return `Ok` with an empty
    /// `EventBuilder` — a successful tx that emitted no events.
    ///
    /// `compute_close_schedule` now uses ceiling division (matching the doc
    /// comment), which guarantees at least one ULP of close size whenever the
    /// user is liquidatable.
    ///
    /// Setup: long 1 BTC, oracle $60k, mmr 5% → denominator = $3,000.
    /// Deficit = $0.001 (raw 1_000 in `Dec128_6`'s 6-decimal representation).
    ///
    /// - `floor(deficit / denominator) = floor(1_000 × 10⁶ / 3_000_000_000) = floor(1/3) = 0` (old, buggy)
    /// - `ceil (deficit / denominator) = ceil (1/3) = 1` raw ULP of `Quantity` (new)
    #[test]
    fn sub_ulp_deficit_produces_one_ulp_close() {
        let user_state = UserState {
            positions: btree_map! {
                pair_btc() => Position {
                    size: Quantity::new_int(1),
                    entry_price: UsdPrice::new_int(50_000),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let pair_params = btree_map! { pair_btc() => btc_pair_param() };
        let oracle_prices = btree_map! { pair_btc() => UsdPrice::new_int(60_000) };

        // $0.001 — one milli-dollar, well below `denominator × 1 ULP = $0.003`.
        let deficit = UsdValue::new_raw(1_000);

        let schedule =
            compute_close_schedule(&user_state, &pair_params, &oracle_prices, deficit).unwrap();

        // With ceil division, the schedule contains exactly one entry of the
        // smallest representable close size (1 raw ULP = 0.000001 Quantity),
        // in the opposite direction of the long position.
        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule[0].0, pair_btc());
        assert_eq!(schedule[0].1, Quantity::new_raw(-1));
    }

    // -------------------------- dust-snap tests ------------------------------

    /// The deficit-only close would leave a sub-`min_order_size` remainder,
    /// so the snap fires and the entire position is closed.
    ///
    /// Setup: long 100 BTC @ $1, mmr 5%, `min_order_size = $5`. Deficit
    /// $4.85 → deficit-only `close_amount = ceil(4.85/0.05) = 97`,
    /// remainder = 3, notional $3 < $5 → snap to 100.
    #[test]
    fn dust_snap_triggers_full_close() {
        let user_state = UserState {
            positions: btree_map! {
                pair_btc() => Position {
                    size: Quantity::new_int(100),
                    entry_price: UsdPrice::new_int(1),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let pair_params = btree_map! {
            pair_btc() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::new_int(5),
                ..Default::default()
            },
        };
        let oracle_prices = btree_map! { pair_btc() => UsdPrice::new_int(1) };

        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_raw(4_850_000),
        )
        .unwrap();

        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule[0].0, pair_btc());
        assert_eq!(schedule[0].1, Quantity::new_int(-100));
    }

    /// Same shape as `dust_snap_triggers_full_close` but with
    /// `min_order_size = 0`. The snap stays inactive and the deficit-only
    /// close (97 BTC) is preserved. Verifies the change is opt-in per pair.
    #[test]
    fn dust_snap_inactive_when_min_order_size_zero() {
        let user_state = UserState {
            positions: btree_map! {
                pair_btc() => Position {
                    size: Quantity::new_int(100),
                    entry_price: UsdPrice::new_int(1),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let pair_params = btree_map! {
            pair_btc() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::ZERO,
                ..Default::default()
            },
        };
        let oracle_prices = btree_map! { pair_btc() => UsdPrice::new_int(1) };

        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_raw(4_850_000),
        )
        .unwrap();

        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule[0].1, Quantity::new_int(-97));
    }

    /// The remainder after a deficit-only close is comfortably above the
    /// floor, so no snap. Position survives partial liquidation as
    /// expected.
    ///
    /// Setup: long 100 BTC @ $1, mmr 5%, `min_order_size = $5`. Deficit
    /// $1 → `close_amount = 20`, remainder = 80 BTC @ $1 = $80 ≫ $5.
    #[test]
    fn dust_snap_inactive_when_remaining_above_threshold() {
        let user_state = UserState {
            positions: btree_map! {
                pair_btc() => Position {
                    size: Quantity::new_int(100),
                    entry_price: UsdPrice::new_int(1),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let pair_params = btree_map! {
            pair_btc() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::new_int(5),
                ..Default::default()
            },
        };
        let oracle_prices = btree_map! { pair_btc() => UsdPrice::new_int(1) };

        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_int(1),
        )
        .unwrap();

        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule[0].1, Quantity::new_int(-20));
    }

    /// Remainder notional exactly equals `min_order_size`. The comparison
    /// is strict (`<`), so no snap.
    ///
    /// Setup: long 100 BTC @ $1, mmr 5%, `min_order_size = $5`. Deficit
    /// $4.75 → `close_amount = ceil(4.75/0.05) = 95`, remainder = 5 BTC =
    /// $5 = min_order_size → boundary, no snap.
    #[test]
    fn dust_snap_boundary_not_strict() {
        let user_state = UserState {
            positions: btree_map! {
                pair_btc() => Position {
                    size: Quantity::new_int(100),
                    entry_price: UsdPrice::new_int(1),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let pair_params = btree_map! {
            pair_btc() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::new_int(5),
                ..Default::default()
            },
        };
        let oracle_prices = btree_map! { pair_btc() => UsdPrice::new_int(1) };

        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_raw(4_750_000),
        )
        .unwrap();

        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule[0].1, Quantity::new_int(-95));
    }

    /// The position is already smaller than `min_order_size` at
    /// liquidation entry. Any non-zero `close_amount` leaves a smaller-
    /// still remainder, so the snap fires and the whole position closes.
    ///
    /// Setup: long 5 BTC @ $1, mmr 5%, `min_order_size = $10`. Total
    /// notional = $5 < $10. Deficit $0.10 → deficit-only close = 2,
    /// remainder = 3 BTC @ $1 = $3 < $10 → snap to 5.
    #[test]
    fn dust_snap_position_already_dust() {
        let user_state = UserState {
            positions: btree_map! {
                pair_btc() => Position {
                    size: Quantity::new_int(5),
                    entry_price: UsdPrice::new_int(1),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let pair_params = btree_map! {
            pair_btc() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::new_int(10),
                ..Default::default()
            },
        };
        let oracle_prices = btree_map! { pair_btc() => UsdPrice::new_int(1) };

        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_raw(100_000),
        )
        .unwrap();

        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule[0].1, Quantity::new_int(-5));
    }

    /// Snap on a short position. `close_size` is positive (covering the
    /// short) and its magnitude equals `abs(size)`, confirming the sign
    /// is computed against `position.size`, not the snapped magnitude.
    ///
    /// Setup: short -10 ETH @ $1, mmr 5%, `min_order_size = $9`. Deficit
    /// $0.45 → close_amount = 9, remainder = 1 ETH = $1 < $9 → snap to
    /// 10. close_size = +10.
    #[test]
    fn dust_snap_short_position() {
        let user_state = UserState {
            positions: btree_map! {
                pair_eth() => Position {
                    size: Quantity::new_int(-10),
                    entry_price: UsdPrice::new_int(1),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let pair_params = btree_map! {
            pair_eth() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::new_int(9),
                ..Default::default()
            },
        };
        let oracle_prices = btree_map! { pair_eth() => UsdPrice::new_int(1) };

        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_raw(450_000),
        )
        .unwrap();

        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule[0].0, pair_eth());
        assert_eq!(schedule[0].1, Quantity::new_int(10));
    }

    /// Multi-pair: the first pair's snap clears the entire deficit, so
    /// the schedule never spills into the second pair. Demonstrates that
    /// the natural `deficit <= 0` break handles over-snapping without
    /// special multi-pair plumbing.
    ///
    /// Setup:
    /// - BTC: 100 @ $2, MM = $10, `min_order_size = $190`
    /// - ETH: 100 @ $1, MM = $5,  `min_order_size = 0`
    /// - deficit = $1 (would close 10 BTC under deficit-only logic)
    ///
    /// BTC step: close=10, remainder 90 @ $2 = $180 < $190 → snap to 100,
    /// mm_removed = $10, deficit cleared. ETH step breaks immediately.
    #[test]
    fn dust_snap_first_pair_clears_deficit() {
        let user_state = UserState {
            positions: btree_map! {
                pair_btc() => Position {
                    size: Quantity::new_int(100),
                    entry_price: UsdPrice::new_int(2),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
                pair_eth() => Position {
                    size: Quantity::new_int(100),
                    entry_price: UsdPrice::new_int(1),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let pair_params = btree_map! {
            pair_btc() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::new_int(190),
                ..Default::default()
            },
            pair_eth() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::ZERO,
                ..Default::default()
            },
        };
        let oracle_prices = btree_map! {
            pair_btc() => UsdPrice::new_int(2),
            pair_eth() => UsdPrice::new_int(1),
        };

        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_int(1),
        )
        .unwrap();

        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule[0].0, pair_btc());
        assert_eq!(schedule[0].1, Quantity::new_int(-100));
    }

    /// Multi-pair: the snap fires on the second (lower-MM) pair after
    /// the first is fully closed. Compared against the same fixture with
    /// `min_order_size = 0` on ETH so the difference is explicit.
    ///
    /// Setup:
    /// - BTC: 100 @ $2, MM = $10, `min_order_size = 0`
    /// - ETH: 100 @ $1, MM = $5
    /// - deficit = $11
    ///
    /// BTC always closes fully ($10 MM removed, deficit → $1). ETH then:
    /// - With `min_order_size = 0`: close 20, remainder 80, schedule = [BTC -100, ETH -20]
    /// - With `min_order_size = $90`: remainder 80 @ $1 = $80 < $90 → snap to 100
    #[test]
    fn dust_snap_second_pair_snaps_to_full() {
        let user_state = UserState {
            positions: btree_map! {
                pair_btc() => Position {
                    size: Quantity::new_int(100),
                    entry_price: UsdPrice::new_int(2),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
                pair_eth() => Position {
                    size: Quantity::new_int(100),
                    entry_price: UsdPrice::new_int(1),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        let oracle_prices = btree_map! {
            pair_btc() => UsdPrice::new_int(2),
            pair_eth() => UsdPrice::new_int(1),
        };

        // No floor on ETH → deficit-only partial close.
        let pair_params = btree_map! {
            pair_btc() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::ZERO,
                ..Default::default()
            },
            pair_eth() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::ZERO,
                ..Default::default()
            },
        };
        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_int(11),
        )
        .unwrap();
        assert_eq!(schedule.len(), 2);
        assert_eq!(schedule[0].0, pair_btc());
        assert_eq!(schedule[0].1, Quantity::new_int(-100));
        assert_eq!(schedule[1].0, pair_eth());
        assert_eq!(schedule[1].1, Quantity::new_int(-20));

        // With $90 floor on ETH → ETH snap to full close.
        let pair_params = btree_map! {
            pair_btc() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::ZERO,
                ..Default::default()
            },
            pair_eth() => PairParam {
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                min_order_size: UsdValue::new_int(90),
                ..Default::default()
            },
        };
        let schedule = compute_close_schedule(
            &user_state,
            &pair_params,
            &oracle_prices,
            UsdValue::new_int(11),
        )
        .unwrap();
        assert_eq!(schedule.len(), 2);
        assert_eq!(schedule[0].0, pair_btc());
        assert_eq!(schedule[0].1, Quantity::new_int(-100));
        assert_eq!(schedule[1].0, pair_eth());
        assert_eq!(schedule[1].1, Quantity::new_int(-100));
    }

    // ==================== `compute_bankruptcy_price` tests ====================

    #[test]
    fn bankruptcy_price_long_single_position() {
        // Long 1 BTC @ $50k, margin $3k, oracle $46k.
        // Equity = 3k + 1*(46k - 50k) = -1k.
        // bp = 46k - (-1k)/1 = 47k.
        let user_state = UserState {
            margin: UsdValue::new_int(3_000),
            positions: BTreeMap::from([(
                pair_btc(),
                Position {
                    size: Quantity::new_int(1),
                    entry_price: UsdPrice::new_int(50_000),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            )]),
            ..Default::default()
        };

        let oracle_prices = BTreeMap::from([(pair_btc(), UsdPrice::new_int(46_000))]);

        let bp = compute_bankruptcy_price(
            &user_state,
            &pair_btc(),
            &oracle_prices,
            UsdValue::ZERO,
            UsdValue::ZERO,
        )
        .unwrap();

        assert_eq!(bp, UsdPrice::new_int(47_000));
    }

    #[test]
    fn bankruptcy_price_short_single_position() {
        // Short 1 BTC @ $50k, margin $3k, oracle $54k.
        // Equity = 3k + (-1)*(54k-50k) = 3k - 4k = -1k.
        // bp = 54k + (-1k)/1 = 53k.
        let user_state = UserState {
            margin: UsdValue::new_int(3_000),
            positions: BTreeMap::from([(
                pair_btc(),
                Position {
                    size: Quantity::new_int(-1),
                    entry_price: UsdPrice::new_int(50_000),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            )]),
            ..Default::default()
        };

        let oracle_prices = BTreeMap::from([(pair_btc(), UsdPrice::new_int(54_000))]);

        let bp = compute_bankruptcy_price(
            &user_state,
            &pair_btc(),
            &oracle_prices,
            UsdValue::ZERO,
            UsdValue::ZERO,
        )
        .unwrap();

        assert_eq!(bp, UsdPrice::new_int(53_000));
    }

    #[test]
    fn bankruptcy_price_with_other_positions() {
        // Long 1 BTC @ $50k + Long 10 ETH @ $3k, margin $5k.
        // Oracle BTC $46k, ETH $3.2k.
        // Unrealized BTC = 1*(46k-50k) = -4k.
        // Unrealized ETH = 10*(3.2k-3k) = 2k.
        // Equity = 5k + (-4k) + 2k = 3k.
        // bp = 46k - 3k/1 = 43k.
        //
        // The offset divides the WHOLE-ACCOUNT equity (including the ETH
        // position's unrealized PnL) by the BTC position's full size. Under
        // the pre-fix formula, a 0.1 BTC scheduled close would have produced
        // bp = 46k - 3k/0.1 = 16k.
        let user_state = UserState {
            margin: UsdValue::new_int(5_000),
            positions: BTreeMap::from([
                (
                    pair_btc(),
                    Position {
                        size: Quantity::new_int(1),
                        entry_price: UsdPrice::new_int(50_000),
                        entry_funding_per_unit: FundingPerUnit::ZERO,
                        conditional_order_above: None,
                        conditional_order_below: None,
                    },
                ),
                (
                    pair_eth(),
                    Position {
                        size: Quantity::new_int(10),
                        entry_price: UsdPrice::new_int(3_000),
                        entry_funding_per_unit: FundingPerUnit::ZERO,
                        conditional_order_above: None,
                        conditional_order_below: None,
                    },
                ),
            ]),
            ..Default::default()
        };

        let oracle_prices = BTreeMap::from([
            (pair_btc(), UsdPrice::new_int(46_000)),
            (pair_eth(), UsdPrice::new_int(3_200)),
        ]);

        let bp = compute_bankruptcy_price(
            &user_state,
            &pair_btc(),
            &oracle_prices,
            UsdValue::ZERO,
            UsdValue::ZERO,
        )
        .unwrap();

        assert_eq!(bp, UsdPrice::new_int(43_000));
    }

    /// Regression test for the testnet incident at block 32500262 (and the
    /// mainnet ETH cluster on 2026-06-05): the bankruptcy price of a
    /// position scheduled for a **partial** close.
    ///
    /// The bug: the bankruptcy price divided the account's *whole* equity by
    /// the scheduled close amount. The close schedule sizes the close to cure
    /// the maintenance-margin deficit, so for a solvent account it is only a
    /// fraction of the position; the amplified offset `equity / close_amount`
    /// threw the price far from the oracle (here: 1,875 − 750/2 = 1,500,
    /// −20%; on testnet, SOL fills as low as −$0.596944 against a $76.32
    /// oracle), confiscating the account's entire equity through that one
    /// partial fill.
    ///
    /// The bankruptcy price now divides by the full position size — by
    /// definition, it is the price at which closing the *whole* position
    /// zeroes the account's equity — and is independent of how much the
    /// schedule intends to close.
    ///
    /// Setup: long 10 BTC @ $2,000, margin $2,000, oracle $1,875.
    /// Equity = 2,000 + 10 × (1,875 − 2,000) = 750 (solvent; the
    /// deficit-curing close for mmr 5% would be 2 of the 10 BTC).
    ///
    ///   bp = 1,875 − 750/10 = 1,800
    #[test]
    fn bankruptcy_price_partial_close_long() {
        let user_state = UserState {
            margin: UsdValue::new_int(2_000),
            positions: BTreeMap::from([(
                pair_btc(),
                Position {
                    size: Quantity::new_int(10),
                    entry_price: UsdPrice::new_int(2_000),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            )]),
            ..Default::default()
        };

        let oracle_prices = BTreeMap::from([(pair_btc(), UsdPrice::new_int(1_875))]);

        let bp = compute_bankruptcy_price(
            &user_state,
            &pair_btc(),
            &oracle_prices,
            UsdValue::ZERO,
            UsdValue::ZERO,
        )
        .unwrap();

        assert_eq!(bp, UsdPrice::new_int(1_800));
    }

    /// Short-side mirror of `bankruptcy_price_partial_close_long`.
    ///
    /// Setup: short 10 BTC @ $2,000, margin $2,000, oracle $2,125.
    /// Equity = 2,000 + (−10) × (2,125 − 2,000) = 750 (solvent).
    ///
    /// Under the bug, a partial close of 2 would have been priced at
    /// 2,125 + 750/2 = 2,500 (+17.6% from oracle). With the full position
    /// size:
    ///
    ///   bp = 2,125 + 750/10 = 2,200
    #[test]
    fn bankruptcy_price_partial_close_short() {
        let user_state = UserState {
            margin: UsdValue::new_int(2_000),
            positions: BTreeMap::from([(
                pair_btc(),
                Position {
                    size: Quantity::new_int(-10),
                    entry_price: UsdPrice::new_int(2_000),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            )]),
            ..Default::default()
        };

        let oracle_prices = BTreeMap::from([(pair_btc(), UsdPrice::new_int(2_125))]);

        let bp = compute_bankruptcy_price(
            &user_state,
            &pair_btc(),
            &oracle_prices,
            UsdValue::ZERO,
            UsdValue::ZERO,
        )
        .unwrap();

        assert_eq!(bp, UsdPrice::new_int(2_200));
    }
}
