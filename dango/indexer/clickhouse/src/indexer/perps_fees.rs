use {
    crate::{context::Context, entities::perps_fees::PerpsFees, error::Result, indexer::Indexer},
    chrono::{DateTime, Utc},
    dango_math::{IsZero, Number as _, NumberConst, Sign, Signed, Udec128_6},
    dango_order_book::{Quantity, UsdPrice, UsdValue},
    dango_primitives::{
        Addr, BlockAndBlockOutcomeWithHttpDetails, CommitmentStatus, EventName, EventStatus,
        EvtCron, FlatCommitmentStatus, FlatEvent, FlatEventInfo, FlatEventStatus, JsonDeExt,
        NaiveFlatten, SearchEvent,
    },
    dango_types::perps::{Deleveraged, FeeDistributed, OrderFilled},
};

impl Indexer {
    pub(crate) async fn store_perps_fees(
        perps_addr: &Addr,
        block_and_block_outcome: &BlockAndBlockOutcomeWithHttpDetails,
        context: &Context,
    ) -> Result<()> {
        let block_height = block_and_block_outcome.block.info.height;
        let created_at = DateTime::<Utc>::from_naive_utc_and_offset(
            block_and_block_outcome
                .block
                .info
                .timestamp
                .to_naive_date_time(),
            Utc,
        );

        let mut acc = FeesAccumulator::default();

        // User-submitted txs: `FeeDistributed` can surface here when an action
        // (e.g. closing a position directly) settles a fee-paying fill inline.
        for tx_outcome in block_and_block_outcome.block_outcome.tx_outcomes.iter() {
            if tx_outcome.result.is_err() {
                continue;
            }

            for event in tx_outcome.events.clone().flat() {
                if event.commitment_status != FlatCommitmentStatus::Committed {
                    continue;
                }

                let FlatEvent::ContractEvent(ref contract_event) = event.event else {
                    continue;
                };

                if contract_event.contract != *perps_addr {
                    continue;
                }

                if contract_event.ty == FeeDistributed::EVENT_NAME {
                    process_fee_distributed(contract_event, &mut acc, block_height);
                } else if contract_event.ty == OrderFilled::EVENT_NAME {
                    process_order_filled(contract_event, &mut acc, block_height);
                } else if contract_event.ty == Deleveraged::EVENT_NAME {
                    process_deleveraged(contract_event, &mut acc, block_height);
                }
            }
        }

        // End-of-block cron: the usual path where the perps settlement emits
        // `FeeDistributed` / `OrderFilled` / `Deleveraged` for matched,
        // liquidated, and ADL fills.
        for outcome in &block_and_block_outcome.block_outcome.cron_outcomes {
            let CommitmentStatus::Committed(EventStatus::Ok(EvtCron {
                guest_event: EventStatus::Ok(ref event),
                ..
            })) = outcome.cron_event
            else {
                continue;
            };

            if event.contract != perps_addr {
                continue;
            }

            for event in event
                .clone()
                .naive_flatten(FlatCommitmentStatus::Committed, FlatEventStatus::Ok)
            {
                let FlatEventInfo {
                    event: FlatEvent::ContractEvent(ref contract_event),
                    commitment_status: FlatCommitmentStatus::Committed,
                    event_status: FlatEventStatus::Ok,
                    ..
                } = event
                else {
                    continue;
                };

                if contract_event.ty == FeeDistributed::EVENT_NAME {
                    process_fee_distributed(contract_event, &mut acc, block_height);
                } else if contract_event.ty == OrderFilled::EVENT_NAME {
                    process_order_filled(contract_event, &mut acc, block_height);
                } else if contract_event.ty == Deleveraged::EVENT_NAME {
                    process_deleveraged(contract_event, &mut acc, block_height);
                }
            }
        }

        // Insert a row whenever the block had any fee activity OR any
        // perps trading volume. Vault-fill blocks emit `OrderFilled` with
        // `fee = 0` and no `FeeDistributed`, and ADL-only blocks emit
        // `Deleveraged` with no `FeeDistributed` either; both still carry
        // real volume and need to be captured.
        if acc.count == 0 && acc.volume_usd.is_zero() {
            return Ok(());
        }

        #[cfg(feature = "metrics")]
        {
            metrics::counter!("indexer.clickhouse.perps_fees.rows_inserted.total").increment(1);
            metrics::counter!("indexer.clickhouse.perps_fees.events_processed.total")
                .increment(acc.count as u64);
        }

        #[cfg(feature = "tracing")]
        tracing::debug!(
            block_height,
            fee_events = acc.count,
            "Inserting perps_fees row"
        );

        let row = PerpsFees {
            block_height,
            created_at,
            protocol_fee: acc.protocol_fee,
            vault_fee: acc.vault_fee,
            referee_rebate: acc.referee_rebate,
            referrer_payout: acc.referrer_payout,
            fee_events_count: acc.count,
            volume_usd: acc.volume_usd,
        };

        let clickhouse_client = context.clickhouse_client().clone();
        let mut inserter = clickhouse_client
            .inserter::<PerpsFees>("perps_fees")
            .with_max_rows(1);

        inserter.write(&row).await.inspect_err(|_err| {
            #[cfg(feature = "tracing")]
            tracing::error!("Failed to write perps_fees row: {row:#?}: {_err}");
        })?;

        inserter.commit().await.inspect_err(|_err| {
            #[cfg(feature = "tracing")]
            tracing::error!("Failed to commit inserter for perps_fees: {_err}");
        })?;

        inserter.end().await.inspect_err(|_err| {
            #[cfg(feature = "tracing")]
            tracing::error!("Failed to end inserter for perps_fees: {_err}");
        })?;

        Ok(())
    }
}

