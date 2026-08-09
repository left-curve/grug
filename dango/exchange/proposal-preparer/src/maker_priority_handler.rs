use {
    dango_order_book::{OrderKind, TimeInForce},
    dango_primitives::{Addr, Inner, JsonDeExt, Message, QuerierExt, QuerierWrapper, StdError, Tx},
    dango_types::{
        config::AppConfig,
        perps::{self, SubmitOrCancelOrderRequest, TraderMsg},
    },
    prost::bytes::Bytes,
};

/// `ProposalPreparer` implementation that promotes "maker-priority"
/// transactions to the front of the block.
///
/// A transaction qualifies as priority if every one of its messages is a
/// perps trade message that is either a cancel or a post-only submit.
/// Within the priority group, transactions that contain at least one
/// post-only placement come before pure-cancellation transactions; other
/// transactions follow.
///
/// The placement-before-cancellation order ensures a user's
/// `[place X, cancel X]` broadcast pair is replayed in the right order
/// inside a block, regardless of CometBFT mempool gossip reordering
/// (CometBFT's per-peer FIFO is best-effort, not guaranteed across the
/// network).
///
/// Inspired by Hyperliquid's protection of market makers from toxic flow,
/// although Hyperliquid orders cancels first; see:
/// <https://hyperliquid.medium.com/latency-and-transaction-ordering-on-hyperliquid-cf28df3648eb>
#[derive(Default, Clone, Copy)]
pub struct MakerPriorityHandler;

impl dango_app::ProposalPreparer for MakerPriorityHandler {
    type Error = StdError;

    fn prepare_proposal(
        &self,
        querier: QuerierWrapper,
        txs: Vec<Bytes>,
        _max_tx_bytes: usize,
    ) -> Result<Vec<Bytes>, Self::Error> {
        let cfg: AppConfig = querier.query_app_config()?;
        Ok(promote_priority_txs(txs, &cfg.addresses.perps))
    }
}

/// Bucket assigned to a transaction during proposal preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityClass {
    /// Priority tx that contains at least one post-only placement.
    /// May also contain cancellations (in which case the placement and
    /// cancellation execute atomically inside the tx, in the order chosen
    /// by the user).
    Placement,

    /// Priority tx that contains only cancellations.
    Cancel,

    /// Anything else: malformed bytes, non-perps targets, non-trade
    /// execute messages, or any tx with at least one non-priority
    /// message (market, GTC, IOC, deposit, withdraw, conditional submit).
    Other,
}

/// Per-message classification used while scanning a tx.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MsgClass {
    /// The message contains at least one post-only placement. May also
    /// contain cancellations (e.g. inside a `BatchUpdateOrders`).
    HasPlacement,

    /// The message is purely a cancellation.
    CancelOnly,

    /// The message disqualifies the entire tx from priority status.
    NotPriority,
}

/// Reorder transactions: post-only placements first, then cancellations,
/// then everything else. Relative order within each bucket is preserved.
fn promote_priority_txs(txs: Vec<Bytes>, perps: &Addr) -> Vec<Bytes> {
    let mut placements = Vec::new();
    let mut cancels = Vec::new();
    let mut others = Vec::new();

    for raw in txs {
        match classify_tx(raw.as_ref(), perps) {
            PriorityClass::Placement => placements.push(raw),
            PriorityClass::Cancel => cancels.push(raw),
            PriorityClass::Other => others.push(raw),
        }
    }

    placements.extend(cancels);
    placements.extend(others);
    placements
}

/// Classify a raw tx into one of the three priority buckets.
///
/// `Other` covers malformed bytes, non-perps execute targets, non-trade
/// execute payloads, top-level non-execute messages, and any tx with at
/// least one non-priority message.
pub fn classify_tx(raw_tx: &[u8], perps: &Addr) -> PriorityClass {
    let Ok(tx) = raw_tx.deserialize_json::<Tx>() else {
        return PriorityClass::Other;
    };

    let mut has_placement = false;

    for msg in tx.msgs.into_inner() {
        let Message::Execute(exec) = msg else {
            return PriorityClass::Other;
        };

        if exec.contract != *perps {
            return PriorityClass::Other;
        }

        let Ok(execute_msg) = exec.msg.deserialize_json::<perps::ExecuteMsg>() else {
            return PriorityClass::Other;
        };

        let perps::ExecuteMsg::Trade(trader_msg) = execute_msg else {
            return PriorityClass::Other;
        };

        match classify_trader_msg(&trader_msg) {
            MsgClass::HasPlacement => has_placement = true,
            MsgClass::CancelOnly => {},
            MsgClass::NotPriority => return PriorityClass::Other,
        }
    }

    if has_placement {
        PriorityClass::Placement
    } else {
        PriorityClass::Cancel
    }
}

