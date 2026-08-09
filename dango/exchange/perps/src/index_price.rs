use {
    crate::{
        MAX_ORACLE_STALENESS,
        core::compute_ewma_index_price,
        state::{PAIR_IDS, PAIR_PARAMS, PAIR_STATES},
    },
    dango_oracle::OracleQuerier,
    dango_order_book::{ASKS, BIDS, Dimensionless, PairId, compute_impact_price, may_invert_price},
    dango_primitives::{Order as IterationOrder, Storage, Timestamp},
    dango_pyth_types::MarketSession,
    dango_types::{
        oracle::Price,
        perps::{PairParam, PairState},
    },
};

/// Update `PairState::index_price` for every active pair.
///
/// When the oracle price is available (regular market session, fresh
/// timestamp), the index price snaps to the oracle price. When it is
/// unavailable (market closed, stale feed), the index price drifts via
/// the EWMA mechanism driven by impact bid/ask from the order book.
pub fn process_index_price(
    storage: &mut dyn Storage,
    current_time: Timestamp,
    oracle_querier: &mut OracleQuerier,
) -> anyhow::Result<()> {
    let pair_ids = PAIR_IDS.load(storage)?;

    for pair_id in pair_ids {
        let pair_param = PAIR_PARAMS.load(storage, &pair_id)?;
        let mut pair_state = PAIR_STATES.load(storage, &pair_id)?;

        let price = oracle_querier.query_price(&pair_id, None);

        process_index_price_for_pair(
            storage,
            current_time,
            &pair_id,
            &pair_param,
            &mut pair_state,
            price,
        )?;

        PAIR_STATES.save(storage, &pair_id, &pair_state)?;

        #[cfg(feature = "tracing")]
        {
            tracing::info!(
                %pair_id,
                index_price = %pair_state.index_price,
                "Updated index price"
            );
        }
    }

    Ok(())
}

