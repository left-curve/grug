use {
    crate::{
        core::{compute_funding_delta, compute_vault_premium},
        state::{PAIR_IDS, PAIR_PARAMS, PAIR_STATES, PARAM, STATE, USER_STATES},
    },
    dango_order_book::{Days, PairId, Quantity},
    dango_primitives::{Addr, Storage, Timestamp},
};

/// Compute and apply funding deltas for each trading pair using the vault's
/// inventory-skew-derived premium.
pub fn process_funding(
    storage: &mut dyn Storage,
    current_time: Timestamp,
    contract: Addr,
) -> anyhow::Result<()> {
    let param = PARAM.load(storage)?;
    let mut state = STATE.load(storage)?;

    // Only process funding if sufficient time has elapsed since the last funding time.
    let elapsed = current_time - state.last_funding_time;
    if elapsed < param.funding_period {
        return Ok(());
    }

    let interval = Days::from_duration(elapsed)?;
    let pair_ids = PAIR_IDS.load(storage)?;

    for pair_id in pair_ids {
        process_funding_for_pair(storage, contract, interval, pair_id)?;
    }

    state.last_funding_time = current_time;

    STATE.save(storage, &state)?;

    Ok(())
}

fn process_funding_for_pair(
    storage: &mut dyn Storage,
    contract: Addr,
    interval: Days,
    pair_id: PairId,
) -> anyhow::Result<()> {
    let pair_param = PAIR_PARAMS.load(storage, &pair_id)?;
    let mut pair_state = PAIR_STATES.load(storage, &pair_id)?;

    let oracle_price = pair_state.index_price;

    // Compute premium from the vault's inventory skew. When the vault is
    // the dominant maker, its skew-aware pricing directly determines the
    // mid-market price, yielding:
    //   premium = -halfSpread × skew × spreadSkewFactor
    let vault_position_size = USER_STATES
        .may_load(storage, contract)?
        .and_then(|vs| vs.positions.get(&pair_id).map(|p| p.size))
        .unwrap_or(Quantity::ZERO);

    let premium = compute_vault_premium(vault_position_size, &pair_param)?;

    // --- Impact-price model (replaced by vault premium above) -----------
    // let bid_iter = BIDS
    //     .prefix(pair_id.clone())
    //     .range(storage, None, None, IterationOrder::Ascending)
    //     .map(|res| {
    //         let ((stored_price, _), order) = res?;
    //         let real_price = may_invert_price(stored_price, true);
    //         Ok((real_price, order.size))
    //     });
    //
    // let ask_iter = ASKS
    //     .prefix(pair_id.clone())
    //     .range(storage, None, None, IterationOrder::Ascending)
    //     .map(|res| {
    //         let ((stored_price, _), order) = res?;
    //         Ok((stored_price, order.size.checked_abs()?))
    //     });
    //
    // let impact_bid = compute_impact_price(bid_iter, pair_param.impact_size)?;
    // let impact_ask = compute_impact_price(ask_iter, pair_param.impact_size)?;
    //
    // let premium = match (impact_bid, impact_ask) {
    //     (Some(impact_bid), Some(impact_ask)) => {
    //         compute_impact_premium(impact_bid, impact_ask, oracle_price)?
    //     },
    //     _ => Dimensionless::ZERO,
    // };
    // -------------------------------------------------------------------

    let (funding_delta, funding_rate) = compute_funding_delta(
        premium,
        oracle_price,
        pair_param.max_abs_funding_rate,
        interval,
    )?;

    pair_state.funding_rate = funding_rate;
    (pair_state.funding_per_unit).checked_add_assign(funding_delta)?;

    PAIR_STATES.save(storage, &pair_id, &pair_state)?;

    #[cfg(feature = "tracing")]
    {
        tracing::info!(
            %pair_id,
            %funding_delta,
            "Applied funding delta"
        );
    }

    #[cfg(feature = "metrics")]
    {
        let pair_label = pair_id.to_string();

        metrics::gauge!(
            crate::metrics::LABEL_FUNDING_RATE,
            "pair_id" => pair_label.clone()
        )
        .set(pair_state.funding_rate.to_f64());

        metrics::gauge!(
            crate::metrics::LABEL_FUNDING_PER_UNIT,
            "pair_id" => pair_label
        )
        .set(pair_state.funding_per_unit.to_f64());
    }

    Ok(())
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_order_book::{Dimensionless, FundingPerUnit, FundingRate, UsdPrice},
        dango_primitives::{Duration, MockStorage},
        dango_types::perps::{PairParam, PairState, Param, Position, State, UserState},
        std::collections::{BTreeMap, BTreeSet},
    };

    const CONTRACT: Addr = Addr::mock(100);

    fn btc_pair_id() -> PairId {
        "perp/btcusd".parse().unwrap()
    }

    fn eth_pair_id() -> PairId {
        "perp/ethusd".parse().unwrap()
    }

    /// Store the vault's user state with a position for the given pair.
    ///
    /// Mutates: writes to `USER_STATES` under `CONTRACT`.
    /// Returns: nothing.
    fn set_vault_position(storage: &mut dyn Storage, pair_id: &PairId, size: i128) {
        let mut positions = BTreeMap::new();
        positions.insert(
            pair_id.clone(),
            Position {
                size: Quantity::new_int(size),
                entry_price: UsdPrice::ZERO,
                entry_funding_per_unit: FundingPerUnit::ZERO,
                conditional_order_above: None,
                conditional_order_below: None,
            },
        );
        USER_STATES
            .save(
                storage,
                CONTRACT,
                &UserState {
                    positions,
                    ..Default::default()
                },
            )
            .unwrap();
    }

    /// Save the common funding-related storage items for a single pair.
    ///
    /// Mutates: writes `PARAM`, `STATE`, `PAIR_IDS`, `PAIR_PARAMS`, `PAIR_STATES`
    /// to `storage`.
    /// Returns: nothing.
    fn init_funding_storage(
        storage: &mut dyn Storage,
        pair_id: &PairId,
        pair_param: &PairParam,
        pair_state: &PairState,
        funding_period_secs: u128,
        last_funding_time_secs: u128,
    ) {
        PARAM
            .save(
                storage,
                &Param {
                    funding_period: Duration::from_seconds(funding_period_secs),
                    ..Default::default()
                },
            )
            .unwrap();
        STATE
            .save(
                storage,
                &State {
                    last_funding_time: Timestamp::from_seconds(last_funding_time_secs),
                    ..Default::default()
                },
            )
            .unwrap();
        PAIR_IDS
            .save(storage, &BTreeSet::from([pair_id.clone()]))
            .unwrap();
        PAIR_PARAMS.save(storage, pair_id, pair_param).unwrap();
        PAIR_STATES.save(storage, pair_id, pair_state).unwrap();
    }

    fn default_funding_pair_param() -> PairParam {
        PairParam {
            max_abs_funding_rate: FundingRate::new_raw(50_000), // 0.05/day
            vault_half_spread: Dimensionless::new_permille(10), // 1%
            vault_spread_skew_factor: Dimensionless::new_permille(300), // 0.3
            vault_max_skew_size: Quantity::new_int(100),
            funding_rate_multiplier: Dimensionless::ONE,
            ..Default::default()
        }
    }

    #[test]
    fn funding_skipped_when_period_not_elapsed() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        init_funding_storage(
            &mut storage,
            &pair_id,
            &default_funding_pair_param(),
            &PairState {
                index_price: UsdPrice::new_percent(5_000_000), // $50,000
                ..Default::default()
            },
            3600,
            0,
        );

        // Only 1800s elapsed, period is 3600s → funding skipped.
        process_funding(&mut storage, Timestamp::from_seconds(1800), CONTRACT).unwrap();

        let state = STATE.load(&storage).unwrap();
        assert_eq!(state.last_funding_time, Timestamp::from_seconds(0));

        let pair_state = PAIR_STATES.load(&storage, &pair_id).unwrap();
        assert_eq!(pair_state.funding_per_unit, FundingPerUnit::ZERO);
        assert_eq!(pair_state.funding_rate, FundingRate::ZERO);
    }

    #[test]
    fn funding_vault_long_produces_negative_rate() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        init_funding_storage(
            &mut storage,
            &pair_id,
            &default_funding_pair_param(),
            &PairState {
                index_price: UsdPrice::new_percent(5_000_000), // $50,000
                ..Default::default()
            },
            3600,
            0,
        );

        // Vault is long 50 → skew = 0.5 → premium = -(0.01 * 0.5 * 0.3) = -0.0015
        // → shorts pay longs.
        set_vault_position(&mut storage, &pair_id, 50);

        process_funding(&mut storage, Timestamp::from_seconds(3600), CONTRACT).unwrap();

        let state = STATE.load(&storage).unwrap();
        assert_eq!(state.last_funding_time, Timestamp::from_seconds(3600));

        let pair_state = PAIR_STATES.load(&storage, &pair_id).unwrap();
        assert!(pair_state.funding_per_unit < FundingPerUnit::ZERO);
        assert!(pair_state.funding_rate < FundingRate::ZERO);
    }

    #[test]
    fn funding_vault_short_produces_positive_rate() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        init_funding_storage(
            &mut storage,
            &pair_id,
            &default_funding_pair_param(),
            &PairState {
                index_price: UsdPrice::new_percent(5_000_000), // $50,000
                ..Default::default()
            },
            3600,
            0,
        );

        // Vault is short 50 → skew = -0.5 → premium = 0.0015
        // → longs pay shorts.
        set_vault_position(&mut storage, &pair_id, -50);

        process_funding(&mut storage, Timestamp::from_seconds(3600), CONTRACT).unwrap();

        let pair_state = PAIR_STATES.load(&storage, &pair_id).unwrap();
        assert!(pair_state.funding_per_unit > FundingPerUnit::ZERO);
        assert!(pair_state.funding_rate > FundingRate::ZERO);
    }

    #[test]
    fn funding_zero_vault_position_gives_zero_delta() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        init_funding_storage(
            &mut storage,
            &pair_id,
            &default_funding_pair_param(),
            &PairState {
                index_price: UsdPrice::new_percent(5_000_000), // $50,000
                ..Default::default()
            },
            3600,
            0,
        );

        // No vault state stored → position defaults to zero → premium = 0.

        process_funding(&mut storage, Timestamp::from_seconds(3600), CONTRACT).unwrap();

        let state = STATE.load(&storage).unwrap();
        assert_eq!(state.last_funding_time, Timestamp::from_seconds(3600));

        let pair_state = PAIR_STATES.load(&storage, &pair_id).unwrap();
        assert_eq!(pair_state.funding_per_unit, FundingPerUnit::ZERO);
        assert_eq!(pair_state.funding_rate, FundingRate::ZERO);
    }

    #[test]
    fn funding_zero_position_preserves_accumulator() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();

        let initial_funding_per_unit = FundingPerUnit::new_int(1_234);
        let initial_funding_rate = FundingRate::new_raw(42_000);

        init_funding_storage(
            &mut storage,
            &pair_id,
            &default_funding_pair_param(),
            &PairState {
                funding_per_unit: initial_funding_per_unit,
                funding_rate: initial_funding_rate,
                index_price: UsdPrice::new_percent(5_000_000),
                ..Default::default()
            },
            3600,
            0,
        );

        process_funding(&mut storage, Timestamp::from_seconds(3600), CONTRACT).unwrap();

        // Zero vault position → premium = 0 → rate overwritten to zero;
        // the accumulator receives a zero delta, so it is preserved.
        let pair_state = PAIR_STATES.load(&storage, &pair_id).unwrap();
        assert_eq!(pair_state.funding_rate, FundingRate::ZERO);
        assert_eq!(pair_state.funding_per_unit, initial_funding_per_unit);
    }

    #[test]
    fn funding_multiple_pairs() {
        let mut storage = MockStorage::new();
        let btc = btc_pair_id();
        let eth = eth_pair_id();
        let pair_param = default_funding_pair_param();

        PARAM
            .save(
                &mut storage,
                &Param {
                    funding_period: Duration::from_seconds(3600),
                    ..Default::default()
                },
            )
            .unwrap();
        STATE
            .save(
                &mut storage,
                &State {
                    last_funding_time: Timestamp::from_seconds(0),
                    ..Default::default()
                },
            )
            .unwrap();
        PAIR_IDS
            .save(&mut storage, &BTreeSet::from([btc.clone(), eth.clone()]))
            .unwrap();
        PAIR_PARAMS.save(&mut storage, &btc, &pair_param).unwrap();
        PAIR_PARAMS.save(&mut storage, &eth, &pair_param).unwrap();
        PAIR_STATES
            .save(
                &mut storage,
                &btc,
                &PairState {
                    index_price: UsdPrice::new_percent(5_000_000), // $50,000
                    ..Default::default()
                },
            )
            .unwrap();
        PAIR_STATES
            .save(
                &mut storage,
                &eth,
                &PairState {
                    index_price: UsdPrice::new_percent(300_000), // $3,000
                    ..Default::default()
                },
            )
            .unwrap();

        // Vault: long 50 BTC, short 50 ETH.
        let mut positions = BTreeMap::new();
        positions.insert(
            btc.clone(),
            Position {
                size: Quantity::new_int(50),
                entry_price: UsdPrice::ZERO,
                entry_funding_per_unit: FundingPerUnit::ZERO,
                conditional_order_above: None,
                conditional_order_below: None,
            },
        );
        positions.insert(
            eth.clone(),
            Position {
                size: Quantity::new_int(-50),
                entry_price: UsdPrice::ZERO,
                entry_funding_per_unit: FundingPerUnit::ZERO,
                conditional_order_above: None,
                conditional_order_below: None,
            },
        );
        USER_STATES
            .save(
                &mut storage,
                CONTRACT,
                &UserState {
                    positions,
                    ..Default::default()
                },
            )
            .unwrap();

        process_funding(&mut storage, Timestamp::from_seconds(3600), CONTRACT).unwrap();

        let state = STATE.load(&storage).unwrap();
        assert_eq!(state.last_funding_time, Timestamp::from_seconds(3600));

        // BTC: vault long → negative premium → shorts pay longs.
        let btc_state = PAIR_STATES.load(&storage, &btc).unwrap();
        assert!(btc_state.funding_per_unit < FundingPerUnit::ZERO);
        assert!(btc_state.funding_rate < FundingRate::ZERO);

        // ETH: vault short → positive premium → longs pay shorts.
        let eth_state = PAIR_STATES.load(&storage, &eth).unwrap();
        assert!(eth_state.funding_per_unit > FundingPerUnit::ZERO);
        assert!(eth_state.funding_rate > FundingRate::ZERO);
    }

    #[test]
    fn funding_updates_accumulator_not_replaces() {
        let mut storage = MockStorage::new();
        let pair_id = btc_pair_id();
        let initial_funding = FundingPerUnit::new_int(100);

        init_funding_storage(
            &mut storage,
            &pair_id,
            &default_funding_pair_param(),
            &PairState {
                funding_per_unit: initial_funding,
                index_price: UsdPrice::new_percent(5_000_000),
                ..Default::default()
            },
            3600,
            0,
        );

        // Vault short → positive premium → positive delta added to accumulator.
        set_vault_position(&mut storage, &pair_id, -50);

        process_funding(&mut storage, Timestamp::from_seconds(3600), CONTRACT).unwrap();

        let pair_state = PAIR_STATES.load(&storage, &pair_id).unwrap();
        // Accumulator = initial (100) + positive delta, so strictly greater.
        assert!(pair_state.funding_per_unit > initial_funding);
    }
}