/// Running totals for the current block. All fields are `Udec128_6` (unsigned)
/// because the contract guarantees non-negativity by construction; the
/// per-event guard below rejects any violation before it reaches here.
#[derive(Default)]
struct FeesAccumulator {
    protocol_fee: Udec128_6,
    vault_fee: Udec128_6,
    referee_rebate: Udec128_6,
    referrer_payout: Udec128_6,
    count: u32,
    /// Sum of `|fill_size| × |fill_price|` across all `OrderFilled` (one
    /// side per match) and `Deleveraged` events in the block, across all
    /// pairs.
    volume_usd: Udec128_6,
}

fn process_fee_distributed(
    contract_event: &dango_primitives::CheckedContractEvent,
    acc: &mut FeesAccumulator,
    block_height: u64,
) {
    let fee: FeeDistributed = match contract_event.data.clone().deserialize_json() {
        Ok(e) => e,
        Err(_err) => {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                block_height,
                err = %_err,
                "Failed to deserialize FeeDistributed event; skipping"
            );
            return;
        },
    };

    let payer = fee.payer_addr;

    let protocol_fee = match to_non_negative(fee.protocol_fee, block_height, payer, "protocol_fee")
    {
        Some(v) => v,
        None => return,
    };
    let vault_fee = match to_non_negative(fee.vault_fee, block_height, payer, "vault_fee") {
        Some(v) => v,
        None => return,
    };

    // `commissions[0]` is the rebate to the payer; `commissions[1..]` go to
    // the referrer chain. The contract emits an empty vec when the payer has
    // no referrer or referrals are globally inactive — both treated as zeros.
    let referee_rebate = match fee.commissions.first().copied() {
        Some(v) => match to_non_negative(v, block_height, payer, "referee_rebate") {
            Some(v) => v,
            None => return,
        },
        None => Udec128_6::ZERO,
    };

    let mut referrer_payout = Udec128_6::ZERO;
    for c in fee.commissions.iter().copied().skip(1) {
        let Some(v) = to_non_negative(c, block_height, payer, "referrer_payout") else {
            return;
        };
        if referrer_payout.checked_add_assign(v).is_err() {
            #[cfg(feature = "tracing")]
            tracing::error!(
                block_height,
                %payer,
                "referrer_payout sum overflowed within a single FeeDistributed; skipping event"
            );
            #[cfg(feature = "metrics")]
            metrics::counter!("indexer.clickhouse.perps_fees.overflow.total").increment(1);
            return;
        }
    }

    // The contract emits `vault_fee` already net of referral commissions
    // (see `apply_fee_commissions` in `dango/exchange/perps`), so we store it as-is.
    // `referee_rebate` and `referrer_payout` are kept as informational
    // breakdowns of the commissions distributed alongside.
    if acc.protocol_fee.checked_add_assign(protocol_fee).is_err()
        || acc.vault_fee.checked_add_assign(vault_fee).is_err()
        || acc
            .referee_rebate
            .checked_add_assign(referee_rebate)
            .is_err()
        || acc
            .referrer_payout
            .checked_add_assign(referrer_payout)
            .is_err()
    {
        #[cfg(feature = "tracing")]
        tracing::error!(
            block_height,
            %payer,
            "perps_fees block accumulator overflowed; skipping event"
        );
        #[cfg(feature = "metrics")]
        metrics::counter!("indexer.clickhouse.perps_fees.overflow.total").increment(1);
        return;
    }

    acc.count = acc.count.saturating_add(1);
}