fn classify_trader_msg(msg: &perps::TraderMsg) -> MsgClass {
    match msg {
        TraderMsg::CancelOrder(_) | TraderMsg::CancelConditionalOrder(_) => MsgClass::CancelOnly,
        TraderMsg::SubmitOrder(req) => {
            if is_post_only(&req.kind) {
                MsgClass::HasPlacement
            } else {
                MsgClass::NotPriority
            }
        },
        TraderMsg::BatchUpdateOrders(batch) => {
            let mut has_placement = false;
            for req in batch.iter() {
                match classify_submit_or_cancel(req) {
                    MsgClass::HasPlacement => has_placement = true,
                    MsgClass::CancelOnly => {},
                    MsgClass::NotPriority => return MsgClass::NotPriority,
                }
            }
            if has_placement {
                MsgClass::HasPlacement
            } else {
                MsgClass::CancelOnly
            }
        },
        // Explicit non-priority arms — no `_` wildcard, so adding a new
        // `TraderMsg` variant forces an explicit decision at compile time.
        TraderMsg::Deposit { .. }
        | TraderMsg::Withdraw { .. }
        | TraderMsg::SubmitConditionalOrder { .. } => MsgClass::NotPriority,
    }
}

fn classify_submit_or_cancel(req: &SubmitOrCancelOrderRequest) -> MsgClass {
    match req {
        SubmitOrCancelOrderRequest::Cancel(_) => MsgClass::CancelOnly,
        SubmitOrCancelOrderRequest::Submit(req) => {
            if is_post_only(&req.kind) {
                MsgClass::HasPlacement
            } else {
                MsgClass::NotPriority
            }
        },
    }
}

