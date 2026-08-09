//! Perps-exchange event types, extraction from a `BlockOutcome`, and the
//! client-supplied filter, for the `perps_events` subscription.
//!
//! Every event emitted by the perps contract is captured (filtered only by
//! emitting contract address) — there is no event-name whitelist, so new perps
//! event types appear in the feed automatically. Clients narrow the feed with
//! the `event_types` / `pair_ids` / `users` / `order_ids` / `client_order_ids`
//! filter.

use {
    crate::recent_stream::HasHeight,
    dango_order_book::{ClientOrderId, OrderId},
    dango_primitives::{
        Addr, BlockOutcome, CheckedContractEvent, Denom, FlatCommitmentStatus, FlatEvent, Inner,
        JsonDeExt, SearchEvent,
    },
    std::collections::HashSet,
};

/// A single perps-contract event, as streamed to clients over the WebSocket
/// feed. `serde(rename_all = "camelCase")` produces the camelCase wire field
/// names (`eventType`, `pairId`, ...).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerpsEvent {
    /// Per-block ordinal across all perps-contract events in the block.
    pub idx: u32,

    /// Raw on-chain event type string, e.g. `"order_filled"`, `"liquidated"`.
    pub event_type: String,

    /// The event's subject address (its `user` field), if present. Each perps
    /// event names a single participant — a fill's two sides are separate
    /// events — so this is what the `user` filter matches on. `None` for events
    /// without a `user` field (e.g. fee distribution).
    pub user: Option<String>,

    /// The market the event pertains to (its `pair_id` field), if present.
    pub pair_id: Option<String>,

    /// The order this event pertains to (its `order_id` field), if present.
    /// Carried by the order-lifecycle events (`order_filled`, `order_persisted`,
    /// `order_resized`, `order_removed`); `None` otherwise.
    pub order_id: Option<String>,

    /// The caller-assigned client order id (its `client_order_id` field), if
    /// present. Optional even on order-lifecycle events — an order submitted
    /// without one yields `None` here. Unique only per sender, so combine with
    /// the `users` filter to single out one trader's order.
    pub client_order_id: Option<String>,

    /// The raw event payload.
    pub data: serde_json::Value,
}

/// All perps-contract events emitted in one block. Doubles as the ring item
/// (holding every event) and the streamed output (holding the filtered subset).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerpsEventBlock {
    pub block_height: u64,

    /// Block timestamp, RFC 3339.
    pub created_at: String,

    pub events: Vec<PerpsEvent>,
}

impl HasHeight for PerpsEventBlock {
    fn height(&self) -> u64 {
        self.block_height
    }
}

/// Extract every committed perps-contract event from a block's outcome.
///
/// `BlockOutcome::flat()` folds both tx and cron outcomes into one ordered
/// list, so liquidations/ADL emitted in cron context are captured alongside
/// user-submitted order events.
pub fn extract_perps_event_block(
    block_height: u64,
    created_at: String,
    outcome: BlockOutcome,
    perps_addr: Addr,
) -> PerpsEventBlock {
    let mut events = Vec::new();
    let mut idx = 0u32;

    for flat in outcome.flat() {
        if flat.commitment_status != FlatCommitmentStatus::Committed {
            continue;
        }

        let FlatEvent::ContractEvent(ref contract_event) = flat.event else {
            continue;
        };

        if contract_event.contract != perps_addr {
            continue;
        }

        let (user, pair_id, order_id, client_order_id) = extract_filter_fields(contract_event);

        events.push(PerpsEvent {
            idx,
            event_type: contract_event.ty.clone(),
            user,
            pair_id,
            order_id,
            client_order_id,
            data: contract_event.data.clone().into_inner(),
        });

        idx += 1;
    }

    PerpsEventBlock {
        block_height,
        created_at,
        events,
    }
}

/// Best-effort extraction of the filterable fields shared by most perps events,
/// returned as `(user, pair_id, order_id, client_order_id)`. A field absent from
/// the payload simply yields `None` there (the event can still match on the
/// fields it does carry, or on its `event_type`).
///
/// `order_id` and `client_order_id` are grug `Uint64`s, which serialize as
/// decimal strings in the payload; we keep that canonical string form so it
/// matches verbatim against the values clients pass to the `order_ids` /
/// `client_order_ids` filters (mirroring how `user` and `pair_id` are handled).
fn extract_filter_fields(
    event: &CheckedContractEvent,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    #[derive(Default, serde::Deserialize)]
    struct Raw {
        #[serde(default)]
        user: Option<Addr>,

        #[serde(default)]
        pair_id: Option<Denom>,

        #[serde(default)]
        order_id: Option<OrderId>,

        #[serde(default)]
        client_order_id: Option<ClientOrderId>,
    }

    let raw: Raw = event.data.clone().deserialize_json().unwrap_or_default();

    (
        raw.user.map(|a| a.to_string()),
        raw.pair_id.map(|d| d.to_string()),
        raw.order_id.map(|o| o.to_string()),
        raw.client_order_id.map(|c| c.to_string()),
    )
}