/// Add `|fill_size| × |fill_price|` from an `OrderFilled` event to the
/// block's `volume_usd` accumulator. Each order-book match emits two
/// events sharing one `fill_id` with opposite signs; we count only the
/// positive side to avoid double-counting. Same convention as
/// `perps_candles/mod.rs::process_order_filled`.
fn process_order_filled(
    contract_event: &dango_primitives::CheckedContractEvent,
    acc: &mut FeesAccumulator,
    #[cfg_attr(not(feature = "tracing"), allow(unused_variables))] block_height: u64,
) {
    let event: OrderFilled = match contract_event.data.clone().deserialize_json() {
        Ok(e) => e,
        Err(_err) => {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                block_height,
                err = %_err,
                "Failed to deserialize OrderFilled event; skipping"
            );
            return;
        },
    };

    if event.fill_size.is_negative() {
        return;
    }

    accumulate_volume_usd(
        event.fill_size,
        event.fill_price,
        acc,
        block_height,
        "order_filled",
    );
}

/// Add `|closing_size| × |fill_price|` from a `Deleveraged` event (one per
/// ADL counter-party hit, never duplicated). The signed magnitude is
/// taken absolute since `Deleveraged.closing_size` carries the
/// counter-party's reduction sign — equal in magnitude to the liquidated
/// user's slice but opposite in direction.
///
/// We deliberately do NOT also accumulate from `Liquidated.adl_size`:
/// `Σ Deleveraged.closing_size` for a liquidation already equals
/// `Liquidated.adl_size`, so summing both would double-count the ADL
/// contribution.
fn process_deleveraged(
    contract_event: &dango_primitives::CheckedContractEvent,
    acc: &mut FeesAccumulator,
    #[cfg_attr(not(feature = "tracing"), allow(unused_variables))] block_height: u64,
) {
    let event: Deleveraged = match contract_event.data.clone().deserialize_json() {
        Ok(e) => e,
        Err(_err) => {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                block_height,
                err = %_err,
                "Failed to deserialize Deleveraged event; skipping"
            );
            return;
        },
    };

    accumulate_volume_usd(
        event.closing_size,
        event.fill_price,
        acc,
        block_height,
        "deleveraged",
    );
}