fn process_index_price_for_pair(
    storage: &dyn Storage,
    current_time: Timestamp,
    pair_id: &PairId,
    pair_param: &PairParam,
    pair_state: &mut PairState,
    price: anyhow::Result<Price>,
) -> anyhow::Result<()> {
    let index_price = match &price {
        // The oracle is considered available when the market is in regular
        // session (not pre/post/closed) and the price is fresh enough.
        // When available, snap to the oracle price.
        Ok(p)
            if p.market_session == MarketSession::Regular
                && p.timestamp >= current_time - MAX_ORACLE_STALENESS =>
        {
            // Fresh regular-session oracle: snap the mark to it and record it
            // as the anchor for the price band and the closed-session drift
            // bound below.
            pair_state.oracle_price = p.humanized_price;
            pair_state.last_oracle_time = p.timestamp;
            p.humanized_price
        },
        // When oracle is not available, use EWMA driven by the order book's
        // impact bid/ask spread.
        _ => {
            #[cfg(feature = "tracing")]
            match price {
                Ok(p) => {
                    tracing::warn!(
                        %pair_id,
                        price = %p.humanized_price,
                        market_session = ?p.market_session,
                        timestamp = p.timestamp.to_rfc3339_string(),
                        "Oracle unavailable; using EWMA"
                    );
                },
                Err(err) => {
                    tracing::warn!(
                        %pair_id,
                        %err,
                        "Oracle query failed; using EWMA"
                    );
                },
            }

            let bid_iter = BIDS
                .prefix(pair_id.clone())
                .range(storage, None, None, IterationOrder::Ascending)
                .map(|res| {
                    let ((stored_price, _), order) = res?;
                    let real_price = may_invert_price(stored_price, true);
                    Ok((real_price, order.size))
                });

            let ask_iter = ASKS
                .prefix(pair_id.clone())
                .range(storage, None, None, IterationOrder::Ascending)
                .map(|res| {
                    let ((stored_price, _), order) = res?;
                    Ok((stored_price, order.size.checked_abs()?))
                });

            let impact_bid = compute_impact_price(bid_iter, pair_param.impact_size)?;
            let impact_ask = compute_impact_price(ask_iter, pair_param.impact_size)?;

            let delta_t = current_time - pair_state.last_index_time;

            let ewma =
                compute_ewma_index_price(pair_state.index_price, impact_bid, impact_ask, delta_t)?;

            // Bound closed-session drift to ±(1 / max_leverage) =
            // ±`initial_margin_ratio` of the last oracle price, so the book
            // cannot walk the mark arbitrarily far from the true price while
            // the market is closed.
            let imr = pair_param.initial_margin_ratio;
            let lower = pair_state
                .oracle_price
                .checked_mul(Dimensionless::ONE.checked_sub(imr)?)?;
            let upper = pair_state
                .oracle_price
                .checked_mul(Dimensionless::ONE.checked_add(imr)?)?;

            if ewma < lower {
                lower
            } else if ewma > upper {
                upper
            } else {
                ewma
            }
        },
    };

    pair_state.index_price = index_price;
    pair_state.last_index_time = current_time;

    Ok(())
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        anyhow::anyhow,
        dango_math::Uint64,
        dango_order_book::{Dimensionless, LimitOrder, OrderKey, Quantity, UsdPrice, UsdValue},
        dango_primitives::{Addr, Duration, MockStorage},
    };

    const T: Timestamp = Timestamp::from_seconds(1_700_000_000);
    const MAKER: Addr = Addr::mock(99);

    fn btc_pair_id() -> PairId {
        "perp/btcusd".parse().unwrap()
    }

    fn default_pair_param() -> PairParam {
        PairParam {
            impact_size: UsdValue::new_int(10_000),
            // Wide bound so the closed-session drift clamp stays inert for these
            // EWMA-mechanics tests; the clamp is covered by dedicated cases in
            // the `e*` group below.
            initial_margin_ratio: Dimensionless::new_permille(500),
            ..Default::default()
        }
    }

    fn save_bid(
        storage: &mut dyn Storage,
        pair_id: &PairId,
        order_id: u64,
        price: i128,
        size: i128,
    ) {
        let stored_price = may_invert_price(UsdPrice::new_int(price), true);
        let key: OrderKey = (pair_id.clone(), stored_price, Uint64::new(order_id));
        BIDS.save(
            storage,
            key,
            &LimitOrder {
                user: MAKER,
                size: Quantity::new_int(size),
                reduce_only: false,
                reserved_margin: UsdValue::ZERO,
                created_at: Timestamp::ZERO,
                tp: None,
                sl: None,
                client_order_id: None,
            },
        )
        .unwrap();
    }

    fn save_ask(
        storage: &mut dyn Storage,
        pair_id: &PairId,
        order_id: u64,
        price: i128,
        size: i128,
    ) {
        let key: OrderKey = (
            pair_id.clone(),
            UsdPrice::new_int(price),
            Uint64::new(order_id),
        );
        ASKS.save(
            storage,
            key,
            &LimitOrder {
                user: MAKER,
                size: Quantity::new_int(size),
                reduce_only: false,
                reserved_margin: UsdValue::ZERO,
                created_at: Timestamp::ZERO,
                tp: None,
                sl: None,
                client_order_id: None,
            },
        )
        .unwrap();
    }

    // ---- Oracle availability decision ----

    /// When the oracle reports a Regular market session and the price timestamp
    /// is within the staleness window, the index price must snap to the oracle
    /// price regardless of the order book state.
    #[test]
    fn a1_regular_fresh_snaps_to_oracle() {
        let storage = MockStorage::new();
        let pair_id = btc_pair_id();
        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        let price = Ok(Price::new(
            UsdPrice::new_int(105),
            T,
            MarketSession::Regular,
        ));

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            price,
        )
        .unwrap();

        assert_eq!(pair_state.index_price, UsdPrice::new_int(105));
        assert_eq!(pair_state.last_index_time, T);
    }

    /// A price whose timestamp is exactly MAX_ORACLE_STALENESS old is still
    /// considered fresh (the comparison is `>=`). Verify the boundary is
    /// inclusive.
    #[test]
    fn a2_staleness_boundary_inclusive() {
        let storage = MockStorage::new();
        let pair_id = btc_pair_id();
        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        let price = Ok(Price::new(
            UsdPrice::new_int(105),
            T - MAX_ORACLE_STALENESS,
            MarketSession::Regular,
        ));

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            price,
        )
        .unwrap();

        assert_eq!(pair_state.index_price, UsdPrice::new_int(105));
    }

    /// A Regular-session price that is 1 ms beyond the staleness window is
    /// treated as unavailable. The index must NOT snap to the oracle; instead
    /// EWMA runs. With an empty book the index stays put.
    #[test]
    fn a3_regular_stale_falls_through_to_ewma() {
        let storage = MockStorage::new();
        let pair_id = btc_pair_id();
        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        let price = Ok(Price::new(
            UsdPrice::new_int(105),
            T - MAX_ORACLE_STALENESS - Duration::from_millis(1),
            MarketSession::Regular,
        ));

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            price,
        )
        .unwrap();

        // Empty book -> IPD=0 -> no movement. Crucially, does NOT snap to 105.
        assert_eq!(pair_state.index_price, UsdPrice::new_int(100));
    }

    /// When the market session is Other (closed/pre/post), the oracle is
    /// unavailable even if the timestamp is perfectly fresh. The index must
    /// NOT snap to the oracle price.
    #[test]
    fn a4_other_session_uses_ewma_not_snap() {
        let storage = MockStorage::new();
        let pair_id = btc_pair_id();
        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        let price = Ok(Price::new(UsdPrice::new_int(200), T, MarketSession::Other));

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            price,
        )
        .unwrap();

        // Must NOT snap to 200. Empty book -> no movement.
        assert_eq!(pair_state.index_price, UsdPrice::new_int(100));
    }

    /// When the oracle query itself fails (network error, missing price
    /// source), the EWMA path runs as a graceful fallback.
    #[test]
    fn a5_oracle_query_failure_uses_ewma() {
        let storage = MockStorage::new();
        let pair_id = btc_pair_id();
        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        let price: anyhow::Result<Price> = Err(anyhow!("connection failed"));

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            price,
        )
        .unwrap();

        assert_eq!(pair_state.index_price, UsdPrice::new_int(100));
    }

    /// When the market reopens after a period of EWMA drift, the index must
    /// snap back to the oracle price immediately with no smoothing — the
    /// external feed is authoritative.
    #[test]
    fn a6_reopen_snaps_back_from_drift() {
        let storage = MockStorage::new();
        let pair_id = btc_pair_id();
        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(110),
            oracle_price: UsdPrice::new_int(110),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        let price = Ok(Price::new(
            UsdPrice::new_int(102),
            T,
            MarketSession::Regular,
        ));

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            price,
        )
        .unwrap();

        assert_eq!(pair_state.index_price, UsdPrice::new_int(102));
    }

    // ---- EWMA with order book ----

    fn other_price() -> anyhow::Result<Price> {
        Ok(Price::new(UsdPrice::new_int(999), T, MarketSession::Other))
    }

    /// When the index sits between the impact bid and impact ask, IPD is zero
    /// and the index does not move — the order book is consistent with the
    /// current price.
    #[test]
    fn b1_index_inside_spread_no_movement() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        // bid@100 with sufficient depth (size 200 @ 100 = 20,000 >= 10,000)
        save_bid(&mut storage, &pair_id, 1, 100, 200);
        // ask@106 with sufficient depth (size 200 @ 106 = 21,200 >= 10,000)
        save_ask(&mut storage, &pair_id, 2, 106, -200);

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(103),
            oracle_price: UsdPrice::new_int(103),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            other_price(),
        )
        .unwrap();

        assert_eq!(pair_state.index_price, UsdPrice::new_int(103));
    }

    /// When the index is below the impact bid, the order book's bid side has
    /// moved past the index — buyers are willing to pay more. The index must
    /// drift upward.
    #[test]
    fn b2_index_below_bid_drifts_up() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        save_bid(&mut storage, &pair_id, 1, 100, 200);
        save_ask(&mut storage, &pair_id, 2, 106, -200);

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(95),
            oracle_price: UsdPrice::new_int(95),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            other_price(),
        )
        .unwrap();

        // S = 95, impact_bid = 100, impact_ask = 106, dt = 3s, α = raw(1665)
        // IPD = max(100 − 95, 0) − max(95 − 106, 0) = 5
        // S_new = 95 + 1665 × 5_000_000 / 1_000_000 = raw(95_008_325)
        assert_eq!(pair_state.index_price, UsdPrice::new_raw(95_008_325));
    }

    /// When the index is above the impact ask, sellers are willing to sell
    /// below the index. The index must drift downward.
    #[test]
    fn b3_index_above_ask_drifts_down() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        save_bid(&mut storage, &pair_id, 1, 100, 200);
        save_ask(&mut storage, &pair_id, 2, 106, -200);

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(112),
            oracle_price: UsdPrice::new_int(112),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            other_price(),
        )
        .unwrap();

        // S = 112, impact_bid = 100 < S → 0, impact_ask = 106, dt = 3s, α = raw(1665)
        // IPD = 0 − (112 − 106) = −6
        // S_new = 112 + 1665 × (−6_000_000) / 1_000_000 = raw(111_990_010)
        assert_eq!(pair_state.index_price, UsdPrice::new_raw(111_990_010));
    }

    /// When the bid side has insufficient depth to fill the impact notional,
    /// its contribution to IPD is zero. Only the ask side pulls the index.
    #[test]
    fn b4_bid_insufficient_only_ask_pulls() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        // Tiny bid: size 1 @ 100 = notional 100 < impact_size 10,000
        save_bid(&mut storage, &pair_id, 1, 100, 1);
        // Sufficient ask
        save_ask(&mut storage, &pair_id, 2, 106, -200);

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(112),
            oracle_price: UsdPrice::new_int(112),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            other_price(),
        )
        .unwrap();

        // bid insufficient (notional 100 < 10_000) → impact_bid = None
        // impact_ask = 106, S = 112, IPD = 0 − (112 − 106) = −6
        // S_new = 112 + 1665 × (−6_000_000) / 1_000_000 = raw(111_990_010)
        assert_eq!(pair_state.index_price, UsdPrice::new_raw(111_990_010));
    }

    /// When the ask side has insufficient depth, its contribution to IPD is
    /// zero. Only the bid side pulls the index.
    #[test]
    fn b5_ask_insufficient_only_bid_pulls() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        // Sufficient bid
        save_bid(&mut storage, &pair_id, 1, 100, 200);
        // Tiny ask: size 1 @ 106 = notional 106 < impact_size 10,000
        save_ask(&mut storage, &pair_id, 2, 106, -1);

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(95),
            oracle_price: UsdPrice::new_int(95),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            other_price(),
        )
        .unwrap();

        // ask insufficient (notional 106 < 10_000) → impact_ask = None
        // impact_bid = 100, S = 95, IPD = (100 − 95) − 0 = 5
        // S_new = 95 + 1665 × 5_000_000 / 1_000_000 = raw(95_008_325)
        assert_eq!(pair_state.index_price, UsdPrice::new_raw(95_008_325));
    }

    /// When both sides of the book have insufficient depth, neither side
    /// produces a reliable impact price, so IPD is zero and the index holds.
    #[test]
    fn b6_both_insufficient_no_movement() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        save_bid(&mut storage, &pair_id, 1, 100, 1);
        save_ask(&mut storage, &pair_id, 2, 106, -1);

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            other_price(),
        )
        .unwrap();

        assert_eq!(pair_state.index_price, UsdPrice::new_int(100));
    }

    /// A completely empty order book is the degenerate case of both sides
    /// having no depth — the index stays put.
    #[test]
    fn b7_empty_book_no_movement() {
        let storage = MockStorage::new();
        let pair_id = btc_pair_id();

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &default_pair_param(),
            &mut pair_state,
            other_price(),
        )
        .unwrap();

        assert_eq!(pair_state.index_price, UsdPrice::new_int(100));
    }

    // ---- Transition sequences ----

    /// Full close-then-reopen cycle: snap to oracle, drift via EWMA for two
    /// ticks while the market is closed, then snap back to a new oracle price
    /// when the market reopens. The snap-back must be exact — no residual
    /// smoothing from the accumulated drift.
    #[test]
    fn d1_close_then_reopen_snaps_back() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();
        let pair_param = default_pair_param();
        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(50),
            last_index_time: T - Duration::from_seconds(10),
            ..Default::default()
        };

        // Step 1: Regular@100 -> snap
        let t1 = T;
        let price = Ok(Price::new(
            UsdPrice::new_int(100),
            t1,
            MarketSession::Regular,
        ));
        process_index_price_for_pair(&storage, t1, &pair_id, &pair_param, &mut pair_state, price)
            .unwrap();
        assert_eq!(pair_state.index_price, UsdPrice::new_int(100));

        // Place orders: bid@105, ask@110
        save_bid(&mut storage, &pair_id, 1, 105, 200);
        save_ask(&mut storage, &pair_id, 2, 110, -200);

        // Step 2: Other, 3s later -> EWMA drift up
        let t2 = t1 + Duration::from_seconds(3);
        let price = Ok(Price::new(UsdPrice::new_int(999), t2, MarketSession::Other));
        process_index_price_for_pair(&storage, t2, &pair_id, &pair_param, &mut pair_state, price)
            .unwrap();
        // S = 100, bid = 105, ask = 110, dt = 3s, α = raw(1665)
        // IPD = (105 − 100) − 0 = 5
        // S_new = 100 + 1665 × 5_000_000 / 1_000_000 = raw(100_008_325)
        let after_step2 = pair_state.index_price;
        assert_eq!(after_step2, UsdPrice::new_raw(100_008_325));

        // Step 3: Other, 3s later -> further drift
        let t3 = t2 + Duration::from_seconds(3);
        let price = Ok(Price::new(UsdPrice::new_int(999), t3, MarketSession::Other));
        process_index_price_for_pair(&storage, t3, &pair_id, &pair_param, &mut pair_state, price)
            .unwrap();
        // S = 100.008325 (raw 100_008_325), bid = 105
        // IPD = 105_000_000 − 100_008_325 = 4_991_675
        // delta = 1665 × 4_991_675 / 1_000_000 = 8_311 (truncated)
        // S_new = 100_008_325 + 8_311 = raw(100_016_636)
        assert_eq!(pair_state.index_price, UsdPrice::new_raw(100_016_636));

        // Step 4: Regular@98 -> snap back exactly
        let t4 = t3 + Duration::from_seconds(3);
        let price = Ok(Price::new(
            UsdPrice::new_int(98),
            t4,
            MarketSession::Regular,
        ));
        process_index_price_for_pair(&storage, t4, &pair_id, &pair_param, &mut pair_state, price)
            .unwrap();
        assert_eq!(pair_state.index_price, UsdPrice::new_int(98));
    }

    /// A Regular-session price becomes stale when the block timestamp advances
    /// past the staleness window without a new oracle feed. The second call
    /// must fall through to EWMA even though the session is still Regular.
    #[test]
    fn d2_regular_goes_stale() {
        let storage = MockStorage::new();
        let pair_id = btc_pair_id();
        let pair_param = default_pair_param();
        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(50),
            last_index_time: T - Duration::from_seconds(10),
            ..Default::default()
        };

        // Step 1: Regular fresh@100 -> snap
        let t1 = T;
        let price = Ok(Price::new(
            UsdPrice::new_int(100),
            t1,
            MarketSession::Regular,
        ));
        process_index_price_for_pair(&storage, t1, &pair_id, &pair_param, &mut pair_state, price)
            .unwrap();
        assert_eq!(pair_state.index_price, UsdPrice::new_int(100));

        // Step 2: Same price object but current_time advances past staleness.
        // Timestamp t1 is now stale relative to t2.
        let t2 = t1 + Duration::from_seconds(1);
        let price = Ok(Price::new(
            UsdPrice::new_int(100),
            t1,
            MarketSession::Regular,
        ));
        process_index_price_for_pair(&storage, t2, &pair_id, &pair_param, &mut pair_state, price)
            .unwrap();

        // Empty book, EWMA with no depth -> holds at 100.
        assert_eq!(pair_state.index_price, UsdPrice::new_int(100));
    }

    /// Five consecutive EWMA ticks with the index below the impact bid.
    /// Each tick's output feeds into the next as input. The drift must be
    /// monotonically increasing (toward the bid) and each per-tick increment
    /// must be smaller than the previous one — because the IPD shrinks as
    /// the index approaches the bid.
    #[test]
    fn d3_accumulated_drift_monotonic_decreasing_increments() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();
        let pair_param = default_pair_param();

        // bid@110, ask@115
        save_bid(&mut storage, &pair_id, 1, 110, 200);
        save_ask(&mut storage, &pair_id, 2, 115, -200);

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T,
            ..Default::default()
        };

        let mut prev_index = pair_state.index_price;
        let mut prev_increment = UsdPrice::new_int(999);

        for i in 1..=5u128 {
            let t = T + Duration::from_seconds(i * 3);
            process_index_price_for_pair(
                &storage,
                t,
                &pair_id,
                &pair_param,
                &mut pair_state,
                other_price(),
            )
            .unwrap();

            let current = pair_state.index_price;
            let increment = current.checked_sub(prev_index).unwrap();

            // Monotonically increasing toward bid (110)
            assert!(
                current > prev_index,
                "tick {i}: expected monotonic increase"
            );

            // Each increment should be smaller than the previous (IPD shrinks)
            if i > 1 {
                assert!(
                    increment < prev_increment,
                    "tick {i}: expected decreasing increments, got {increment} >= {prev_increment}"
                );
            }

            prev_index = current;
            prev_increment = increment;
        }

        // After 5 ticks of 3s with α = raw(1665), bid = 110, ask = 115:
        //   tick 1: S = raw(100_016_650)  tick 2: raw(100_033_272)
        //   tick 3: raw(100_049_866)      tick 4: raw(100_066_432)
        //   tick 5: raw(100_082_971)
        assert_eq!(pair_state.index_price, UsdPrice::new_raw(100_082_971));
    }

    // ---- Drift clamp (discovery bound) ----

    /// A pair param whose `initial_margin_ratio` (= 1 / max_leverage) is 5%, so
    /// the closed-session drift bound is ±5% of the oracle anchor.
    fn clamped_pair_param() -> PairParam {
        PairParam {
            impact_size: UsdValue::new_int(10_000),
            initial_margin_ratio: Dimensionless::new_permille(50),
            ..Default::default()
        }
    }

    /// A far bid plus a long time delta would push the EWMA well above the
    /// oracle, but the drift is capped at oracle × (1 + imr) = 105.
    #[test]
    fn e1_drift_clamped_to_upper_bound() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        save_bid(&mut storage, &pair_id, 1, 200, 200);
        save_ask(&mut storage, &pair_id, 2, 205, -200);

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T - Duration::from_seconds(3600),
            ..Default::default()
        };

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &clamped_pair_param(),
            &mut pair_state,
            other_price(),
        )
        .unwrap();

        // Unclamped EWMA would be ~109.52; the +5% bound caps it at 105.
        assert_eq!(pair_state.index_price, UsdPrice::new_int(105));
    }

    /// A far ask plus a long time delta would push the EWMA well below the
    /// oracle, but the drift is capped at oracle × (1 - imr) = 95.
    #[test]
    fn e2_drift_clamped_to_lower_bound() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        // Ask only: bid side is empty, so IPD is purely the downward pull.
        save_ask(&mut storage, &pair_id, 2, 10, -2000);

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T - Duration::from_seconds(3600),
            ..Default::default()
        };

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &clamped_pair_param(),
            &mut pair_state,
            other_price(),
        )
        .unwrap();

        // Unclamped EWMA would be ~91.44; the -5% bound floors it at 95.
        assert_eq!(pair_state.index_price, UsdPrice::new_int(95));
    }

    /// A small drift that stays inside the ±5% corridor is not clamped — the
    /// EWMA value passes through unchanged.
    #[test]
    fn e3_drift_within_bound_not_clamped() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        save_bid(&mut storage, &pair_id, 1, 102, 200);
        save_ask(&mut storage, &pair_id, 2, 105, -200);

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T - Duration::from_seconds(3),
            ..Default::default()
        };

        process_index_price_for_pair(
            &storage,
            T,
            &pair_id,
            &clamped_pair_param(),
            &mut pair_state,
            other_price(),
        )
        .unwrap();

        // Same value as the unclamped EWMA (dt = 3s, IPD = 2): 100.003330.
        assert_eq!(pair_state.index_price, UsdPrice::new_raw(100_003_330));
    }

    /// The bound anchors to the last regular-session oracle price, not to the
    /// drifting index. No matter how many closed-session ticks an aggressive
    /// book pushes, the mark can never walk past +5% of the anchor.
    #[test]
    fn e4_bound_anchors_to_last_regular_price_not_drifting_index() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();
        let pair_param = clamped_pair_param();

        save_bid(&mut storage, &pair_id, 1, 200, 2000);
        save_ask(&mut storage, &pair_id, 2, 205, -2000);

        let mut pair_state = PairState {
            index_price: UsdPrice::new_int(100),
            oracle_price: UsdPrice::new_int(100),
            last_index_time: T,
            ..Default::default()
        };

        let mut t = T;
        for _ in 0..50 {
            t = t + Duration::from_seconds(180);
            let price = Ok(Price::new(UsdPrice::new_int(999), t, MarketSession::Other));
            process_index_price_for_pair(
                &storage,
                t,
                &pair_id,
                &pair_param,
                &mut pair_state,
                price,
            )
            .unwrap();

            assert!(
                pair_state.index_price <= UsdPrice::new_int(105),
                "closed-session mark must never exceed +5% of the anchor"
            );
            // The anchor itself never moves during the closure.
            assert_eq!(pair_state.oracle_price, UsdPrice::new_int(100));
        }

        // Pinned at the bound; the geometric walk is gone.
        assert_eq!(pair_state.index_price, UsdPrice::new_int(105));
    }
}
