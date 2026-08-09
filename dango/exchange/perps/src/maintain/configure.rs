use {
    crate::{
        core::check_fee_sign_invariant,
        oracle,
        state::{PAIR_IDS, PAIR_PARAMS, PAIR_STATES, PARAM},
    },
    anyhow::ensure,
    dango_oracle::OracleQuerier,
    dango_order_book::{Dimensionless, FundingRate, PairId, UsdPrice, UsdValue},
    dango_primitives::{
        Duration, GENESIS_BLOCK_HEIGHT, GENESIS_SENDER, MutableCtx, QuerierExt, Response,
    },
    dango_types::perps::{PairParam, PairState, Param, RateSchedule},
    std::collections::BTreeMap,
};

/// Upper bound for `funding_period`. A period longer than a week would
/// effectively suspend funding collection.
const MAX_FUNDING_PERIOD: Duration = Duration::from_days(7);

/// Upper bound for `vault_cooldown_period`. A cooldown longer than a month
/// would effectively lock LPs out of their deposits.
const MAX_VAULT_COOLDOWN_PERIOD: Duration = Duration::from_days(30);

/// Upper bound for `max_abs_funding_rate` (per day). 100% per day already
/// implies an OI-weighted funding payment equal to one day's notional —
/// anything larger is pathological.
const MAX_FUNDING_RATE_CAP: FundingRate = FundingRate::new_int(1);

/// Update global and per-pair parameters.
/// Callable by the chain owner or `GENESIS_SENDER` (during instantiation).
///
/// Validation runs before any state mutation: if any field or cross-struct
/// invariant is rejected, `PARAM`, `PAIR_PARAMS`, `PAIR_STATES`, and
/// `PAIR_IDS` are left untouched.
pub fn configure(
    ctx: MutableCtx,
    param: Param,
    pair_params: BTreeMap<PairId, PairParam>,
) -> anyhow::Result<Response> {
    // ---------------------------- 1. Validations -----------------------------

    ensure!(
        ctx.sender == ctx.querier.query_owner()? || ctx.sender == GENESIS_SENDER,
        "You don't have the right, O you don't have the right"
    );

    validate_param(&param)?;

    for (pair_id, pair_param) in &pair_params {
        validate_pair_param(pair_id, pair_param)?;
    }

    validate_vault_total_weight(&param, &pair_params)?;

    // Net-fee distribution in `settle_pnls` requires
    // `taker_fee_rate + maker_fee_rate >= 0` on every fill. Enforce the
    // invariant against the tier schedules and any stored per-user overrides.
    check_fee_sign_invariant(ctx.storage, &param, None)?;

    // --------------------------- 2. State changes ----------------------------

    PARAM.save(ctx.storage, &param)?;

    for (pair_id, pair_param) in &pair_params {
        PAIR_PARAMS.save(ctx.storage, pair_id, pair_param)?;

        if !PAIR_STATES.has(ctx.storage, pair_id) {
            let pair_state = if ctx.block.height == GENESIS_BLOCK_HEIGHT {
                // At genesis the oracle has no prices yet (they are fed after
                // all contracts are instantiated). Setting index_price to zero
                // is technically wrong, but harmless: process_index_price will
                // populate it on the first cron tick after prices are fed.
                // This only affects tests — mainnet has already genesis'd.
                PairState::default()
            } else {
                // Post-genesis, the oracle must have a price. Defaulting to
                // zero would break margin and PnL calculations.
                let price = OracleQuerier::new_remote(
                    oracle(ctx.querier),
                    ctx.querier,
                    ctx.block.timestamp,
                )
                .query_price(pair_id, None)?;

                // Use the oracle's own timestamp so that last_index_time
                // reflects when the price was actually observed.
                PairState {
                    index_price: price.humanized_price,
                    last_index_time: price.timestamp,
                    oracle_price: price.humanized_price,
                    last_oracle_time: price.timestamp,
                    ..Default::default()
                }
            };

            PAIR_STATES.save(ctx.storage, pair_id, &pair_state)?;
        }
    }

    if !pair_params.is_empty() {
        PAIR_IDS.save(ctx.storage, &pair_params.into_keys().collect())?;
    }

    Ok(Response::new())
}