/// Compute `|size| × |price|` and fold it into the block accumulator.
/// Any conversion or arithmetic failure is logged and the event is
/// dropped rather than corrupting the aggregate by wrapping.
#[cfg_attr(not(feature = "tracing"), allow(unused_variables))]
fn accumulate_volume_usd(
    size: Quantity,
    price: UsdPrice,
    acc: &mut FeesAccumulator,
    block_height: u64,
    event_name: &'static str,
) {
    let size_abs = match size
        .into_inner()
        .checked_abs()
        .and_then(|v| v.checked_into_unsigned())
    {
        Ok(v) => v,
        Err(_err) => {
            #[cfg(feature = "tracing")]
            tracing::error!(
                block_height,
                event = event_name,
                err = %_err,
                "Failed to take |size| for volume_usd; skipping event"
            );
            #[cfg(feature = "metrics")]
            metrics::counter!("indexer.clickhouse.perps_fees.overflow.total").increment(1);
            return;
        },
    };

    let price_abs = match price
        .into_inner()
        .checked_abs()
        .and_then(|v| v.checked_into_unsigned())
    {
        Ok(v) => v,
        Err(_err) => {
            #[cfg(feature = "tracing")]
            tracing::error!(
                block_height,
                event = event_name,
                err = %_err,
                "Failed to take |price| for volume_usd; skipping event"
            );
            #[cfg(feature = "metrics")]
            metrics::counter!("indexer.clickhouse.perps_fees.overflow.total").increment(1);
            return;
        },
    };

    let volume_usd = match size_abs.checked_mul(price_abs) {
        Ok(v) => v,
        Err(_err) => {
            #[cfg(feature = "tracing")]
            tracing::error!(
                block_height,
                event = event_name,
                err = %_err,
                "volume_usd overflow on |size| * |price|; skipping event"
            );
            #[cfg(feature = "metrics")]
            metrics::counter!("indexer.clickhouse.perps_fees.overflow.total").increment(1);
            return;
        },
    };

    if acc.volume_usd.checked_add_assign(volume_usd).is_err() {
        #[cfg(feature = "tracing")]
        tracing::error!(
            block_height,
            event = event_name,
            "perps_fees block volume_usd accumulator overflowed; skipping event"
        );
        #[cfg(feature = "metrics")]
        metrics::counter!("indexer.clickhouse.perps_fees.overflow.total").increment(1);
    }
}

