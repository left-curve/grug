//! Chain-exact rewrite of the frontend `usePerpsMaxSize` formula, used
//! purely for test verification that the frontend math lines up exactly
//! with `check_margin`'s acceptance boundary.
//!
//! The frontend version in `ui/store/src/hooks/usePerpsMaxSize.ts` takes a
//! user-selected UI `leverage` and uses `1/L` for the *current* pair's IM
//! term. When `L = L_max = 1 / IMR_pair`, that coincides with `IMR_pair`
//! and the formulas agree. This module uses `IMR_pair` directly (i.e.
//! always the `L = L_max` case) so the `×1.001` boundary test is clean.

use {
    crate::{core::compute_user_equity, querier::NoCachePerpQuerier},
    dango_math::MathResult,
    dango_order_book::{Dimensionless, PairId, Quantity, UsdValue},
    dango_types::perps::UserState,
};

/// Order side entering the trade form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

/// Computing the "available to trade" amount on the frontend.
///
/// This logic is not used in the contract at all -- only in the frontend,
/// implemented in TypeScript. This function is for verifying the correctness
/// of the logic.
///
/// Variables used below:
///
/// - `equity`: the user's equity across all open positions, as returned by
///   [`compute_user_equity`]. Equals
///   `user_state.margin + Σ unrealized_pnl − Σ accrued_funding`.
/// - `pos_j`: the user's signed base-unit position in pair `j`, i.e.
///   `user_state.positions[j].size`. Positive = long, negative = short.
/// - `pos_current`: shorthand for `pos_j` where `j == current_pair_id`.
///   Treated as zero when the user has no position in the traded pair.
/// - `price_j`: the **oracle price** for pair `j`, as returned by
///   [`OracleQuerier::query_price`]. Dango uses the oracle price
///   as the mark price, so this is the same number the frontend reads off
///   `allPerpsPairStatsStore[pid].currentPrice`. It is neither the
///   orderbook best-bid/ask nor the position's entry price.
/// - `price`: shorthand for `price_j` where `j == current_pair_id`.
/// - `IMR_pair_j`: the **fixed** initial-margin ratio for pair `j` from
///   [`PairParam::initial_margin_ratio`]. It is a per-pair chain constant
///   (equal to `1 / max_leverage_for_pair`) and does **not** depend on any
///   UI leverage slider.
/// - `IMR_pair`: shorthand for `IMR_pair_j` where `j == current_pair_id`.
/// - `reserved_margin`: the user's total margin locked by open GTC limit
///   orders across all pairs and both sides, `user_state.reserved_margin`.
/// - "order opposes the position": the order side differs from the sign
///   of `pos_current` (buy against a short, sell against a long).
///   Otherwise the order adds to the position (same side).
///
/// ```plain
/// otherIM = Σ(j ≠ current_pair_id) |pos_j| · price_j · IMR_pair_j
///
/// currentTerm =
///   0                                         if pos_current = 0
///   + |pos_current| · price · IMR_pair        if order opposes the position
///   − |pos_current| · price · IMR_pair        if order adds to the position
///
/// availToTrade = equity + currentTerm − otherIM − reserved_margin
/// ```
///
/// Clamping is deliberately skipped here (the caller can clamp) so callers
/// that want to reason about the raw signed value can do so.
pub fn compute_available_to_trade(
    perp_querier: &NoCachePerpQuerier,
    user_state: &UserState,
    current_pair_id: &PairId,
    action: Side,
) -> anyhow::Result<UsdValue> {
    let equity = compute_user_equity(perp_querier, user_state)?;

    let mut other_im = UsdValue::ZERO;
    for (pair_id, position) in &user_state.positions {
        if pair_id == current_pair_id {
            continue;
        }

        if position.size.is_zero() {
            continue;
        }

        let price = perp_querier.query_pair_state(pair_id)?.index_price;
        let pair_param = perp_querier.query_pair_param(pair_id)?;
        let im = position
            .size
            .checked_abs()?
            .checked_mul(price)?
            .checked_mul(pair_param.initial_margin_ratio)?;

        other_im.checked_add_assign(im)?;
    }

    let current_pos = user_state
        .positions
        .get(current_pair_id)
        .map(|p| p.size)
        .unwrap_or(Quantity::ZERO);

    let current_term = if current_pos.is_zero() {
        UsdValue::ZERO
    } else {
        let price = perp_querier.query_pair_state(current_pair_id)?.index_price;
        let pair_param = perp_querier.query_pair_param(current_pair_id)?;
        let abs_im = current_pos
            .checked_abs()?
            .checked_mul(price)?
            .checked_mul(pair_param.initial_margin_ratio)?;

        let pos_is_long = current_pos.is_positive();
        let order_is_buy = matches!(action, Side::Buy);
        let is_opposing = pos_is_long != order_is_buy;

        if is_opposing {
            abs_im
        } else {
            abs_im.checked_neg()?
        }
    };

    Ok(equity
        .checked_add(current_term)?
        .checked_sub(other_im)?
        .checked_sub(user_state.reserved_margin)?)
}

