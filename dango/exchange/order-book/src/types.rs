use {
    crate::{Dimensionless, Quantity, UsdPrice, UsdValue},
    dango_math::Uint64,
    dango_primitives::{Addr, Denom, Timestamp},
    std::collections::BTreeMap,
};

/// Identifier of a trading pair. It should be a string that looks like e.g. "perp/btcusd".
pub type PairId = Denom;

/// Identifier for a resting limit order.
///
/// Order Id has two purposes:
///
/// 1. For uniquely identifying an order.
/// 2. For determining an order's seniority. Orders matching follows **price-time
///    priority**: orders with the better prices are executed first; for orders
///    with the same price, those submitted earlier are executed first. Order IDs
///    are allocated in incremental order, so orders with smaller IDs are more senior.
///    It's also for this reason, that the order ID is included as a sub-key in
///    the `BIDS` and `ASKS` maps, as well as in the index key of `UserStateIndex::conditional_orders`
///    (see `dango/exchange/perps/src/state.rs`).
///    Timestamp doesn't work for this case, because two orders submitted in the
///    same block have the same timestamp.
pub type OrderId = Uint64;

/// Shares the same ID space as `OrderId` (same `NEXT_ORDER_ID` counter).
pub type ConditionalOrderId = OrderId;

/// Identifier for an order-book match. Both `OrderFilled` events emitted
/// for a single match (taker side + maker side) carry the same `FillId`,
/// so consumers can group the two sides by this field. Strictly
/// increasing across matches.
///
/// Not assigned to ADL fills — those are emitted via the `Deleveraged`
/// and `Liquidated` events, which have no `fill_id` field.
pub type FillId = Uint64;

/// Client-assigned order id. Lets a trader cancel an order in the same block
/// it was submitted, without round-tripping through the server response to
/// learn the system-assigned `OrderId`.
///
/// Scope of uniqueness: per-sender, across the sender's *active* (resting)
/// limit orders only. The contract does not remember client order ids of
/// orders that have been canceled or filled, so they can be reused freely.
pub type ClientOrderId = Uint64;

#[dango_primitives::derive(Serde)]
#[derive(Copy, Default)]
pub enum TimeInForce {
    /// Persist the unfilled portion in the order book.
    #[default]
    #[serde(rename = "GTC")]
    GoodTilCanceled,

    /// Cancel the unfilled portion immediately.
    #[serde(rename = "IOC")]
    ImmediateOrCancel,

    /// Insert into the book without matching. The limit price must not cross
    /// the best offer on the other side; reject if violated.
    #[serde(rename = "POST")]
    PostOnly,
}

#[dango_primitives::derive(Serde)]
#[derive(Copy)]
pub enum OrderKind {
    /// Trade at the best available prices in the order book, optionally
    /// with a slippage tolerance relative to the oracle price.
    ///
    /// If the order cannot be fully filled, the unfilled portion is
    /// canceled (immediate-or-cancel behavior).
    Market { max_slippage: Dimensionless },

    /// Trade at the specified limit price.
    Limit {
        limit_price: UsdPrice,

        /// Controls what happens to the unfilled portion:
        ///
        /// - GTC: persist in the order book;
        /// - IOC: cancel;
        /// - PostOnly: skip matching, rest entire order on book (reject if
        ///   limit price crosses best offer).
        #[serde(default)]
        time_in_force: TimeInForce,

        /// Caller-assigned id used to cancel this order via
        /// `CancelOrderRequest::OneByClientOrderId` before the system-assigned
        /// `OrderId` is known. Must be unique across the sender's *active*
        /// orders. Not allowed with `TimeInForce::ImmediateOrCancel`, which
        /// never enters the book.
        #[serde(default)]
        client_order_id: Option<ClientOrderId>,
    },
}

/// For a conditional (TP/SL) order, direction the oracle price must cross to
/// trigger it.
#[dango_primitives::derive(Serde, Borsh)]
#[derive(Copy, dango_storage::PrimaryKey)]
pub enum TriggerDirection {
    /// Trigger when oracle_price >= trigger_price (TP for longs, SL for shorts).
    Above,

    /// Trigger when oracle_price <= trigger_price (SL for longs, TP for shorts).
    Below,
}