/// Convert a signed `UsdValue` to `Udec128_6`, enforcing the contract's
/// non-negativity invariant. A negative value indicates a contract-side bug
/// (param validation allowed `taker_fee + maker_fee < 0` or a fee-rate outside
/// [0, 1]); we log loudly, bump a metric, and drop the event rather than
/// corrupt the aggregates by wrapping.
#[cfg_attr(not(feature = "tracing"), allow(unused_variables))]
fn to_non_negative(
    v: UsdValue,
    block_height: u64,
    payer: Addr,
    field: &'static str,
) -> Option<Udec128_6> {
    if v.is_negative() {
        #[cfg(feature = "tracing")]
        tracing::error!(
            block_height,
            %payer,
            field,
            value = %v,
            "FeeDistributed invariant violated: field is negative; skipping event"
        );
        #[cfg(feature = "metrics")]
        metrics::counter!(
            "indexer.clickhouse.perps_fees.invariant_violations.total",
            "field" => field,
        )
        .increment(1);
        return None;
    }

    v.into_inner()
        .checked_into_unsigned()
        .inspect_err(|_err| {
            #[cfg(feature = "tracing")]
            tracing::error!(
                block_height,
                %payer,
                field,
                err = %_err,
                "FeeDistributed value failed Dec128_6 -> Udec128_6 conversion; skipping event"
            );
        })
        .ok()
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_math::Uint64,
        dango_order_book::{PairId, Quantity, UsdPrice, UsdValue},
        dango_primitives::Denom,
        std::str::FromStr,
    };

    fn perp_pair() -> PairId {
        Denom::from_str("perp/btcusd").unwrap()
    }

    fn order_filled_event(fill_size_int: i128, fill_price_int: i128) -> OrderFilled {
        OrderFilled {
            order_id: Uint64::new(1),
            pair_id: perp_pair(),
            user: Addr::mock(1),
            fill_price: UsdPrice::new_int(fill_price_int),
            fill_size: Quantity::new_int(fill_size_int),
            closing_size: Quantity::new_int(0),
            opening_size: Quantity::new_int(fill_size_int),
            realized_pnl: UsdValue::new_int(0),
            realized_funding: Some(UsdValue::new_int(0)),
            fee: UsdValue::new_int(0),
            client_order_id: None,
            fill_id: Some(Uint64::new(42)),
            is_maker: Some(false),
            remaining_order_size: None,
            remaining_position_size: None,
        }
    }

    fn deleveraged_event(closing_size_int: i128, fill_price_int: i128) -> Deleveraged {
        Deleveraged {
            user: Addr::mock(2),
            pair_id: perp_pair(),
            closing_size: Quantity::new_int(closing_size_int),
            fill_price: UsdPrice::new_int(fill_price_int),
            realized_pnl: UsdValue::new_int(0),
            realized_funding: Some(UsdValue::new_int(0)),
            remaining_position_size: None,
        }
    }

    fn checked_event<T: serde::Serialize>(
        ty: &str,
        data: &T,
    ) -> dango_primitives::CheckedContractEvent {
        dango_primitives::CheckedContractEvent::new(Addr::mock(0), ty, data).unwrap()
    }

    fn expected_volume(size: i128, price: i128) -> Udec128_6 {
        // Both inputs are integer values stored at scale 10^6, so the
        // accumulator's Udec128_6 raw value (also at scale 10^6) is
        // size * price * 10^6.
        let raw = size
            .unsigned_abs()
            .checked_mul(price.unsigned_abs())
            .unwrap()
            .checked_mul(1_000_000)
            .unwrap();
        Udec128_6::raw(dango_math::Uint128::new(raw))
    }

    /// `OrderFilled` events come in pairs (maker/taker) sharing one
    /// `fill_id` but with opposite `fill_size` signs. We must count only
    /// the positive side to avoid double-counting volume.
    #[test]
    fn order_filled_skips_negative_side() {
        let mut acc = FeesAccumulator::default();

        process_order_filled(
            &checked_event("order_filled", &order_filled_event(10, 50_000)),
            &mut acc,
            1,
        );
        process_order_filled(
            &checked_event("order_filled", &order_filled_event(-10, 50_000)),
            &mut acc,
            1,
        );

        assert_eq!(acc.volume_usd, expected_volume(10, 50_000));
        assert_eq!(acc.count, 0); // OrderFilled does not bump fee_events_count
    }

    /// Multiple `OrderFilled` events on the positive side should sum.
    #[test]
    fn order_filled_accumulates_across_fills() {
        let mut acc = FeesAccumulator::default();

        process_order_filled(
            &checked_event("order_filled", &order_filled_event(10, 50_000)),
            &mut acc,
            1,
        );
        process_order_filled(
            &checked_event("order_filled", &order_filled_event(5, 60_000)),
            &mut acc,
            1,
        );

        let expected = expected_volume(10, 50_000)
            .checked_add(expected_volume(5, 60_000))
            .unwrap();
        assert_eq!(acc.volume_usd, expected);
    }

    /// `Deleveraged` is emitted once per ADL counter-party hit. There is
    /// no two-sided sibling event, so we count both positive and negative
    /// `closing_size` magnitudes.
    #[test]
    fn deleveraged_counts_both_signs() {
        let mut acc = FeesAccumulator::default();

        process_deleveraged(
            &checked_event("deleveraged", &deleveraged_event(7, 30_000)),
            &mut acc,
            1,
        );
        process_deleveraged(
            &checked_event("deleveraged", &deleveraged_event(-3, 30_000)),
            &mut acc,
            1,
        );

        let expected = expected_volume(7, 30_000)
            .checked_add(expected_volume(3, 30_000))
            .unwrap();
        assert_eq!(acc.volume_usd, expected);
    }

    /// Deserialization failures must not panic and must not mutate the
    /// accumulator — they only log/warn.
    #[test]
    fn malformed_event_payload_is_ignored() {
        let mut acc = FeesAccumulator::default();
        let bogus = dango_primitives::CheckedContractEvent::new(
            Addr::mock(0),
            "order_filled",
            serde_json::json!({"not": "an OrderFilled"}),
        )
        .unwrap();

        process_order_filled(&bogus, &mut acc, 1);

        assert_eq!(acc.volume_usd, Udec128_6::ZERO);
    }
}