/// Validate every field of the global `Param` struct.
///
/// Bounds are documented on each field in `dango_types::perps::Param`.
fn validate_param(param: &Param) -> anyhow::Result<()> {
    ensure!(
        param.max_unlocks > 0,
        "invalid `max_unlocks`! bounds: > 0, found: {}",
        param.max_unlocks,
    );

    ensure!(
        param.max_open_orders > 0,
        "invalid `max_open_orders`! bounds: > 0, found: {}",
        param.max_open_orders,
    );

    ensure!(
        param.max_action_batch_size > 0,
        "invalid `max_action_batch_size`! bounds: > 0, found: {}",
        param.max_action_batch_size,
    );

    // Maker fees may be negative: a negative rate represents a rebate paid
    // to the maker. Taker fees and referrer commissions are always non-negative.
    validate_rate_schedule(
        "maker_fee_rates",
        &param.maker_fee_rates,
        Dimensionless::new_int(-1),
        Dimensionless::ONE,
    )?;

    validate_rate_schedule(
        "taker_fee_rates",
        &param.taker_fee_rates,
        Dimensionless::ZERO,
        Dimensionless::ONE,
    )?;

    validate_rate_schedule(
        "referrer_commission_rates",
        &param.referrer_commission_rates,
        Dimensionless::ZERO,
        Dimensionless::ONE,
    )?;

    ensure!(
        (Dimensionless::ZERO..=Dimensionless::ONE).contains(&param.protocol_fee_rate),
        "invalid `protocol_fee_rate`! bounds: [0, 1], found: {}",
        param.protocol_fee_rate,
    );

    ensure!(
        (Dimensionless::ZERO..=Dimensionless::ONE).contains(&param.liquidation_fee_rate),
        "invalid `liquidation_fee_rate`! bounds: [0, 1], found: {}",
        param.liquidation_fee_rate,
    );

    ensure!(
        (Dimensionless::ZERO..Dimensionless::ONE).contains(&param.liquidation_buffer_ratio),
        "invalid `liquidation_buffer_ratio`! bounds: [0, 1), found: {}",
        param.liquidation_buffer_ratio,
    );

    ensure!(
        param.funding_period > Duration::ZERO && param.funding_period <= MAX_FUNDING_PERIOD,
        "invalid `funding_period`! bounds: (0, {:?}], found: {:?}",
        MAX_FUNDING_PERIOD,
        param.funding_period,
    );

    // `vault_total_weight` is cross-validated against the sum of per-pair
    // weights in `validate_vault_total_weight`; no independent lower-bound
    // check is needed here.

    ensure!(
        param.vault_cooldown_period > Duration::ZERO
            && param.vault_cooldown_period <= MAX_VAULT_COOLDOWN_PERIOD,
        "invalid `vault_cooldown_period`! bounds: (0, {:?}], found: {:?}",
        MAX_VAULT_COOLDOWN_PERIOD,
        param.vault_cooldown_period,
    );

    // `referral_active: bool` — always valid.

    ensure!(
        param.min_referrer_volume >= UsdValue::ZERO,
        "invalid `min_referrer_volume`! bounds: >= 0, found: {}",
        param.min_referrer_volume,
    );

    if let Some(cap) = param.vault_deposit_cap {
        ensure!(
            cap > UsdValue::ZERO,
            "invalid `vault_deposit_cap`! bounds: if Some, > 0, found: {}",
            cap,
        );
    }

    Ok(())
}

/// Validate every field of a per-pair `PairParam` struct.
///
/// Bounds are documented on each field in `dango_types::perps::PairParam`.
fn validate_pair_param(pair_id: &PairId, pair_param: &PairParam) -> anyhow::Result<()> {
    ensure!(
        pair_param.tick_size > UsdPrice::ZERO,
        "invalid `tick_size`! pair id: {}, bounds: > 0, found: {}",
        pair_id,
        pair_param.tick_size,
    );

    ensure!(
        pair_param.min_order_size >= UsdValue::ZERO,
        "invalid `min_order_size`! pair id: {}, bounds: >= 0, found: {}",
        pair_id,
        pair_param.min_order_size,
    );

    ensure!(
        pair_param.max_abs_oi.is_positive(),
        "invalid `max_abs_oi`! pair id: {}, bounds: > 0, found: {}",
        pair_id,
        pair_param.max_abs_oi,
    );

    ensure!(
        !pair_param.max_abs_funding_rate.is_negative()
            && pair_param.max_abs_funding_rate <= MAX_FUNDING_RATE_CAP,
        "invalid `max_abs_funding_rate`! pair id: {}, bounds: [0, {}], found: {}",
        pair_id,
        MAX_FUNDING_RATE_CAP,
        pair_param.max_abs_funding_rate,
    );

    ensure!(
        pair_param.initial_margin_ratio > Dimensionless::ZERO
            && pair_param.initial_margin_ratio <= Dimensionless::ONE,
        "invalid `initial_margin_ratio`! pair id: {}, bounds: (0, 1], found: {}",
        pair_id,
        pair_param.initial_margin_ratio,
    );

    ensure!(
        pair_param.maintenance_margin_ratio > Dimensionless::ZERO
            && pair_param.maintenance_margin_ratio <= Dimensionless::ONE,
        "invalid `maintenance_margin_ratio`! pair id: {}, bounds: (0, 1], found: {}",
        pair_id,
        pair_param.maintenance_margin_ratio,
    );

    ensure!(
        pair_param.maintenance_margin_ratio < pair_param.initial_margin_ratio,
        "invalid `maintenance_margin_ratio`! pair id: {}, bounds: must be < `initial_margin_ratio` ({}), found: {}",
        pair_id,
        pair_param.initial_margin_ratio,
        pair_param.maintenance_margin_ratio,
    );

    ensure!(
        pair_param.impact_size > UsdValue::ZERO,
        "invalid `impact_size`! pair id: {}, bounds: > 0, found: {}",
        pair_id,
        pair_param.impact_size,
    );

    ensure!(
        pair_param.vault_liquidity_weight >= Dimensionless::ZERO,
        "invalid `vault_liquidity_weight`! pair id: {}, bounds: >= 0, found: {}",
        pair_id,
        pair_param.vault_liquidity_weight,
    );

    ensure!(
        pair_param.vault_half_spread > Dimensionless::ZERO
            && pair_param.vault_half_spread < Dimensionless::ONE,
        "invalid `vault_half_spread`! pair id: {}, bounds: (0, 1), found: {}",
        pair_id,
        pair_param.vault_half_spread,
    );

    ensure!(
        pair_param.vault_max_quote_size.is_positive(),
        "invalid `vault_max_quote_size`! pair id: {}, bounds: > 0, found: {}",
        pair_id,
        pair_param.vault_max_quote_size,
    );

    ensure!(
        (Dimensionless::ZERO..=Dimensionless::ONE).contains(&pair_param.vault_size_skew_factor),
        "invalid `vault_size_skew_factor`! pair id: {}, bounds: [0, 1], found: {}",
        pair_id,
        pair_param.vault_size_skew_factor,
    );

    ensure!(
        !pair_param.vault_spread_skew_factor.is_negative(),
        "invalid `vault_spread_skew_factor`! pair id: {}, bounds: >= 0, found: {}",
        pair_id,
        pair_param.vault_spread_skew_factor,
    );

    // Cross-field: under maximum positive skew, the bid-side effective spread
    // is `vault_half_spread * (1 + vault_spread_skew_factor)`. We require
    // this product to stay strictly below 1 so the vault's bid price never
    // collapses to or below zero, even when inventory is fully skewed.
    let one_plus_factor = Dimensionless::ONE.checked_add(pair_param.vault_spread_skew_factor)?;
    let max_bid_effective_spread = pair_param.vault_half_spread.checked_mul(one_plus_factor)?;
    ensure!(
        max_bid_effective_spread < Dimensionless::ONE,
        "invalid `vault_half_spread`/`vault_spread_skew_factor`! pair id: {}, bounds: vault_half_spread * (1 + vault_spread_skew_factor) < 1, found: {} * (1 + {}) = {}",
        pair_id,
        pair_param.vault_half_spread,
        pair_param.vault_spread_skew_factor,
        max_bid_effective_spread,
    );

    ensure!(
        !pair_param.vault_max_skew_size.is_negative(),
        "invalid `vault_max_skew_size`! pair id: {}, bounds: >= 0, found: {}",
        pair_id,
        pair_param.vault_max_skew_size,
    );

    ensure!(
        !pair_param.funding_rate_multiplier.is_negative(),
        "invalid `funding_rate_multiplier`! pair id: {}, bounds: >= 0, found: {}",
        pair_id,
        pair_param.funding_rate_multiplier,
    );

    ensure!(
        pair_param.max_limit_price_deviation > Dimensionless::ZERO
            && pair_param.max_limit_price_deviation < Dimensionless::ONE,
        "invalid `max_limit_price_deviation`! pair id: {}, bounds: (0, 1), found: {}",
        pair_id,
        pair_param.max_limit_price_deviation,
    );

    // Cross-field: the band must be wide enough to admit any price the
    // vault may legitimately quote into. The vault's widest quote under
    // maximum skew sits at oracle_price × (1 ± max_bid_effective_spread)
    // (computed above), so a user's crossing limit at that price must
    // also pass the band check. If `max_limit_price_deviation` is set
    // tighter than the vault's widest deviation, users cannot match the
    // vault at its legitimately-quoted edges.
    ensure!(
        pair_param.max_limit_price_deviation >= max_bid_effective_spread,
        "invalid `max_limit_price_deviation`! pair id: {}, bounds: must be >= vault_half_spread * (1 + vault_spread_skew_factor) = {}, found: {}",
        pair_id,
        max_bid_effective_spread,
        pair_param.max_limit_price_deviation,
    );

    ensure!(
        pair_param.max_market_slippage > Dimensionless::ZERO
            && pair_param.max_market_slippage < Dimensionless::ONE,
        "invalid `max_market_slippage`! pair id: {}, bounds: (0, 1), found: {}",
        pair_id,
        pair_param.max_market_slippage,
    );

    for bucket_size in &pair_param.bucket_sizes {
        ensure!(
            *bucket_size > UsdPrice::ZERO,
            "invalid `bucket_sizes`! pair id: {}, bounds: each entry > 0, found: {}",
            pair_id,
            bucket_size,
        );
    }

    Ok(())
}