/// A resting limit order, waiting to be fulfilled.
///
/// This struct does not contain the pair ID, order ID, and the limit price,
/// which are instead included in the storage key, with which this struct is
/// saved in the contract storage.
#[dango_primitives::derive(Serde, Borsh)]
pub struct LimitOrder {
    pub user: Addr,
    pub size: Quantity,
    pub reduce_only: bool,
    pub reserved_margin: UsdValue,
    pub created_at: Timestamp,

    /// Take-profit child order to apply when this order fills.
    pub tp: Option<ChildOrder>,

    /// Stop-loss child order to apply when this order fills.
    pub sl: Option<ChildOrder>,

    /// Caller-assigned id used to look this order up via the
    /// `client_order_id` index on `BIDS`/`ASKS`. `None` if the order was
    /// submitted without one.
    pub client_order_id: Option<ClientOrderId>,
}

/// A conditional order stored off-book until triggered.
#[dango_primitives::derive(Serde, Borsh)]
pub struct ConditionalOrder {
    /// Internal ID for price-time priority tiebreaking during cron execution.
    pub order_id: ConditionalOrderId,

    /// Size to close. If `Some`, the sign must oppose the position (negative for
    /// closing longs, positive for closing shorts). If `None`, closes the entire
    /// position at trigger time.
    pub size: Option<Quantity>,

    /// Oracle price that activates this order.
    pub trigger_price: UsdPrice,

    /// Max slippage for the market order executed at trigger.
    pub max_slippage: Dimensionless,
}

/// TP or SL parameters attached to a parent order as a "child order".
/// Applied to the resulting position when the parent order fills.
#[dango_primitives::derive(Serde, Borsh)]
pub struct ChildOrder {
    /// Oracle price that activates this order.
    pub trigger_price: UsdPrice,

    /// Max slippage for the market order executed at trigger.
    pub max_slippage: Dimensionless,

    /// Size to close. If `None`, closes the entire position at trigger time.
    pub size: Option<Quantity>,
}

#[dango_primitives::derive(Serde)]
pub struct QueryOrderResponse {
    pub user: Addr,
    pub pair_id: PairId,
    pub size: Quantity,
    pub limit_price: UsdPrice,
    pub reduce_only: bool,
    pub reserved_margin: UsdValue,
    pub created_at: Timestamp,
    pub tp: Option<ChildOrder>,
    pub sl: Option<ChildOrder>,
    pub client_order_id: Option<ClientOrderId>,
}

/// Response type of the order-by-client-order-ID query.
///
/// Includes the system-assigned `order_id`, which the caller — knowing only
/// the client order ID — is typically after. The query inputs (`user`,
/// `client_order_id`) are not echoed back, consistent with the other order
/// query responses.
#[dango_primitives::derive(Serde)]
pub struct QueryOrderByClientOrderIdResponse {
    pub order_id: OrderId,
    pub pair_id: PairId,
    pub size: Quantity,
    pub limit_price: UsdPrice,
    pub reduce_only: bool,
    pub reserved_margin: UsdValue,
    pub created_at: Timestamp,
    pub tp: Option<ChildOrder>,
    pub sl: Option<ChildOrder>,
}

#[dango_primitives::derive(Serde)]
pub struct QueryOrdersByUserResponseItem {
    pub pair_id: PairId,
    pub size: Quantity,
    pub limit_price: UsdPrice,
    pub reduce_only: bool,
    pub reserved_margin: UsdValue,
    pub created_at: Timestamp,
    pub tp: Option<ChildOrder>,
    pub sl: Option<ChildOrder>,
    pub client_order_id: Option<ClientOrderId>,
}

#[dango_primitives::derive(Serde)]
pub struct LiquidityDepth {
    /// Absolute order size aggregated in this bucket.
    pub size: Quantity,

    /// USD notional value aggregated in this bucket (size × price).
    pub notional: UsdValue,
}

#[dango_primitives::derive(Serde)]
pub struct LiquidityDepthResponse {
    pub bids: BTreeMap<UsdPrice, LiquidityDepth>,
    pub asks: BTreeMap<UsdPrice, LiquidityDepth>,
}