/// ```plain
/// max_notional = max(availToTrade, 0) / (IMR_pair + fee)
/// ```
pub fn compute_max_order_notional(
    avail_to_trade: UsdValue,
    pair_imr: Dimensionless,
    fee: Dimensionless,
) -> MathResult<UsdValue> {
    let avail_clamped = if avail_to_trade.is_negative() {
        UsdValue::ZERO
    } else {
        avail_to_trade
    };

    let denominator = pair_imr.checked_add(fee)?;

    avail_clamped.checked_div(denominator)
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::core::check_margin,
        dango_order_book::{FundingPerUnit, UsdPrice},
        dango_primitives::{btree_map, hash_map},
        dango_types::{
            constants::{perp_btc, perp_eth},
            perps::{PairParam, PairState, Position},
        },
        test_case::test_case,
    };

    /// Static inputs shared by every test case. Chosen so that every
    /// test case's `availToTrade` stays comfortably positive and the
    /// `×1.001` bump is well above `Dec128_6` rounding noise.
    const USER_MARGIN: UsdValue = UsdValue::new_int(20_000);
    const CURRENT_PRICE: UsdPrice = UsdPrice::new_int(2_000); // ETH = $2000
    const OTHER_PRICE: UsdPrice = UsdPrice::new_int(50_000); // BTC = $50_000
    const CURRENT_IMR: Dimensionless = Dimensionless::new_percent(10);
    const OTHER_IMR: Dimensionless = Dimensionless::new_percent(5);
    const FEE: Dimensionless = Dimensionless::new_raw(450); // 0.045%
    const RESERVED_WHEN_SET: UsdValue = UsdValue::new_int(500); // when `has_orders`

    fn build_setup(
        current_pos: i128,
        other_pos: i128,
        has_orders: bool,
    ) -> (UserState, NoCachePerpQuerier<'static>) {
        let mut positions = btree_map! {};
        if current_pos != 0 {
            positions.insert(
                perp_eth::DENOM.clone(),
                Position {
                    size: Quantity::new_int(current_pos),
                    // entry_price = oracle so unrealized pnl is zero
                    entry_price: CURRENT_PRICE,
                    entry_funding_per_unit: FundingPerUnit::new_int(0),
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            );
        }
        if other_pos != 0 {
            positions.insert(
                perp_btc::DENOM.clone(),
                Position {
                    size: Quantity::new_int(other_pos),
                    entry_price: OTHER_PRICE,
                    entry_funding_per_unit: FundingPerUnit::new_int(0),
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            );
        }

        let reserved = if has_orders {
            RESERVED_WHEN_SET
        } else {
            UsdValue::ZERO
        };

        let user_state = UserState {
            margin: USER_MARGIN,
            positions,
            reserved_margin: reserved,
            ..Default::default()
        };

        let perp_querier = NoCachePerpQuerier::new_mock(
            hash_map! {
                perp_eth::DENOM.clone() => PairParam {
                    initial_margin_ratio: CURRENT_IMR,
                    ..Default::default()
                },
                perp_btc::DENOM.clone() => PairParam {
                    initial_margin_ratio: OTHER_IMR,
                    ..Default::default()
                },
            },
            hash_map! {
                perp_eth::DENOM.clone() => PairState {
                    funding_per_unit: FundingPerUnit::new_int(0),
                    index_price: UsdPrice::new_int(2_000),
                    ..Default::default()
                },
                perp_btc::DENOM.clone() => PairState {
                    funding_per_unit: FundingPerUnit::new_int(0),
                    index_price: UsdPrice::new_int(50_000),
                    ..Default::default()
                },
            },
        );

        (user_state, perp_querier)
    }

    /// Full cartesian matrix — 2 × 3 × 3 × 2 = **36** cases — covering
    /// buy/sell × {no / long / short in current pair} × {no / long / short
    /// in other pair} × {no / with open orders}.
    ///
    /// For each case, solve `compute_max_order_notional` at exactly the
    /// boundary and prove:
    ///
    /// 1. `check_margin(size)` accepts.
    /// 2. `check_margin(size × 1.001)` rejects.
    ///
    /// Reduce-only is deliberately omitted: `check_margin` is skipped for
    /// reduce-only on-chain, so the boundary paradigm doesn't apply.
    #[test_case(Side::Buy,   0,  0, false ; "buy  | no pos    | no other    | no orders")]
    #[test_case(Side::Buy,   0,  0, true  ; "buy  | no pos    | no other    | with orders")]
    #[test_case(Side::Buy,   0,  1, false ; "buy  | no pos    | long other  | no orders")]
    #[test_case(Side::Buy,   0,  1, true  ; "buy  | no pos    | long other  | with orders")]
    #[test_case(Side::Buy,   0, -1, false ; "buy  | no pos    | short other | no orders")]
    #[test_case(Side::Buy,   0, -1, true  ; "buy  | no pos    | short other | with orders")]
    #[test_case(Side::Buy,   1,  0, false ; "buy  | long pos  | no other    | no orders")]
    #[test_case(Side::Buy,   1,  0, true  ; "buy  | long pos  | no other    | with orders")]
    #[test_case(Side::Buy,   1,  1, false ; "buy  | long pos  | long other  | no orders")]
    #[test_case(Side::Buy,   1,  1, true  ; "buy  | long pos  | long other  | with orders")]
    #[test_case(Side::Buy,   1, -1, false ; "buy  | long pos  | short other | no orders")]
    #[test_case(Side::Buy,   1, -1, true  ; "buy  | long pos  | short other | with orders")]
    #[test_case(Side::Buy,  -1,  0, false ; "buy  | short pos | no other    | no orders")]
    #[test_case(Side::Buy,  -1,  0, true  ; "buy  | short pos | no other    | with orders")]
    #[test_case(Side::Buy,  -1,  1, false ; "buy  | short pos | long other  | no orders")]
    #[test_case(Side::Buy,  -1,  1, true  ; "buy  | short pos | long other  | with orders")]
    #[test_case(Side::Buy,  -1, -1, false ; "buy  | short pos | short other | no orders")]
    #[test_case(Side::Buy,  -1, -1, true  ; "buy  | short pos | short other | with orders")]
    #[test_case(Side::Sell,  0,  0, false ; "sell | no pos    | no other    | no orders")]
    #[test_case(Side::Sell,  0,  0, true  ; "sell | no pos    | no other    | with orders")]
    #[test_case(Side::Sell,  0,  1, false ; "sell | no pos    | long other  | no orders")]
    #[test_case(Side::Sell,  0,  1, true  ; "sell | no pos    | long other  | with orders")]
    #[test_case(Side::Sell,  0, -1, false ; "sell | no pos    | short other | no orders")]
    #[test_case(Side::Sell,  0, -1, true  ; "sell | no pos    | short other | with orders")]
    #[test_case(Side::Sell,  1,  0, false ; "sell | long pos  | no other    | no orders")]
    #[test_case(Side::Sell,  1,  0, true  ; "sell | long pos  | no other    | with orders")]
    #[test_case(Side::Sell,  1,  1, false ; "sell | long pos  | long other  | no orders")]
    #[test_case(Side::Sell,  1,  1, true  ; "sell | long pos  | long other  | with orders")]
    #[test_case(Side::Sell,  1, -1, false ; "sell | long pos  | short other | no orders")]
    #[test_case(Side::Sell,  1, -1, true  ; "sell | long pos  | short other | with orders")]
    #[test_case(Side::Sell, -1,  0, false ; "sell | short pos | no other    | no orders")]
    #[test_case(Side::Sell, -1,  0, true  ; "sell | short pos | no other    | with orders")]
    #[test_case(Side::Sell, -1,  1, false ; "sell | short pos | long other  | no orders")]
    #[test_case(Side::Sell, -1,  1, true  ; "sell | short pos | long other  | with orders")]
    #[test_case(Side::Sell, -1, -1, false ; "sell | short pos | short other | no orders")]
    #[test_case(Side::Sell, -1, -1, true  ; "sell | short pos | short other | with orders")]
    fn max_size_is_at_check_margin_boundary(
        order_side: Side,
        current_pos: i128,
        other_pos: i128,
        has_orders: bool,
    ) {
        let (user_state, perp_querier) = build_setup(current_pos, other_pos, has_orders);
        let current_pair_id = perp_eth::DENOM.clone();

        // 1. Available to trade, then max notional, then max base size.
        let avail =
            compute_available_to_trade(&perp_querier, &user_state, &current_pair_id, order_side)
                .expect("avail should compute");

        assert!(
            !avail.is_negative(),
            "avail should stay non-negative for chosen test parameters; got {}",
            avail,
        );

        let pair_param = perp_querier.query_pair_param(&current_pair_id).unwrap();
        let max_notional = compute_max_order_notional(avail, pair_param.initial_margin_ratio, FEE)
            .expect("max notional should compute");

        // max_notional (USD) / oracle_price (USD/unit) = |size| (units)
        let max_size_abs: Quantity = max_notional
            .checked_div(CURRENT_PRICE)
            .expect("max size should compute");

        assert!(
            !max_size_abs.is_zero(),
            "boundary case should have non-zero max size",
        );

        let max_size_signed = match order_side {
            Side::Buy => max_size_abs,
            Side::Sell => max_size_abs.checked_neg().unwrap(),
        };

        // 2. Boundary order: check_margin should accept.
        check_margin(
            &current_pair_id,
            &perp_querier,
            &user_state,
            FEE,
            CURRENT_PRICE,
            max_size_signed,
        )
        .expect("check_margin should accept the boundary order");

        // 3. Bump the order size by 0.1% (via permille multiplier); check_margin
        //    should reject.
        let bump = Dimensionless::new_permille(1_001);
        let bumped = max_size_signed
            .checked_mul(bump)
            .expect("bumped size should compute");

        let bumped_res = check_margin(
            &current_pair_id,
            &perp_querier,
            &user_state,
            FEE,
            CURRENT_PRICE,
            bumped,
        );

        assert!(
            bumped_res.is_err(),
            "check_margin should reject the bumped (×1.001) order; got Ok. \
             avail={} max_notional={} max_size={} bumped={}",
            avail,
            max_notional,
            max_size_signed,
            bumped,
        );
    }
}