/// Cross-struct invariant: `param.vault_total_weight` must equal the sum of
/// `vault_liquidity_weight` across all provided pairs. The sum is used as a
/// divisor in `refresh_orders` when allocating vault margin across pairs;
/// a drift between the precomputed total and the actual sum silently
/// corrupts every subsequent allocation.
fn validate_vault_total_weight(
    param: &Param,
    pair_params: &BTreeMap<PairId, PairParam>,
) -> anyhow::Result<()> {
    let mut expected = Dimensionless::ZERO;
    for pair_param in pair_params.values() {
        expected.checked_add_assign(pair_param.vault_liquidity_weight)?;
    }

    ensure!(
        param.vault_total_weight == expected,
        "invalid `vault_total_weight`! bounds: must equal sum of `vault_liquidity_weight` across all pairs, expected: {}, found: {}",
        expected,
        param.vault_total_weight,
    );

    Ok(())
}

/// Validate that every rate in a `RateSchedule` (base + all tier values) lies
/// in the inclusive range `[min, max]`.
fn validate_rate_schedule(
    name: &str,
    schedule: &RateSchedule,
    min: Dimensionless,
    max: Dimensionless,
) -> anyhow::Result<()> {
    ensure!(
        (min..=max).contains(&schedule.base),
        "invalid `{}.base`! bounds: [{}, {}], found: {}",
        name,
        min,
        max,
        schedule.base,
    );

    for (threshold, rate) in &schedule.tiers {
        ensure!(
            (min..=max).contains(rate),
            "invalid `{}.tiers@{}`! bounds: [{}, {}], found: {}",
            name,
            threshold,
            min,
            max,
            rate,
        );
    }

    Ok(())
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_math::Number as _,
        dango_order_book::{FundingRate, Quantity},
        dango_primitives::{Duration, btree_map, btree_set},
    };

    /// A `Param` that passes validation. Individual tests mutate one field to
    /// drive it out of range.
    fn valid_param() -> Param {
        Param {
            max_unlocks: 10,
            max_open_orders: 100,
            maker_fee_rates: RateSchedule::default(),
            taker_fee_rates: RateSchedule::default(),
            protocol_fee_rate: Dimensionless::new_permille(200), // 20%
            liquidation_fee_rate: Dimensionless::new_permille(10), // 1%
            liquidation_buffer_ratio: Dimensionless::new_permille(50), // 5%
            funding_period: Duration::from_hours(1),
            vault_total_weight: Dimensionless::ZERO,
            vault_cooldown_period: Duration::from_days(1),
            referral_active: false,
            min_referrer_volume: UsdValue::ZERO,
            referrer_commission_rates: RateSchedule::default(),
            vault_deposit_cap: None,
            max_action_batch_size: 5,
            trading_enabled: true,
        }
    }

    /// A `PairParam` that passes validation.
    fn valid_pair_param() -> PairParam {
        PairParam {
            tick_size: UsdPrice::new_int(1),
            min_order_size: UsdValue::ZERO,
            max_limit_price_deviation: Dimensionless::new_permille(100), // 10%
            max_market_slippage: Dimensionless::new_permille(100),       // 10%
            max_abs_oi: Quantity::new_int(1_000_000),
            max_abs_funding_rate: FundingRate::ZERO,
            initial_margin_ratio: Dimensionless::new_permille(100), // 10%
            maintenance_margin_ratio: Dimensionless::new_permille(50), // 5%
            impact_size: UsdValue::new_int(10_000),
            vault_liquidity_weight: Dimensionless::ZERO,
            vault_half_spread: Dimensionless::new_permille(10), // 1%
            vault_max_quote_size: Quantity::new_int(100),
            vault_size_skew_factor: Dimensionless::ZERO,
            vault_spread_skew_factor: Dimensionless::ZERO,
            vault_max_skew_size: Quantity::ZERO,
            funding_rate_multiplier: Dimensionless::ONE,
            bucket_sizes: btree_set! {},
        }
    }

    fn pair() -> PairId {
        "perp/ethusd".parse().unwrap()
    }

    // ------------------------------ validate_param ------------------------------

    #[test]
    fn param_default_is_valid() {
        validate_param(&valid_param()).unwrap();
    }

    #[test]
    fn param_zero_max_unlocks_rejected() {
        let param = Param {
            max_unlocks: 0,
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`max_unlocks`"), "{err}");
        assert!(err.contains("> 0"), "{err}");
    }

    #[test]
    fn param_zero_max_open_orders_rejected() {
        let param = Param {
            max_open_orders: 0,
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`max_open_orders`"), "{err}");
    }

    #[test]
    fn param_zero_max_action_batch_size_rejected() {
        let param = Param {
            max_action_batch_size: 0,
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`max_action_batch_size`"), "{err}");
    }

    #[test]
    fn param_negative_maker_fee_allowed() {
        let param = Param {
            maker_fee_rates: RateSchedule {
                base: Dimensionless::new_raw(-100), // -1 bps rebate
                ..Default::default()
            },
            ..valid_param()
        };
        validate_param(&param).unwrap();
    }

    #[test]
    fn param_maker_fee_below_minus_one_rejected() {
        let param = Param {
            maker_fee_rates: RateSchedule {
                base: Dimensionless::new_int(-2),
                ..Default::default()
            },
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`maker_fee_rates.base`"), "{err}");
    }

    #[test]
    fn param_negative_taker_fee_rejected() {
        let param = Param {
            taker_fee_rates: RateSchedule {
                base: Dimensionless::new_raw(-1),
                ..Default::default()
            },
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`taker_fee_rates.base`"), "{err}");
    }

    #[test]
    fn param_taker_fee_tier_out_of_range_rejected() {
        let param = Param {
            taker_fee_rates: RateSchedule {
                base: Dimensionless::new_permille(1),
                tiers: btree_map! {
                    UsdValue::new_int(1_000) => Dimensionless::new_int(2), // 200%
                },
            },
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`taker_fee_rates.tiers"), "{err}");
    }

    #[test]
    fn param_protocol_fee_above_one_rejected() {
        let param = Param {
            protocol_fee_rate: Dimensionless::new_int(2),
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`protocol_fee_rate`"), "{err}");
    }

    #[test]
    fn param_liquidation_buffer_ratio_one_rejected() {
        // Bound is [0, 1) — exactly 1 is excluded.
        let param = Param {
            liquidation_buffer_ratio: Dimensionless::ONE,
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`liquidation_buffer_ratio`"), "{err}");
        assert!(err.contains("[0, 1)"), "{err}");
    }

    #[test]
    fn param_zero_funding_period_rejected() {
        let param = Param {
            funding_period: Duration::ZERO,
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`funding_period`"), "{err}");
    }

    #[test]
    fn param_zero_vault_cooldown_rejected() {
        let param = Param {
            vault_cooldown_period: Duration::ZERO,
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`vault_cooldown_period`"), "{err}");
    }

    #[test]
    fn param_zero_vault_deposit_cap_rejected() {
        // `None` is allowed (unlimited), but `Some(0)` is not.
        let param = Param {
            vault_deposit_cap: Some(UsdValue::ZERO),
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`vault_deposit_cap`"), "{err}");
    }

    // --------------------------- validate_pair_param ----------------------------

    #[test]
    fn pair_param_default_is_valid() {
        validate_pair_param(&pair(), &valid_pair_param()).unwrap();
    }

    #[test]
    fn pair_param_zero_tick_size_rejected() {
        let p = PairParam {
            tick_size: UsdPrice::ZERO,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`tick_size`"), "{err}");
        assert!(err.contains("pair id:"), "{err}");
    }

    #[test]
    fn pair_param_zero_max_abs_oi_rejected() {
        let p = PairParam {
            max_abs_oi: Quantity::ZERO,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`max_abs_oi`"), "{err}");
    }

    #[test]
    fn pair_param_negative_max_abs_funding_rate_rejected() {
        let p = PairParam {
            max_abs_funding_rate: FundingRate::new_raw(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`max_abs_funding_rate`"), "{err}");
    }

    #[test]
    fn pair_param_zero_initial_margin_ratio_rejected() {
        let p = PairParam {
            initial_margin_ratio: Dimensionless::ZERO,
            maintenance_margin_ratio: Dimensionless::ZERO,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`initial_margin_ratio`"), "{err}");
    }

    #[test]
    fn pair_param_imr_above_one_rejected() {
        let p = PairParam {
            initial_margin_ratio: Dimensionless::new_int(2),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`initial_margin_ratio`"), "{err}");
    }

    #[test]
    fn pair_param_mmr_equal_to_imr_rejected() {
        let p = PairParam {
            initial_margin_ratio: Dimensionless::new_permille(100),
            maintenance_margin_ratio: Dimensionless::new_permille(100),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`maintenance_margin_ratio`"), "{err}");
        assert!(err.contains("< `initial_margin_ratio`"), "{err}");
    }

    #[test]
    fn pair_param_mmr_above_imr_rejected() {
        let p = PairParam {
            initial_margin_ratio: Dimensionless::new_permille(100),
            maintenance_margin_ratio: Dimensionless::new_permille(200),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`maintenance_margin_ratio`"), "{err}");
    }

    #[test]
    fn pair_param_zero_impact_size_rejected() {
        let p = PairParam {
            impact_size: UsdValue::ZERO,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`impact_size`"), "{err}");
    }

    #[test]
    fn pair_param_vault_half_spread_zero_rejected() {
        // Zero would collapse bid and ask onto the oracle price.
        let p = PairParam {
            vault_half_spread: Dimensionless::ZERO,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`vault_half_spread`"), "{err}");
        assert!(err.contains("(0, 1)"), "{err}");
    }

    #[test]
    fn pair_param_vault_half_spread_one_rejected() {
        let p = PairParam {
            vault_half_spread: Dimensionless::ONE,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`vault_half_spread`"), "{err}");
        assert!(err.contains("(0, 1)"), "{err}");
    }

    #[test]
    fn pair_param_vault_max_quote_size_zero_rejected() {
        let p = PairParam {
            vault_max_quote_size: Quantity::ZERO,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`vault_max_quote_size`"), "{err}");
        assert!(err.contains("> 0"), "{err}");
    }

    #[test]
    fn pair_param_negative_vault_max_quote_size_rejected() {
        let p = PairParam {
            vault_max_quote_size: Quantity::new_int(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`vault_max_quote_size`"), "{err}");
    }

    #[test]
    fn pair_param_size_skew_factor_above_one_rejected() {
        let p = PairParam {
            vault_size_skew_factor: Dimensionless::new_permille(1_500), // 1.5
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`vault_size_skew_factor`"), "{err}");
    }

    #[test]
    fn pair_param_spread_skew_factor_one_accepted() {
        // `vault_spread_skew_factor = 1` is now accepted as long as the
        // cross-field invariant `vault_half_spread * (1 + factor) < 1` holds.
        // Here: 0.01 * 2 = 0.02 < 1.
        let p = PairParam {
            vault_half_spread: Dimensionless::new_permille(10), // 1%
            vault_spread_skew_factor: Dimensionless::ONE,
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    #[test]
    fn pair_param_spread_skew_factor_above_one_accepted() {
        // `vault_spread_skew_factor = 5` is accepted when paired with a small
        // `vault_half_spread`: 0.1 * (1 + 5) = 0.6 < 1. At max positive skew,
        // the vault's ask drops below the oracle — the intended aggressive
        // unwind semantic.
        let p = PairParam {
            vault_half_spread: Dimensionless::new_permille(100), // 10%
            vault_spread_skew_factor: Dimensionless::new_int(5),
            // Band must accommodate the vault's widest quote deviation
            // (`vault_half_spread * (1 + factor)` = 60%).
            max_limit_price_deviation: Dimensionless::new_permille(700),
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    #[test]
    fn pair_param_spread_skew_factor_above_one_rejected_by_cross_invariant() {
        // `vault_spread_skew_factor = 5` with `vault_half_spread = 0.2` violates
        // the cross-field invariant (0.2 * 6 = 1.2 >= 1). This proves the
        // cross-field invariant — not any per-field cap — is the sole gatekeeper
        // for large factors.
        let p = PairParam {
            vault_half_spread: Dimensionless::new_permille(200), // 20%
            vault_spread_skew_factor: Dimensionless::new_int(5),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(
            err.contains("vault_half_spread * (1 + vault_spread_skew_factor) < 1"),
            "{err}"
        );
    }

    #[test]
    fn pair_param_negative_vault_max_skew_size_rejected() {
        let p = PairParam {
            vault_max_skew_size: Quantity::new_int(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`vault_max_skew_size`"), "{err}");
    }

    #[test]
    fn pair_param_negative_funding_rate_multiplier_rejected() {
        let p = PairParam {
            funding_rate_multiplier: Dimensionless::new_raw(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`funding_rate_multiplier`"), "{err}");
        assert!(err.contains(">= 0"), "{err}");
    }

    #[test]
    fn pair_param_zero_funding_rate_multiplier_accepted() {
        // Zero is in range — lets governance disable funding for a pair
        // without touching the vault's quoting.
        let p = PairParam {
            funding_rate_multiplier: Dimensionless::ZERO,
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    #[test]
    fn pair_param_large_funding_rate_multiplier_accepted() {
        // No upper bound on the multiplier.
        let p = PairParam {
            funding_rate_multiplier: Dimensionless::new_int(1_000),
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    #[test]
    fn pair_param_zero_bucket_size_rejected() {
        let p = PairParam {
            bucket_sizes: btree_set! { UsdPrice::ZERO },
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`bucket_sizes`"), "{err}");
    }

    #[test]
    fn pair_param_zero_max_limit_price_deviation_rejected() {
        let p = PairParam {
            max_limit_price_deviation: Dimensionless::ZERO,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`max_limit_price_deviation`"), "{err}");
        assert!(err.contains("(0, 1)"), "{err}");
    }

    #[test]
    fn pair_param_one_max_limit_price_deviation_rejected() {
        let p = PairParam {
            max_limit_price_deviation: Dimensionless::ONE,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`max_limit_price_deviation`"), "{err}");
        assert!(err.contains("(0, 1)"), "{err}");
    }

    #[test]
    fn pair_param_negative_max_limit_price_deviation_rejected() {
        let p = PairParam {
            max_limit_price_deviation: Dimensionless::new_raw(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`max_limit_price_deviation`"), "{err}");
    }

    #[test]
    fn pair_param_max_limit_price_deviation_just_below_one_accepted() {
        let p = PairParam {
            max_limit_price_deviation: Dimensionless::new_permille(999), // 99.9%
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    /// Boundary: band exactly equal to the vault's widest quote
    /// deviation (`vault_half_spread × (1 + vault_spread_skew_factor)`)
    /// is accepted because the submission band check is inclusive
    /// (`|Δ| ≤ oracle × dev`).
    #[test]
    fn pair_param_max_limit_price_deviation_equals_vault_max_accepted() {
        // vault_half_spread = 2%, vault_spread_skew_factor = 0.5 →
        // vault max deviation = 2% × 1.5 = 3%.
        let p = PairParam {
            vault_half_spread: Dimensionless::new_permille(20), // 2%
            vault_spread_skew_factor: Dimensionless::new_permille(500), // 0.5
            max_limit_price_deviation: Dimensionless::new_permille(30), // 3%
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    /// Band tighter than the vault's widest quote by one permille is
    /// rejected — users would be unable to place a crossing limit at
    /// the vault's max-skewed quote.
    #[test]
    fn pair_param_max_limit_price_deviation_below_vault_max_rejected() {
        // vault_half_spread = 2%, skew = 0.5 → vault max = 3%.
        // Band = 2.9% — one permille below.
        let p = PairParam {
            vault_half_spread: Dimensionless::new_permille(20),
            vault_spread_skew_factor: Dimensionless::new_permille(500),
            max_limit_price_deviation: Dimensionless::new_permille(29),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`max_limit_price_deviation`"), "{err}");
        assert!(
            err.contains("vault_half_spread * (1 + vault_spread_skew_factor)"),
            "{err}"
        );
    }

    /// Realistic setup: vault half-spread 1%, skew 0.3 (vault max
    /// 1.3%), band 10% — comfortably clears the invariant.
    #[test]
    fn pair_param_max_limit_price_deviation_well_above_vault_max_accepted() {
        let p = PairParam {
            vault_half_spread: Dimensionless::new_permille(10), // 1%
            vault_spread_skew_factor: Dimensionless::new_permille(300), // 0.3
            max_limit_price_deviation: Dimensionless::new_permille(100), // 10%
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    #[test]
    fn pair_param_zero_max_market_slippage_rejected() {
        let p = PairParam {
            max_market_slippage: Dimensionless::ZERO,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`max_market_slippage`"), "{err}");
        assert!(err.contains("(0, 1)"), "{err}");
    }

    #[test]
    fn pair_param_one_max_market_slippage_rejected() {
        let p = PairParam {
            max_market_slippage: Dimensionless::ONE,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`max_market_slippage`"), "{err}");
        assert!(err.contains("(0, 1)"), "{err}");
    }

    #[test]
    fn pair_param_negative_max_market_slippage_rejected() {
        let p = PairParam {
            max_market_slippage: Dimensionless::new_raw(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`max_market_slippage`"), "{err}");
    }

    #[test]
    fn pair_param_max_market_slippage_just_below_one_accepted() {
        let p = PairParam {
            max_market_slippage: Dimensionless::new_permille(999), // 99.9%
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    // --------------- validate_param — negative-value branches ------------------

    #[test]
    fn param_negative_liquidation_buffer_ratio_rejected() {
        let param = Param {
            liquidation_buffer_ratio: Dimensionless::new_raw(-1),
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`liquidation_buffer_ratio`"), "{err}");
    }

    #[test]
    fn param_negative_min_referrer_volume_rejected() {
        let param = Param {
            min_referrer_volume: UsdValue::new_raw(-1),
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`min_referrer_volume`"), "{err}");
    }

    #[test]
    fn param_liquidation_fee_rate_above_one_rejected() {
        let param = Param {
            liquidation_fee_rate: Dimensionless::new_int(2),
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`liquidation_fee_rate`"), "{err}");
    }

    #[test]
    fn param_negative_liquidation_fee_rate_rejected() {
        let param = Param {
            liquidation_fee_rate: Dimensionless::new_raw(-1),
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`liquidation_fee_rate`"), "{err}");
    }

    #[test]
    fn param_negative_protocol_fee_rate_rejected() {
        let param = Param {
            protocol_fee_rate: Dimensionless::new_raw(-1),
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`protocol_fee_rate`"), "{err}");
    }

    #[test]
    fn param_negative_referrer_commission_rate_rejected() {
        let param = Param {
            referrer_commission_rates: RateSchedule {
                base: Dimensionless::new_raw(-1),
                ..Default::default()
            },
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`referrer_commission_rates.base`"), "{err}");
    }

    #[test]
    fn param_referrer_commission_tier_out_of_range_rejected() {
        let param = Param {
            referrer_commission_rates: RateSchedule {
                base: Dimensionless::new_permille(100),
                tiers: btree_map! {
                    UsdValue::new_int(1_000) => Dimensionless::new_int(2),
                },
            },
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`referrer_commission_rates.tiers@"), "{err}");
    }

    // ------------------- validate_param — inclusive boundaries -----------------

    #[test]
    fn param_protocol_fee_rate_exactly_one_accepted() {
        let param = Param {
            protocol_fee_rate: Dimensionless::ONE,
            ..valid_param()
        };
        validate_param(&param).unwrap();
    }

    #[test]
    fn param_liquidation_fee_rate_exactly_one_accepted() {
        let param = Param {
            liquidation_fee_rate: Dimensionless::ONE,
            ..valid_param()
        };
        validate_param(&param).unwrap();
    }

    #[test]
    fn param_maker_fee_rate_exactly_one_accepted() {
        let param = Param {
            maker_fee_rates: RateSchedule {
                base: Dimensionless::ONE,
                ..Default::default()
            },
            ..valid_param()
        };
        validate_param(&param).unwrap();
    }

    #[test]
    fn param_maker_fee_rate_exactly_minus_one_accepted() {
        let param = Param {
            maker_fee_rates: RateSchedule {
                base: Dimensionless::new_int(-1),
                ..Default::default()
            },
            ..valid_param()
        };
        validate_param(&param).unwrap();
    }

    #[test]
    fn param_taker_fee_rate_exactly_one_accepted() {
        let param = Param {
            taker_fee_rates: RateSchedule {
                base: Dimensionless::ONE,
                ..Default::default()
            },
            ..valid_param()
        };
        validate_param(&param).unwrap();
    }

    #[test]
    fn param_referrer_commission_exactly_one_accepted() {
        let param = Param {
            referrer_commission_rates: RateSchedule {
                base: Dimensionless::ONE,
                ..Default::default()
            },
            ..valid_param()
        };
        validate_param(&param).unwrap();
    }

    // --------------- validate_param — time/rate upper bounds -------------------

    #[test]
    fn param_funding_period_at_max_accepted() {
        let param = Param {
            funding_period: MAX_FUNDING_PERIOD,
            ..valid_param()
        };
        validate_param(&param).unwrap();
    }

    #[test]
    fn param_funding_period_above_max_rejected() {
        let param = Param {
            funding_period: MAX_FUNDING_PERIOD
                .checked_add(Duration::from_seconds(1))
                .unwrap(),
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`funding_period`"), "{err}");
    }

    #[test]
    fn param_vault_cooldown_at_max_accepted() {
        let param = Param {
            vault_cooldown_period: MAX_VAULT_COOLDOWN_PERIOD,
            ..valid_param()
        };
        validate_param(&param).unwrap();
    }

    #[test]
    fn param_vault_cooldown_above_max_rejected() {
        let param = Param {
            vault_cooldown_period: MAX_VAULT_COOLDOWN_PERIOD
                .checked_add(Duration::from_seconds(1))
                .unwrap(),
            ..valid_param()
        };
        let err = validate_param(&param).unwrap_err().to_string();
        assert!(err.contains("`vault_cooldown_period`"), "{err}");
    }

    // ---------------- validate_pair_param — negative-value branches ------------

    #[test]
    fn pair_param_negative_tick_size_rejected() {
        let p = PairParam {
            tick_size: UsdPrice::new_int(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`tick_size`"), "{err}");
    }

    #[test]
    fn pair_param_negative_min_order_size_rejected() {
        let p = PairParam {
            min_order_size: UsdValue::new_raw(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`min_order_size`"), "{err}");
    }

    #[test]
    fn pair_param_negative_max_abs_oi_rejected() {
        let p = PairParam {
            max_abs_oi: Quantity::new_int(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`max_abs_oi`"), "{err}");
    }

    #[test]
    fn pair_param_mmr_zero_with_nonzero_imr_rejected() {
        // Independently exercises the `mmr > 0` lower bound, so it's not
        // masked by the `imr > 0` check tripping first.
        let p = PairParam {
            initial_margin_ratio: Dimensionless::new_permille(100),
            maintenance_margin_ratio: Dimensionless::ZERO,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`maintenance_margin_ratio`"), "{err}");
        assert!(err.contains("(0, 1]"), "{err}");
    }

    #[test]
    fn pair_param_negative_vault_liquidity_weight_rejected() {
        let p = PairParam {
            vault_liquidity_weight: Dimensionless::new_raw(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`vault_liquidity_weight`"), "{err}");
    }

    #[test]
    fn pair_param_negative_vault_size_skew_factor_rejected() {
        let p = PairParam {
            vault_size_skew_factor: Dimensionless::new_raw(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`vault_size_skew_factor`"), "{err}");
    }

    #[test]
    fn pair_param_negative_vault_spread_skew_factor_rejected() {
        let p = PairParam {
            vault_spread_skew_factor: Dimensionless::new_raw(-1),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`vault_spread_skew_factor`"), "{err}");
        assert!(err.contains(">= 0"), "{err}");
    }

    #[test]
    fn pair_param_negative_bucket_size_rejected() {
        let p = PairParam {
            bucket_sizes: btree_set! { UsdPrice::new_int(-1) },
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`bucket_sizes`"), "{err}");
    }

    #[test]
    fn pair_param_max_abs_funding_rate_above_cap_rejected() {
        let p = PairParam {
            max_abs_funding_rate: FundingRate::new_int(2),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(err.contains("`max_abs_funding_rate`"), "{err}");
    }

    #[test]
    fn pair_param_max_abs_funding_rate_at_cap_accepted() {
        let p = PairParam {
            max_abs_funding_rate: MAX_FUNDING_RATE_CAP,
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    // ------------------ validate_pair_param — happy-path cases -----------------

    #[test]
    fn pair_param_imr_exactly_one_accepted() {
        let p = PairParam {
            initial_margin_ratio: Dimensionless::ONE,
            maintenance_margin_ratio: Dimensionless::new_permille(500), // 50%
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    #[test]
    fn pair_param_nonempty_bucket_sizes_accepted() {
        let p = PairParam {
            bucket_sizes: btree_set! {
                UsdPrice::new_int(1),
                UsdPrice::new_int(10),
                UsdPrice::new_int(100),
            },
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    // --------- validate_pair_param — half_spread × spread_skew_factor ----------

    #[test]
    fn pair_param_half_spread_times_skew_factor_ge_one_rejected() {
        // 0.9 * (1 + 0.5) = 1.35 — pathological: under max positive skew,
        // the bid's effective spread exceeds 100% and the raw bid goes
        // non-positive.
        let p = PairParam {
            vault_half_spread: Dimensionless::new_permille(900),
            vault_spread_skew_factor: Dimensionless::new_permille(500),
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(
            err.contains("vault_half_spread * (1 + vault_spread_skew_factor) < 1"),
            "{err}"
        );
    }

    #[test]
    fn pair_param_half_spread_times_skew_factor_exactly_one_rejected() {
        // Boundary: 0.5 * (1 + 1) = 1.0 — the cross-field invariant is
        // strict (< 1, not <= 1), so this must be rejected.
        let p = PairParam {
            vault_half_spread: Dimensionless::new_permille(500),
            vault_spread_skew_factor: Dimensionless::ONE,
            ..valid_pair_param()
        };
        let err = validate_pair_param(&pair(), &p).unwrap_err().to_string();
        assert!(
            err.contains("vault_half_spread * (1 + vault_spread_skew_factor) < 1"),
            "{err}"
        );
    }

    #[test]
    fn pair_param_half_spread_times_skew_factor_below_one_accepted() {
        // 0.5 * (1 + 0.5) = 0.75 — well below 1.
        // Also bump `max_limit_price_deviation` above the vault's 75%
        // widest-quote deviation so the cross-field invariant added by
        // the banding PR doesn't reject this vault-skew-focused test.
        let p = PairParam {
            vault_half_spread: Dimensionless::new_permille(500),
            vault_spread_skew_factor: Dimensionless::new_permille(500),
            max_limit_price_deviation: Dimensionless::new_permille(800),
            ..valid_pair_param()
        };
        validate_pair_param(&pair(), &p).unwrap();
    }

    // ------------------- validate_vault_total_weight ----------------------------

    #[test]
    fn vault_total_weight_matches_sum_single_pair() {
        let param = Param {
            vault_total_weight: Dimensionless::new_int(3),
            ..valid_param()
        };
        let pp = btree_map! {
            pair() => PairParam {
                vault_liquidity_weight: Dimensionless::new_int(3),
                ..valid_pair_param()
            },
        };
        validate_vault_total_weight(&param, &pp).unwrap();
    }

    #[test]
    fn vault_total_weight_matches_sum_multi_pair() {
        let eth: PairId = "perp/ethusd".parse().unwrap();
        let btc: PairId = "perp/btcusd".parse().unwrap();
        let param = Param {
            vault_total_weight: Dimensionless::new_int(3),
            ..valid_param()
        };
        let pp = btree_map! {
            eth => PairParam {
                vault_liquidity_weight: Dimensionless::new_int(1),
                ..valid_pair_param()
            },
            btc => PairParam {
                vault_liquidity_weight: Dimensionless::new_int(2),
                ..valid_pair_param()
            },
        };
        validate_vault_total_weight(&param, &pp).unwrap();
    }

    #[test]
    fn vault_total_weight_empty_pairs_matches_zero() {
        let param = Param {
            vault_total_weight: Dimensionless::ZERO,
            ..valid_param()
        };
        let pp: BTreeMap<PairId, PairParam> = BTreeMap::new();
        validate_vault_total_weight(&param, &pp).unwrap();
    }

    #[test]
    fn vault_total_weight_greater_than_sum_rejected() {
        let param = Param {
            vault_total_weight: Dimensionless::new_int(2),
            ..valid_param()
        };
        let pp = btree_map! {
            pair() => PairParam {
                vault_liquidity_weight: Dimensionless::new_int(1),
                ..valid_pair_param()
            },
        };
        let err = validate_vault_total_weight(&param, &pp)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`vault_total_weight`"), "{err}");
        assert!(err.contains("expected: 1"), "{err}");
        assert!(err.contains("found: 2"), "{err}");
    }

    #[test]
    fn vault_total_weight_less_than_sum_rejected() {
        let eth: PairId = "perp/ethusd".parse().unwrap();
        let btc: PairId = "perp/btcusd".parse().unwrap();
        let param = Param {
            vault_total_weight: Dimensionless::new_int(1),
            ..valid_param()
        };
        let pp = btree_map! {
            eth => PairParam {
                vault_liquidity_weight: Dimensionless::new_int(1),
                ..valid_pair_param()
            },
            btc => PairParam {
                vault_liquidity_weight: Dimensionless::new_int(1),
                ..valid_pair_param()
            },
        };
        let err = validate_vault_total_weight(&param, &pp)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`vault_total_weight`"), "{err}");
        assert!(err.contains("expected: 2"), "{err}");
        assert!(err.contains("found: 1"), "{err}");
    }

    #[test]
    fn vault_total_weight_nonzero_with_empty_pairs_rejected() {
        // If you bump the total but forget to supply any pair_params,
        // the check catches it.
        let param = Param {
            vault_total_weight: Dimensionless::new_int(1),
            ..valid_param()
        };
        let pp: BTreeMap<PairId, PairParam> = BTreeMap::new();
        let err = validate_vault_total_weight(&param, &pp)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`vault_total_weight`"), "{err}");
    }
}