/// Build the per-block projection closure for a `perps_events` subscription.
///
/// The five predicates AND together. For each, `None` means "do not filter on
/// this field" (matches everything), while `Some(set)` keeps only events whose
/// field is in the set — so an empty set matches nothing. A block with no
/// matching events projects to `None` (it is suppressed, never emitted as an
/// empty batch).
pub fn make_perps_filter(
    event_types: Option<HashSet<String>>,
    pair_ids: Option<HashSet<String>>,
    users: Option<HashSet<String>>,
    order_ids: Option<HashSet<String>>,
    client_order_ids: Option<HashSet<String>>,
) -> impl Fn(&PerpsEventBlock) -> Option<PerpsEventBlock> + Send + 'static {
    move |block: &PerpsEventBlock| {
        let matched = block
            .events
            .iter()
            .filter(|event| {
                if let Some(event_types) = &event_types
                    && !event_types.contains(&event.event_type)
                {
                    return false;
                }

                // An event without a `pair_id` cannot match a pair filter.
                if let Some(pair_ids) = &pair_ids {
                    let Some(pair_id) = &event.pair_id else {
                        return false;
                    };

                    if !pair_ids.contains(pair_id) {
                        return false;
                    }
                }

                // An event without a `user` cannot match a user filter.
                if let Some(users) = &users {
                    let Some(user) = &event.user else {
                        return false;
                    };

                    if !users.contains(user) {
                        return false;
                    }
                }

                // An event without an `order_id` cannot match an order filter.
                if let Some(order_ids) = &order_ids {
                    let Some(order_id) = &event.order_id else {
                        return false;
                    };

                    if !order_ids.contains(order_id) {
                        return false;
                    }
                }

                // An event without a `client_order_id` cannot match a client
                // order filter.
                if let Some(client_order_ids) = &client_order_ids {
                    let Some(client_order_id) = &event.client_order_id else {
                        return false;
                    };

                    if !client_order_ids.contains(client_order_id) {
                        return false;
                    }
                }

                true
            })
            .cloned()
            .collect::<Vec<_>>();

        if matched.is_empty() {
            None
        } else {
            Some(PerpsEventBlock {
                block_height: block.block_height,
                created_at: block.created_at.clone(),
                events: matched,
            })
        }
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        idx: u32,
        ty: &str,
        pair: Option<&str>,
        user: Option<u8>,
        order_id: Option<u64>,
        client_order_id: Option<u64>,
    ) -> PerpsEvent {
        PerpsEvent {
            idx,
            event_type: ty.to_string(),
            user: user.map(|i| Addr::mock(i).to_string()),
            pair_id: pair.map(ToString::to_string),
            // Mirror production: ids are grug `Uint64`s rendered as decimal
            // strings.
            order_id: order_id.map(|i| i.to_string()),
            client_order_id: client_order_id.map(|i| i.to_string()),
            data: serde_json::json!({}),
        }
    }

    fn block() -> PerpsEventBlock {
        PerpsEventBlock {
            block_height: 7,
            created_at: "2026-06-18T00:00:00Z".to_string(),
            events: vec![
                // A fill carrying both an order id and a client order id.
                event(
                    0,
                    "order_filled",
                    Some("perp/btcusd"),
                    Some(1),
                    Some(100),
                    Some(7),
                ),
                // A liquidation summary: no order id, no client order id.
                event(1, "liquidated", Some("perp/ethusd"), Some(3), None, None),
                // A resting order placed without a client order id.
                event(
                    2,
                    "order_persisted",
                    Some("perp/btcusd"),
                    Some(1),
                    Some(101),
                    None,
                ),
            ],
        }
    }

    fn types(names: &[&str]) -> Option<HashSet<String>> {
        Some(names.iter().map(ToString::to_string).collect())
    }

    fn pairs(names: &[&str]) -> Option<HashSet<String>> {
        Some(names.iter().map(ToString::to_string).collect())
    }

    fn users(idxs: &[u8]) -> Option<HashSet<String>> {
        Some(idxs.iter().map(|i| Addr::mock(*i).to_string()).collect())
    }

    fn order_ids(ids: &[u64]) -> Option<HashSet<String>> {
        Some(ids.iter().map(|i| i.to_string()).collect())
    }

    fn client_order_ids(ids: &[u64]) -> Option<HashSet<String>> {
        Some(ids.iter().map(|i| i.to_string()).collect())
    }

    #[test]
    fn no_filter_matches_whole_block() {
        let f = make_perps_filter(None, None, None, None, None);
        let out = f(&block()).unwrap();
        assert_eq!(out.events.len(), 3);
        assert_eq!(out.block_height, 7);
    }

    #[test]
    fn event_type_filter_is_a_set() {
        let f = make_perps_filter(
            types(&["order_filled", "liquidated"]),
            None,
            None,
            None,
            None,
        );
        let out = f(&block()).unwrap();
        assert_eq!(
            out.events
                .iter()
                .map(|e| e.event_type.as_str())
                .collect::<Vec<_>>(),
            vec!["order_filled", "liquidated"]
        );
    }

    #[test]
    fn pair_filter() {
        let f = make_perps_filter(None, pairs(&["perp/btcusd"]), None, None, None);
        let out = f(&block()).unwrap();
        assert_eq!(
            out.events.iter().map(|e| e.idx).collect::<Vec<_>>(),
            vec![0, 2]
        );
    }

    #[test]
    fn user_filter_matches_user_field() {
        // mock(1) is the `user` of both the order_filled and order_persisted.
        let f = make_perps_filter(None, None, users(&[1]), None, None);
        let out = f(&block()).unwrap();
        assert_eq!(
            out.events.iter().map(|e| e.idx).collect::<Vec<_>>(),
            vec![0, 2]
        );

        // mock(2) is not the `user` of any event here.
        let f = make_perps_filter(None, None, users(&[2]), None, None);
        assert!(f(&block()).is_none());
    }

    #[test]
    fn combined_filters_and_together() {
        let f = make_perps_filter(
            types(&["order_persisted"]),
            pairs(&["perp/btcusd"]),
            users(&[1]),
            order_ids(&[101]),
            None,
        );
        let out = f(&block()).unwrap();
        assert_eq!(
            out.events.iter().map(|e| e.idx).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn order_id_filter() {
        // order_id 100 belongs only to the order_filled (idx 0).
        let f = make_perps_filter(None, None, None, order_ids(&[100]), None);
        let out = f(&block()).unwrap();
        assert_eq!(
            out.events.iter().map(|e| e.idx).collect::<Vec<_>>(),
            vec![0]
        );

        // A set spanning both resting orders keeps both, in block order.
        let f = make_perps_filter(None, None, None, order_ids(&[100, 101]), None);
        let out = f(&block()).unwrap();
        assert_eq!(
            out.events.iter().map(|e| e.idx).collect::<Vec<_>>(),
            vec![0, 2]
        );

        // The liquidated event (idx 1) carries no order_id, and no event has id
        // 999, so an order filter excludes everything.
        let f = make_perps_filter(None, None, None, order_ids(&[999]), None);
        assert!(f(&block()).is_none());
    }

    #[test]
    fn client_order_id_filter() {
        // Only the order_filled (idx 0) carries client_order_id 7.
        let f = make_perps_filter(None, None, None, None, client_order_ids(&[7]));
        let out = f(&block()).unwrap();
        assert_eq!(
            out.events.iter().map(|e| e.idx).collect::<Vec<_>>(),
            vec![0]
        );

        // The liquidation and the cid-less resting order (idx 1, 2) cannot match
        // any client order filter.
        let f = make_perps_filter(None, None, None, None, client_order_ids(&[999]));
        assert!(f(&block()).is_none());
    }

    #[test]
    fn no_match_suppresses_block() {
        let f = make_perps_filter(types(&["deleveraged"]), None, None, None, None);
        assert!(f(&block()).is_none());
    }

    #[test]
    fn empty_set_matches_nothing() {
        // `Some(empty)` means "filter on this field, but no value qualifies" —
        // distinct from `None` (do not filter). It must suppress the block.
        let f = make_perps_filter(Some(HashSet::new()), None, None, None, None);
        assert!(f(&block()).is_none());

        let f = make_perps_filter(None, None, Some(HashSet::new()), None, None);
        assert!(f(&block()).is_none());

        let f = make_perps_filter(None, None, None, Some(HashSet::new()), None);
        assert!(f(&block()).is_none());

        let f = make_perps_filter(None, None, None, None, Some(HashSet::new()));
        assert!(f(&block()).is_none());
    }
}