fn is_post_only(kind: &OrderKind) -> bool {
    matches!(
        kind,
        OrderKind::Limit {
            time_in_force: TimeInForce::PostOnly,
            ..
        },
    )
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_math::Uint64,
        dango_order_book::{
            ChildOrder, Dimensionless, OrderKind, Quantity, TimeInForce, TriggerDirection,
            UsdPrice, UsdValue,
        },
        dango_primitives::{Coins, Json, JsonSerExt, MsgExecute, NonEmpty},
        dango_types::{
            constants::perp_btc,
            perps::{
                CancelConditionalOrderRequest, CancelOrderRequest, ExecuteMsg, MaintainerMsg,
                ReferralMsg, SubmitOrCancelOrderRequest, SubmitOrderRequest, TraderMsg,
            },
        },
        test_case::test_case,
    };

    fn perps() -> Addr {
        Addr::mock(1)
    }

    fn other_contract() -> Addr {
        Addr::mock(2)
    }

    // --- OrderKind constructors -------------------------------------------

    fn limit(tif: TimeInForce) -> OrderKind {
        OrderKind::Limit {
            limit_price: UsdPrice::new_int(100),
            time_in_force: tif,
            client_order_id: None,
        }
    }

    fn post_only_limit() -> OrderKind {
        limit(TimeInForce::PostOnly)
    }

    fn post_only_limit_with_client_id() -> OrderKind {
        OrderKind::Limit {
            limit_price: UsdPrice::new_int(100),
            time_in_force: TimeInForce::PostOnly,
            client_order_id: Some(Uint64::new(42)),
        }
    }

    fn gtc_limit() -> OrderKind {
        limit(TimeInForce::GoodTilCanceled)
    }

    fn ioc_limit() -> OrderKind {
        limit(TimeInForce::ImmediateOrCancel)
    }

    fn market() -> OrderKind {
        OrderKind::Market {
            max_slippage: Dimensionless::new_int(0),
        }
    }

    // --- SubmitOrderRequest ------------------------------------------------

    fn submit(kind: OrderKind) -> SubmitOrderRequest {
        SubmitOrderRequest {
            pair_id: perp_btc::DENOM.clone(),
            size: Quantity::new_int(1),
            kind,
            reduce_only: false,
            tp: None,
            sl: None,
        }
    }

    fn child_order() -> ChildOrder {
        ChildOrder {
            trigger_price: UsdPrice::new_int(120),
            max_slippage: Dimensionless::new_int(0),
            size: None,
        }
    }

    fn submit_with_tp_sl(kind: OrderKind) -> SubmitOrderRequest {
        SubmitOrderRequest {
            pair_id: perp_btc::DENOM.clone(),
            size: Quantity::new_int(1),
            kind,
            reduce_only: false,
            tp: Some(child_order()),
            sl: Some(child_order()),
        }
    }

    // --- TraderMsg constructors --------------------------------------------

    fn cancel_one() -> TraderMsg {
        TraderMsg::CancelOrder(CancelOrderRequest::One(Uint64::new(0)))
    }

    fn cancel_one_by_client_id() -> TraderMsg {
        TraderMsg::CancelOrder(CancelOrderRequest::OneByClientOrderId(Uint64::new(7)))
    }

    fn cancel_all() -> TraderMsg {
        TraderMsg::CancelOrder(CancelOrderRequest::All)
    }

    fn cancel_cond_one() -> TraderMsg {
        TraderMsg::CancelConditionalOrder(CancelConditionalOrderRequest::One {
            pair_id: perp_btc::DENOM.clone(),
            trigger_direction: TriggerDirection::Above,
        })
    }

    fn cancel_cond_all_for_pair() -> TraderMsg {
        TraderMsg::CancelConditionalOrder(CancelConditionalOrderRequest::AllForPair {
            pair_id: perp_btc::DENOM.clone(),
        })
    }

    fn cancel_cond_all() -> TraderMsg {
        TraderMsg::CancelConditionalOrder(CancelConditionalOrderRequest::All)
    }

    fn submit_post_only() -> TraderMsg {
        TraderMsg::SubmitOrder(submit(post_only_limit()))
    }

    fn submit_post_only_with_tp_sl() -> TraderMsg {
        TraderMsg::SubmitOrder(submit_with_tp_sl(post_only_limit()))
    }

    fn submit_post_only_with_client_id() -> TraderMsg {
        TraderMsg::SubmitOrder(submit(post_only_limit_with_client_id()))
    }

    fn submit_market() -> TraderMsg {
        TraderMsg::SubmitOrder(submit(market()))
    }

    fn submit_gtc() -> TraderMsg {
        TraderMsg::SubmitOrder(submit(gtc_limit()))
    }

    fn submit_ioc() -> TraderMsg {
        TraderMsg::SubmitOrder(submit(ioc_limit()))
    }

    fn batch_all_cancel() -> TraderMsg {
        TraderMsg::BatchUpdateOrders(NonEmpty::new_unchecked(vec![
            SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::All),
            SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::One(Uint64::new(1))),
        ]))
    }

    fn batch_all_post_only() -> TraderMsg {
        TraderMsg::BatchUpdateOrders(NonEmpty::new_unchecked(vec![
            SubmitOrCancelOrderRequest::Submit(submit(post_only_limit())),
            SubmitOrCancelOrderRequest::Submit(submit(post_only_limit())),
        ]))
    }

    fn batch_mixed_priority() -> TraderMsg {
        TraderMsg::BatchUpdateOrders(NonEmpty::new_unchecked(vec![
            SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::All),
            SubmitOrCancelOrderRequest::Submit(submit(post_only_limit())),
        ]))
    }

    fn batch_with_market() -> TraderMsg {
        TraderMsg::BatchUpdateOrders(NonEmpty::new_unchecked(vec![
            SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::All),
            SubmitOrCancelOrderRequest::Submit(submit(market())),
        ]))
    }

    fn batch_with_gtc() -> TraderMsg {
        TraderMsg::BatchUpdateOrders(NonEmpty::new_unchecked(vec![
            SubmitOrCancelOrderRequest::Submit(submit(post_only_limit())),
            SubmitOrCancelOrderRequest::Submit(submit(gtc_limit())),
        ]))
    }

    fn batch_with_ioc() -> TraderMsg {
        TraderMsg::BatchUpdateOrders(NonEmpty::new_unchecked(vec![
            SubmitOrCancelOrderRequest::Submit(submit(ioc_limit())),
        ]))
    }

    fn deposit() -> TraderMsg {
        TraderMsg::Deposit { to: None }
    }

    fn withdraw() -> TraderMsg {
        TraderMsg::Withdraw {
            amount: UsdValue::new_int(1),
        }
    }

    fn submit_conditional() -> TraderMsg {
        TraderMsg::SubmitConditionalOrder {
            pair_id: perp_btc::DENOM.clone(),
            size: None,
            trigger_price: UsdPrice::new_int(100),
            trigger_direction: TriggerDirection::Above,
            max_slippage: Dimensionless::new_int(0),
        }
    }

    // --- Tx builders -------------------------------------------------------

    /// Build a perps trade tx with the given trader messages.
    fn perps_tx(msgs: Vec<TraderMsg>) -> Bytes {
        tx_with_messages(
            msgs.into_iter()
                .map(|tm| Message::execute(perps(), &ExecuteMsg::Trade(tm), Coins::new()).unwrap())
                .collect(),
        )
    }

    fn tx_with_messages(messages: Vec<Message>) -> Bytes {
        let tx = Tx {
            sender: Addr::mock(99),
            gas_limit: 100_000,
            msgs: NonEmpty::new_unchecked(messages),
            data: Json::null(),
            credential: Json::null(),
        };
        tx.to_json_vec().unwrap().into()
    }

    // --- classify_trader_msg -----------------------------------------------

    #[test_case(cancel_one()                      => MsgClass::CancelOnly   ; "case_cancel_order_one")]
    #[test_case(cancel_one_by_client_id()         => MsgClass::CancelOnly   ; "case_cancel_order_by_client_id")]
    #[test_case(cancel_all()                      => MsgClass::CancelOnly   ; "case_cancel_order_all")]
    #[test_case(cancel_cond_one()                 => MsgClass::CancelOnly   ; "case_cancel_conditional_one")]
    #[test_case(cancel_cond_all_for_pair()        => MsgClass::CancelOnly   ; "case_cancel_conditional_all_for_pair")]
    #[test_case(cancel_cond_all()                 => MsgClass::CancelOnly   ; "case_cancel_conditional_all")]
    #[test_case(submit_post_only()                => MsgClass::HasPlacement ; "case_submit_post_only")]
    #[test_case(submit_post_only_with_tp_sl()     => MsgClass::HasPlacement ; "case_submit_post_only_with_tp_sl")]
    #[test_case(submit_post_only_with_client_id() => MsgClass::HasPlacement ; "case_submit_post_only_with_client_id")]
    #[test_case(batch_all_cancel()                => MsgClass::CancelOnly   ; "case_batch_all_cancel")]
    #[test_case(batch_all_post_only()             => MsgClass::HasPlacement ; "case_batch_all_post_only")]
    #[test_case(batch_mixed_priority()            => MsgClass::HasPlacement ; "case_batch_mixed")]
    #[test_case(submit_market()                   => MsgClass::NotPriority  ; "case_submit_market")]
    #[test_case(submit_gtc()                      => MsgClass::NotPriority  ; "case_submit_gtc")]
    #[test_case(submit_ioc()                      => MsgClass::NotPriority  ; "case_submit_ioc")]
    #[test_case(batch_with_market()               => MsgClass::NotPriority  ; "case_batch_with_market")]
    #[test_case(batch_with_gtc()                  => MsgClass::NotPriority  ; "case_batch_with_gtc")]
    #[test_case(batch_with_ioc()                  => MsgClass::NotPriority  ; "case_batch_with_ioc")]
    #[test_case(deposit()                         => MsgClass::NotPriority  ; "case_deposit")]
    #[test_case(withdraw()                        => MsgClass::NotPriority  ; "case_withdraw")]
    #[test_case(submit_conditional()              => MsgClass::NotPriority  ; "case_submit_conditional")]
    fn priority_trader_msg(msg: TraderMsg) -> MsgClass {
        classify_trader_msg(&msg)
    }

    // --- classify_tx -------------------------------------------------------

    #[test_case(perps_tx(vec![cancel_all()])                                  => PriorityClass::Cancel    ; "single_cancel")]
    #[test_case(perps_tx(vec![submit_post_only()])                            => PriorityClass::Placement ; "single_post_only")]
    #[test_case(perps_tx(vec![batch_all_cancel()])                            => PriorityClass::Cancel    ; "single_batch_all_cancel")]
    #[test_case(perps_tx(vec![batch_all_post_only()])                         => PriorityClass::Placement ; "single_batch_all_post_only")]
    #[test_case(perps_tx(vec![batch_mixed_priority()])                        => PriorityClass::Placement ; "single_batch_mixed")]
    #[test_case(perps_tx(vec![cancel_all(), submit_post_only()])              => PriorityClass::Placement ; "multi_msg_all_priority_with_placement")]
    #[test_case(perps_tx(vec![cancel_all(), cancel_one(), cancel_cond_all()]) => PriorityClass::Cancel    ; "multi_cancel_only")]
    #[test_case(perps_tx(vec![submit_post_only(), submit_post_only()])        => PriorityClass::Placement ; "multi_placement_only")]
    #[test_case(perps_tx(vec![submit_market()])                               => PriorityClass::Other     ; "single_market")]
    #[test_case(perps_tx(vec![cancel_all(), submit_market()])                 => PriorityClass::Other     ; "multi_msg_one_disqualifies")]
    #[test_case(perps_tx(vec![submit_post_only(), submit_market()])           => PriorityClass::Other     ; "multi_msg_placement_with_non_priority")]
    #[test_case(perps_tx(vec![cancel_all(), deposit()])                       => PriorityClass::Other     ; "multi_msg_deposit_disqualifies")]
    #[test_case(perps_tx(vec![submit_conditional()])                          => PriorityClass::Other     ; "single_conditional_submit")]
    fn priority_tx_perps(tx: Bytes) -> PriorityClass {
        classify_tx(tx.as_ref(), &perps())
    }

    #[test]
    fn priority_tx_wrong_contract() {
        // A cancel-only tx targeting a different contract is not priority.
        let tx = tx_with_messages(vec![
            Message::execute(
                other_contract(),
                &ExecuteMsg::Trade(cancel_all()),
                Coins::new(),
            )
            .unwrap(),
        ]);
        assert_eq!(classify_tx(tx.as_ref(), &perps()), PriorityClass::Other);
    }

    // Top-level `Message` variants other than `Execute` are never priority.
    #[test_case(Message::transfer(perps(), Coins::new()).unwrap() => PriorityClass::Other ; "case_non_execute_transfer")]
    #[test_case(Message::upload(vec![0u8; 8])                     => PriorityClass::Other ; "case_non_execute_upload")]
    fn priority_tx_non_execute_message(message: Message) -> PriorityClass {
        let tx = tx_with_messages(vec![message]);
        classify_tx(tx.as_ref(), &perps())
    }

    // A perps Execute message whose payload is not `Trade(...)` is never
    // priority — covers all sibling variants of `ExecuteMsg`.
    #[test_case(ExecuteMsg::Maintain(MaintainerMsg::Donate {})                             => PriorityClass::Other ; "case_non_trade_maintain")]
    #[test_case(ExecuteMsg::Maintain(MaintainerMsg::RefreshIndexPrices {})                 => PriorityClass::Other ; "case_non_trade_refresh_index")]
    #[test_case(ExecuteMsg::Maintain(MaintainerMsg::RefreshVaultOrders {})                 => PriorityClass::Other ; "case_non_trade_refresh_vault")]
    #[test_case(ExecuteMsg::Referral(ReferralMsg::SetReferral { referrer: 1, referee: 2 }) => PriorityClass::Other ; "case_non_trade_referral")]
    fn priority_tx_non_trade_execute_msg(execute_msg: ExecuteMsg) -> PriorityClass {
        let tx = tx_with_messages(vec![
            Message::execute(perps(), &execute_msg, Coins::new()).unwrap(),
        ]);
        classify_tx(tx.as_ref(), &perps())
    }

    #[test]
    fn priority_tx_malformed_bytes() {
        let raw: &[u8] = b"not a tx";
        assert_eq!(classify_tx(raw, &perps()), PriorityClass::Other);
    }

    #[test]
    fn priority_tx_valid_tx_bad_inner_msg() {
        // Valid `Tx`, but `MsgExecute.msg` payload (here, JSON null) isn't a
        // valid perps `ExecuteMsg`.
        let tx = Tx {
            sender: Addr::mock(99),
            gas_limit: 100_000,
            msgs: NonEmpty::new_unchecked(vec![Message::Execute(MsgExecute {
                contract: perps(),
                msg: Json::null(),
                funds: Coins::new(),
            })]),
            data: Json::null(),
            credential: Json::null(),
        };
        let raw: Bytes = tx.to_json_vec().unwrap().into();
        assert_eq!(classify_tx(raw.as_ref(), &perps()), PriorityClass::Other);
    }

    // --- promote_priority_txs ----------------------------------------------

    #[test]
    fn promote_empty_input() {
        let txs = promote_priority_txs(vec![], &perps());
        assert!(txs.is_empty());
    }

    #[test]
    fn promote_all_placements_unchanged() {
        let a = perps_tx(vec![submit_post_only()]);
        let b = perps_tx(vec![submit_post_only()]);
        let c = perps_tx(vec![batch_all_post_only()]);
        let txs = promote_priority_txs(vec![a.clone(), b.clone(), c.clone()], &perps());
        assert_eq!(txs, vec![a, b, c]);
    }

    #[test]
    fn promote_all_cancels_unchanged() {
        let a = perps_tx(vec![cancel_all()]);
        let b = perps_tx(vec![cancel_one()]);
        let c = perps_tx(vec![batch_all_cancel()]);
        let txs = promote_priority_txs(vec![a.clone(), b.clone(), c.clone()], &perps());
        assert_eq!(txs, vec![a, b, c]);
    }

    #[test]
    fn promote_all_non_priority_unchanged() {
        let a = perps_tx(vec![submit_market()]);
        let b = perps_tx(vec![submit_gtc()]);
        let c = perps_tx(vec![deposit()]);
        let txs = promote_priority_txs(vec![a.clone(), b.clone(), c.clone()], &perps());
        assert_eq!(txs, vec![a, b, c]);
    }

    /// Mainnet incident: user broadcasts a placement then a cancel-by-
    /// client-id, but the proposer's mempool sees them swapped. After
    /// reordering, the placement must come first so the cancel can find
    /// the order.
    #[test]
    fn promote_mainnet_incident() {
        let cancel = perps_tx(vec![cancel_one_by_client_id()]);
        let placement = perps_tx(vec![submit_post_only_with_client_id()]);
        let txs = promote_priority_txs(vec![cancel.clone(), placement.clone()], &perps());
        assert_eq!(txs, vec![placement, cancel]);
    }

    #[test]
    fn promote_three_way_preserves_relative_order() {
        // Input:  C1, P1, O1, C2, P2, O2
        // Expect: P1, P2, C1, C2, O1, O2
        let p1 = perps_tx(vec![submit_post_only()]);
        let p2 = perps_tx(vec![batch_all_post_only()]);
        let c1 = perps_tx(vec![cancel_all()]);
        let c2 = perps_tx(vec![cancel_one()]);
        let o1 = perps_tx(vec![submit_market()]);
        let o2 = perps_tx(vec![deposit()]);
        let txs = promote_priority_txs(
            vec![
                c1.clone(),
                p1.clone(),
                o1.clone(),
                c2.clone(),
                p2.clone(),
                o2.clone(),
            ],
            &perps(),
        );
        assert_eq!(txs, vec![p1, p2, c1, c2, o1, o2]);
    }

    /// A mixed-priority tx (one whose messages include both post-only
    /// placements and cancellations) clusters with the placement bucket.
    #[test]
    fn promote_mixed_clusters_with_placements() {
        let mixed = perps_tx(vec![batch_mixed_priority()]);
        let pure_cancel = perps_tx(vec![cancel_all()]);
        let pure_placement = perps_tx(vec![submit_post_only()]);
        let txs = promote_priority_txs(
            vec![mixed.clone(), pure_cancel.clone(), pure_placement.clone()],
            &perps(),
        );
        assert_eq!(txs, vec![mixed, pure_placement, pure_cancel]);
    }

    #[test]
    fn promote_single_priority_among_non_priority() {
        let n1 = perps_tx(vec![submit_market()]);
        let p = perps_tx(vec![submit_post_only()]);
        let n2 = perps_tx(vec![withdraw()]);
        let txs = promote_priority_txs(vec![n1.clone(), p.clone(), n2.clone()], &perps());
        assert_eq!(txs, vec![p, n1, n2]);
    }
}
