"""Typed wire shapes and dango_decimal helper used across the SDK."""

from __future__ import annotations

from dataclasses import dataclass
from decimal import Decimal, InvalidOperation
from enum import StrEnum
from typing import Any, Final, Literal, NewType, NotRequired, TypedDict

# --- Numeric helper ----------------------------------------------------------

_DEFAULT_MAX_PLACES: Final[int] = 6


def dango_decimal(x: float | int | str | Decimal, max_places: int = _DEFAULT_MAX_PLACES) -> str:
    """Return the canonical fixed-decimal string form of x; raise if precision lost."""
    if isinstance(x, bool) or not isinstance(x, int | float | str | Decimal):
        raise TypeError(f"unsupported type for dango_decimal: {type(x).__name__}")

    if isinstance(x, float):
        if x != x or x in (float("inf"), float("-inf")):
            raise ValueError(f"dango_decimal does not accept non-finite floats: {x!r}")
        d = Decimal(str(x))
    elif isinstance(x, str):
        try:
            d = Decimal(x)
        except InvalidOperation as exc:
            raise ValueError(f"invalid decimal string: {x!r}") from exc
    elif isinstance(x, int):
        d = Decimal(x)
    else:
        d = x

    if not d.is_finite():
        raise ValueError(f"dango_decimal does not accept non-finite Decimals: {x!r}")

    exponent = d.as_tuple().exponent
    if isinstance(exponent, int) and exponent < -max_places:
        raise ValueError(f"value {x!r} requires more than {max_places} decimal places of precision")

    return f"{d:.{max_places}f}"


# --- Identifier aliases ------------------------------------------------------

Addr = NewType("Addr", str)
Hash256 = NewType("Hash256", str)
Binary = NewType("Binary", str)
PairId = NewType("PairId", str)
OrderId = NewType("OrderId", str)
ConditionalOrderId = NewType("ConditionalOrderId", str)
FillId = NewType("FillId", str)
ClientOrderId = NewType("ClientOrderId", str)
UserIndex = NewType("UserIndex", int)
Nonce = NewType("Nonce", int)

Dimensionless = NewType("Dimensionless", str)
Quantity = NewType("Quantity", str)
UsdValue = NewType("UsdValue", str)
UsdPrice = NewType("UsdPrice", str)
FundingPerUnit = NewType("FundingPerUnit", str)
FundingRate = NewType("FundingRate", str)
Days = NewType("Days", str)

Timestamp = NewType("Timestamp", str)
Duration = NewType("Duration", str)

Uint64 = NewType("Uint64", str)
Uint128 = NewType("Uint128", str)

Referrer = NewType("Referrer", int)
Referee = NewType("Referee", int)
FeeShareRatio = NewType("FeeShareRatio", str)
CommissionRate = NewType("CommissionRate", str)


# --- Enums -------------------------------------------------------------------


class TimeInForce(StrEnum):
    GTC = "GTC"
    IOC = "IOC"
    POST = "POST"


class TriggerDirection(StrEnum):
    ABOVE = "above"
    BELOW = "below"


class CandleInterval(StrEnum):
    """GraphQL indexer enum form; the contract's strum-Display uses `1s`/`1m`/... separately."""

    ONE_SECOND = "ONE_SECOND"
    ONE_MINUTE = "ONE_MINUTE"
    FIVE_MINUTES = "FIVE_MINUTES"
    FIFTEEN_MINUTES = "FIFTEEN_MINUTES"
    ONE_HOUR = "ONE_HOUR"
    FOUR_HOURS = "FOUR_HOURS"
    ONE_DAY = "ONE_DAY"
    ONE_WEEK = "ONE_WEEK"


class ReasonForOrderRemoval(StrEnum):
    FILLED = "filled"
    CANCELED = "canceled"
    POSITION_CLOSED = "position_closed"
    SELF_TRADE_PREVENTION = "self_trade_prevention"
    LIQUIDATED = "liquidated"
    DELEVERAGED = "deleveraged"
    SLIPPAGE_EXCEEDED = "slippage_exceeded"
    PRICE_BAND_VIOLATION = "price_band_violation"
    SLIPPAGE_CAP_TIGHTENED = "slippage_cap_tightened"


class KeyType(StrEnum):
    # `KeyType` is read at the GraphQL boundary (the indexer exposes it via
    # `graphql(name = "SECP256K1")` etc.) rather than via Rust serde, so the
    # wire form is uppercase, not the snake_case used by the auth Key/Signature
    # variants below. See dango/types/src/auth/key.rs for the source mapping.
    SECP256R1 = "SECP256R1"
    SECP256K1 = "SECP256K1"
    ETHEREUM = "ETHEREUM"


class AccountStatus(StrEnum):
    INACTIVE = "inactive"
    ACTIVE = "active"
    FROZEN = "frozen"


class PerpsEventSortBy(StrEnum):
    """`PerpsEventSortBy` enum from the indexer GraphQL schema."""

    # The indexer accepts only these two values; the GraphQL schema does NOT
    # expose ordering by other fields. `BLOCK_HEIGHT_DESC` is the server-side
    # default, so the SDK mirrors that as the Python kwarg default too.
    BLOCK_HEIGHT_ASC = "BLOCK_HEIGHT_ASC"
    BLOCK_HEIGHT_DESC = "BLOCK_HEIGHT_DESC"


# --- Auth: Key / Signature / Credential primitives ---------------------------


class _KeySecp256r1(TypedDict):
    secp256r1: Binary


class _KeySecp256k1(TypedDict):
    secp256k1: Binary


class _KeyEthereum(TypedDict):
    ethereum: Addr


Key = _KeySecp256r1 | _KeySecp256k1 | _KeyEthereum


ClientData = TypedDict(  # noqa: UP013
    "ClientData",
    {
        "type": str,
        "challenge": str,
        "origin": str,
        "crossOrigin": NotRequired[bool | None],
    },
)


class PasskeySignature(TypedDict):
    authenticator_data: Binary
    client_data: Binary
    sig: Binary


class Eip712Signature(TypedDict):
    typed_data: Binary
    sig: Binary


class _SignaturePasskey(TypedDict):
    passkey: PasskeySignature


class _SignatureSecp256k1(TypedDict):
    secp256k1: Binary


class _SignatureEip712(TypedDict):
    eip712: Eip712Signature


Signature = _SignaturePasskey | _SignatureSecp256k1 | _SignatureEip712


class StandardCredential(TypedDict):
    key_hash: Hash256
    signature: Signature


class SessionInfo(TypedDict):
    chain_id: str
    session_key: Binary
    expire_at: Timestamp


class SessionCredential(TypedDict):
    session_info: SessionInfo
    session_signature: Binary
    authorization: StandardCredential


class _CredentialStandard(TypedDict):
    standard: StandardCredential


class _CredentialSession(TypedDict):
    session: SessionCredential


Credential = _CredentialStandard | _CredentialSession


class Metadata(TypedDict):
    user_index: UserIndex
    chain_id: str
    nonce: Nonce
    expiry: Timestamp | None


# --- Tx primitives -----------------------------------------------------------


Message = dict[str, object]


class SignDoc(TypedDict):
    sender: Addr
    gas_limit: int
    messages: list[Message]
    data: Metadata


class UnsignedTx(TypedDict):
    sender: Addr
    msgs: list[Message]
    data: Metadata


class Tx(TypedDict):
    sender: Addr
    gas_limit: int
    msgs: list[Message]
    data: Metadata
    credential: Credential


# --- Order primitives --------------------------------------------------------


class _MarketPayload(TypedDict):
    max_slippage: Dimensionless


class MarketKind(TypedDict):
    market: _MarketPayload


class _LimitPayload(TypedDict):
    limit_price: UsdPrice
    time_in_force: NotRequired[TimeInForce]
    client_order_id: NotRequired[ClientOrderId | None]


class LimitKind(TypedDict):
    limit: _LimitPayload


OrderKind = MarketKind | LimitKind


class ChildOrder(TypedDict):
    trigger_price: UsdPrice
    max_slippage: Dimensionless
    size: Quantity | None


class SubmitOrderRequest(TypedDict):
    pair_id: PairId
    size: Quantity
    kind: OrderKind
    reduce_only: bool
    tp: ChildOrder | None
    sl: ChildOrder | None


class _CancelOne(TypedDict):
    one: OrderId


class _CancelOneByClientOrderId(TypedDict):
    one_by_client_order_id: ClientOrderId


CancelOrderRequest = _CancelOne | _CancelOneByClientOrderId | Literal["all"]


class _SubmitOrCancelSubmit(TypedDict):
    submit: SubmitOrderRequest


class _SubmitOrCancelCancel(TypedDict):
    cancel: CancelOrderRequest


SubmitOrCancelOrderRequest = _SubmitOrCancelSubmit | _SubmitOrCancelCancel


# User-facing forms of the cancel/batch primitives. The wire-shape TypedDicts
# above are awkward to construct inline (you'd have to remember whether the
# inner key is `one` or `one_by_client_order_id`, and whether to wrap in
# `{"submit": ...}` or `{"cancel": ...}`); the dataclasses below are what
# callers actually pass into `Exchange.batch_update_orders` and
# `Exchange.cancel_order`. Exchange's private helpers translate them into the
# externally-tagged wire shape — so this is a pure ergonomics layer.
#
# `ClientOrderIdRef` is a tagged wrapper rather than a `NewType` because
# `OrderId` is already `NewType("OrderId", str)`, and at runtime both reduce
# to plain `str`/`int`. Without the wrapper, `cancel_order(7)` would be
# ambiguous (is 7 an OrderId or a ClientOrderId?). The dataclass forces an
# explicit choice at the call site without paying for inheritance.


@dataclass(frozen=True)
class ClientOrderIdRef:
    """Tagged wrapper around a client-assigned order id (Uint64 on the wire)."""

    value: int


@dataclass(frozen=True)
class SubmitAction:
    """User-facing form of SubmitOrderRequest for batch_update_orders."""

    pair_id: PairId
    size: float | int | str | Decimal
    kind: OrderKind
    reduce_only: bool = False
    tp: ChildOrder | None = None
    sl: ChildOrder | None = None


@dataclass(frozen=True)
class CancelAction:
    """User-facing form of CancelOrderRequest for batch_update_orders."""

    spec: OrderId | ClientOrderIdRef | Literal["all"]


SubmitOrCancelAction = SubmitAction | CancelAction


class _CancelConditionalOnePayload(TypedDict):
    pair_id: PairId
    trigger_direction: TriggerDirection


class _CancelConditionalOne(TypedDict):
    one: _CancelConditionalOnePayload


class _CancelConditionalAllForPairPayload(TypedDict):
    pair_id: PairId


class _CancelConditionalAllForPair(TypedDict):
    all_for_pair: _CancelConditionalAllForPairPayload


CancelConditionalOrderRequest = (
    _CancelConditionalOne | _CancelConditionalAllForPair | Literal["all"]
)


# User-facing forms of the conditional cancel primitives. Same rationale as
# the order-side dataclasses above: the wire-shape TypedDicts (`one` /
# `all_for_pair`) are awkward to construct inline, so callers pass these
# dataclasses and `Exchange._build_cancel_conditional_wire` translates
# into the externally-tagged wire shape.


@dataclass(frozen=True)
class ConditionalOrderRef:
    """User-facing form of CancelConditionalOrderRequest::One — a single conditional order."""

    pair_id: PairId
    trigger_direction: TriggerDirection


@dataclass(frozen=True)
class AllForPair:
    """User-facing form of CancelConditionalOrderRequest::AllForPair — every CO for one pair."""

    pair_id: PairId


CancelConditionalSpec = ConditionalOrderRef | AllForPair | Literal["all"]


class ConditionalOrder(TypedDict):
    order_id: ConditionalOrderId
    size: Quantity | None
    trigger_price: UsdPrice
    max_slippage: Dimensionless


# --- Position / user state ---------------------------------------------------


class Position(TypedDict):
    size: Quantity
    entry_price: UsdPrice
    entry_funding_per_unit: FundingPerUnit
    conditional_order_above: ConditionalOrder | None
    conditional_order_below: ConditionalOrder | None


class PositionExtended(TypedDict):
    size: Quantity
    entry_price: UsdPrice
    entry_funding_per_unit: FundingPerUnit
    conditional_order_above: ConditionalOrder | None
    conditional_order_below: ConditionalOrder | None
    unrealized_pnl: UsdValue | None
    unrealized_funding: UsdValue | None
    liquidation_price: UsdPrice | None


class Unlock(TypedDict):
    end_time: Timestamp
    amount_to_release: UsdValue


class UserState(TypedDict):
    margin: UsdValue
    vault_shares: Uint128
    positions: dict[PairId, Position]
    unlocks: list[Unlock]
    reserved_margin: UsdValue
    open_order_count: int


class UserStateExtended(TypedDict):
    margin: UsdValue
    vault_shares: Uint128
    unlocks: list[Unlock]
    reserved_margin: UsdValue
    open_order_count: int
    equity: UsdValue | None
    available_margin: UsdValue | None
    maintenance_margin: UsdValue | None
    positions: dict[PairId, PositionExtended]


# --- Pair / market data ------------------------------------------------------


class RateSchedule(TypedDict):
    base: Dimensionless
    tiers: dict[UsdValue, Dimensionless]


class ReferrerSettings(TypedDict):
    commission_rate: CommissionRate
    share_ratio: FeeShareRatio


class RefereeStats(TypedDict):
    registered_at: Timestamp
    volume: UsdValue
    commission_earned: UsdValue
    last_day_active: Timestamp


class Param(TypedDict):
    max_unlocks: int
    max_open_orders: int
    maker_fee_rates: RateSchedule
    taker_fee_rates: RateSchedule
    protocol_fee_rate: Dimensionless
    liquidation_fee_rate: Dimensionless
    liquidation_buffer_ratio: Dimensionless
    funding_period: Duration
    vault_total_weight: Dimensionless
    vault_cooldown_period: Duration
    referral_active: bool
    min_referrer_volume: UsdValue
    referrer_commission_rates: RateSchedule
    vault_deposit_cap: UsdValue | None
    max_action_batch_size: int
    trading_enabled: bool


class State(TypedDict):
    last_funding_time: Timestamp
    vault_share_supply: Uint128
    insurance_fund: UsdValue
    treasury: UsdValue


class PairParam(TypedDict):
    tick_size: UsdPrice
    min_order_size: UsdValue
    max_limit_price_deviation: Dimensionless
    max_market_slippage: Dimensionless
    max_abs_oi: Quantity
    max_abs_funding_rate: FundingRate
    initial_margin_ratio: Dimensionless
    maintenance_margin_ratio: Dimensionless
    impact_size: UsdValue
    vault_liquidity_weight: Dimensionless
    vault_half_spread: Dimensionless
    vault_max_quote_size: Quantity
    vault_size_skew_factor: Dimensionless
    vault_spread_skew_factor: Dimensionless
    vault_max_skew_size: Quantity
    funding_rate_multiplier: Dimensionless
    bucket_sizes: list[UsdPrice]


class PairState(TypedDict):
    long_oi: Quantity
    short_oi: Quantity
    funding_per_unit: FundingPerUnit
    funding_rate: FundingRate


class LiquidityDepth(TypedDict):
    size: Quantity
    notional: UsdValue


class LiquidityDepthResponse(TypedDict):
    bids: dict[UsdPrice, LiquidityDepth]
    asks: dict[UsdPrice, LiquidityDepth]


class QueryOrderResponse(TypedDict):
    user: Addr
    pair_id: PairId
    size: Quantity
    limit_price: UsdPrice
    reduce_only: bool
    reserved_margin: UsdValue
    created_at: Timestamp


class QueryOrdersByUserResponseItem(TypedDict):
    pair_id: PairId
    size: Quantity
    limit_price: UsdPrice
    reduce_only: bool
    reserved_margin: UsdValue
    created_at: Timestamp


class UserReferralData(TypedDict):
    volume: UsdValue
    commission_shared_by_referrer: UsdValue
    referee_count: int
    referees_volume: UsdValue
    commission_earned_from_referees: UsdValue
    cumulative_daily_active_referees: int
    cumulative_global_active_referees: int


# --- Events ------------------------------------------------------------------


class Deposited(TypedDict):
    user: Addr
    amount: UsdValue


class Withdrew(TypedDict):
    user: Addr
    amount: UsdValue


class LiquidityAdded(TypedDict):
    user: Addr
    amount: UsdValue
    shares_minted: Uint128


class LiquidityUnlocking(TypedDict):
    user: Addr
    amount: UsdValue
    shares_burned: Uint128
    end_time: Timestamp


class LiquidityReleased(TypedDict):
    user: Addr
    amount: UsdValue


class OrderFilled(TypedDict):
    order_id: OrderId
    pair_id: PairId
    user: Addr
    fill_price: UsdPrice
    fill_size: Quantity
    closing_size: Quantity
    opening_size: Quantity
    realized_pnl: UsdValue
    fee: UsdValue
    client_order_id: ClientOrderId | None
    fill_id: FillId | None
    is_maker: bool | None


class OrderPersisted(TypedDict):
    order_id: OrderId
    pair_id: PairId
    user: Addr
    limit_price: UsdPrice
    size: Quantity
    client_order_id: ClientOrderId | None


class OrderRemoved(TypedDict):
    order_id: OrderId
    pair_id: PairId
    user: Addr
    reason: ReasonForOrderRemoval
    client_order_id: ClientOrderId | None


class ConditionalOrderPlaced(TypedDict):
    pair_id: PairId
    user: Addr
    trigger_price: UsdPrice
    trigger_direction: TriggerDirection
    size: Quantity | None
    max_slippage: Dimensionless


class ConditionalOrderTriggered(TypedDict):
    pair_id: PairId
    user: Addr
    trigger_price: UsdPrice
    trigger_direction: TriggerDirection
    oracle_price: UsdPrice


class ConditionalOrderRemoved(TypedDict):
    pair_id: PairId
    user: Addr
    trigger_direction: TriggerDirection
    reason: ReasonForOrderRemoval


class Liquidated(TypedDict):
    user: Addr
    pair_id: PairId
    adl_size: Quantity
    adl_price: UsdPrice | None
    adl_realized_pnl: UsdValue


class Deleveraged(TypedDict):
    user: Addr
    pair_id: PairId
    closing_size: Quantity
    fill_price: UsdPrice
    realized_pnl: UsdValue


class BadDebtCovered(TypedDict):
    liquidated_user: Addr
    amount: UsdValue
    insurance_fund_remaining: UsdValue


class FeeDistributed(TypedDict):
    payer: UserIndex
    payer_addr: Addr
    protocol_fee: UsdValue
    vault_fee: UsdValue
    commissions: list[UsdValue]


class ReferralSet(TypedDict):
    referrer: UserIndex
    referee: UserIndex


# --- Indexer types -----------------------------------------------------------
#
# Convention boundary: everything above this point talks to the perps smart
# contract and uses snake_case keys (e.g. `pair_id`, `entry_price`) because
# Rust serde encodes contract-side structs that way. Everything below talks
# to the indexer GraphQL API, which speaks camelCase (e.g. `pairId`,
# `volumeUsd`, `price24HAgo`). The TypedDicts below deliberately keep the
# camelCase wire keys instead of auto-converting to snake_case for two
# reasons:
#
#   1. Fields like `volume24H` and `price24HAgo` don't round-trip cleanly
#      between camelCase and snake_case (the digit/letter boundary is
#      ambiguous), so any auto-conversion would either drop or duplicate
#      casing information.
#   2. Keeping wire-shape == Python-shape lets `cast()` be a true no-op:
#      the indexer JSON is already a valid `PerpsCandle`/`PerpsEvent` etc.,
#      with no field renames at the boundary. Callers can treat dict keys
#      as exactly what the GraphQL schema documents.
#
# `PageInfo` and `Connection[T]` are the only types in this section that use
# snake_case — they're frozen dataclasses, not wire shapes. They're user-
# facing control-flow types where Python convention wins, so we cross the
# boundary once in `_make_page_info` / `_make_connection` and keep the
# snake_case attribute names downstream.


class PerpsCandle(TypedDict):
    """One OHLCV candle from the indexer; keys are camelCase (wire shape)."""

    pairId: str  # noqa: N815
    interval: str  # `CandleInterval` enum value, e.g. "ONE_MINUTE".
    minBlockHeight: int  # noqa: N815
    maxBlockHeight: int  # noqa: N815
    open: str  # 6-decimal `BigDecimal` string.
    high: str
    low: str
    close: str
    volume: str
    volumeUsd: str  # noqa: N815
    timeStart: str  # noqa: N815  # ISO-8601 datetime.
    timeStartUnix: int  # noqa: N815  # Unix milliseconds (despite the "Unix" suffix).
    timeEnd: str  # noqa: N815
    timeEndUnix: int  # noqa: N815  # Unix milliseconds.


class PerpsEvent(TypedDict):
    """One indexer event record; keys are camelCase (wire shape)."""

    idx: int
    blockHeight: int  # noqa: N815
    txHash: str  # noqa: N815
    eventType: str  # noqa: N815
    userAddr: str  # noqa: N815
    pairId: str  # noqa: N815
    # The event payload is intentionally typed as an opaque dict because the
    # shape varies by `eventType` (each variant of the Rust event enum
    # serializes its own fields). Callers that want a typed view should
    # match on `eventType` and re-cast `data` to the corresponding TypedDict
    # from the events section above (e.g. `OrderFilled`, `Liquidated`).
    data: dict[str, Any]
    createdAt: str  # noqa: N815  # ISO-8601 datetime.


class PerpsPairStats(TypedDict):
    """24-hour price/volume stats for a pair; keys are camelCase (wire shape)."""

    pairId: str  # noqa: N815
    # `currentPrice`, `price24HAgo`, and `priceChange24H` can be null when
    # the pair has no recorded trades in the lookback window, so the Python
    # types are `str | None`.
    currentPrice: str | None  # noqa: N815
    price24HAgo: str | None  # noqa: N815
    volume24H: str  # noqa: N815  # Always populated; defaults to "0" on no trades.
    priceChange24H: str | None  # noqa: N815


class Trade(TypedDict):  # noqa: N815
    """Real-time perps trade fill from the perpsTrades subscription."""

    # Wire shape per `Subscription.perpsTrades` on the indexer
    # (indexer/graphql-types/src/schemas/schema.graphql) and API doc §8.2. Keys stay
    # camelCase for the same reasons as `PerpsCandle` / `PerpsEvent` —
    # see the convention-boundary comment at the top of this section.
    orderId: str  # noqa: N815
    pairId: str  # noqa: N815
    user: str
    fillPrice: str  # noqa: N815
    fillSize: str  # noqa: N815
    closingSize: str  # noqa: N815
    openingSize: str  # noqa: N815
    realizedPnl: str  # noqa: N815
    fee: str
    createdAt: str  # noqa: N815  # ISO-8601 datetime.
    blockHeight: int  # noqa: N815
    tradeIdx: int  # noqa: N815
    fillId: str | None  # noqa: N815
    isMaker: bool | None  # noqa: N815


class PerpsEvent2(TypedDict):
    """One perps-contract event from the /ws perpsEvents feed; camelCase (wire shape)."""

    # Mirrors the perps event the indexer emits on the `/ws` `perpsEvents`
    # channel.
    # `user` / `pairId` / `orderId` / `clientOrderId` are nullable: not every
    # event names a participant, market, or order (e.g. fee distribution), and
    # `clientOrderId` is set only when the order carried one. `data` is the
    # opaque per-event payload — match on `eventType` to interpret it.
    idx: int
    eventType: str  # noqa: N815
    user: str | None
    pairId: str | None  # noqa: N815
    orderId: str | None  # noqa: N815
    clientOrderId: str | None  # noqa: N815
    data: dict[str, Any]


class PerpsEvent2Batch(TypedDict):  # noqa: N815
    """One block's matching perps events from the /ws perpsEvents feed."""

    # Each `perpsEvents` frame on `subscribe_perps_events` is one of these: the
    # block's height/timestamp plus every perps event in it that passed the
    # filter. Only blocks with at least one matching event are delivered.
    blockHeight: int  # noqa: N815
    createdAt: str  # noqa: N815  # RFC 3339 datetime.
    events: list[PerpsEvent2]


class BlockTransaction(TypedDict):  # noqa: N815
    """One transaction inside a Block (subscribe_block stream)."""

    # The `data` and `credential` fields are JSON scalars whose schema
    # depends on the transaction kind, so they're typed as opaque dicts.
    hash: str
    blockHeight: int  # noqa: N815
    transactionType: str  # noqa: N815
    transactionIdx: int  # noqa: N815
    sender: str
    data: dict[str, Any]
    credential: dict[str, Any]
    hasSucceeded: bool  # noqa: N815
    errorMessage: str | None  # noqa: N815
    gasWanted: int  # noqa: N815
    gasUsed: int  # noqa: N815
    createdAt: str  # noqa: N815  # ISO-8601 datetime.


class BlockEvent(TypedDict):  # noqa: N815
    """One event inside a Block; same shape as the events subscription stream."""

    # Mirrors `subscriptions/events.graphql` exactly so callers can route
    # block-embedded events through the same handlers as
    # `subscribe_user_events` / per-event subscriptions.
    type: str
    method: str | None
    eventStatus: str  # noqa: N815
    commitmentStatus: str  # noqa: N815
    transactionType: int  # noqa: N815
    transactionIdx: int  # noqa: N815
    messageIdx: int | None  # noqa: N815
    eventIdx: int  # noqa: N815
    data: dict[str, Any]
    blockHeight: int  # noqa: N815
    createdAt: str  # noqa: N815


class Block(TypedDict):  # noqa: N815
    """Block payload from the block subscription."""

    # Each `next` message on `subscribe_block` is one of these. The
    # nested `transactions` and `flattenEvents` lists carry the
    # block's full content — `flattenEvents` is the indexer's already-
    # flattened event stream (cron events, message events, ...).
    blockHeight: int  # noqa: N815
    hash: str
    appHash: str  # noqa: N815
    createdAt: str  # noqa: N815
    cronsOutcomes: list[str]  # noqa: N815
    transactions: list[BlockTransaction]
    flattenEvents: list[BlockEvent]  # noqa: N815


@dataclass(frozen=True)
class PageInfo:
    """Cursor-pagination metadata; mirrors the GraphQL `PageInfo` object."""

    # `frozen=True` makes instances hashable and prevents accidental mutation
    # of cursor state mid-iteration. snake_case attribute names follow Python
    # convention because this is a user-facing dataclass, not a wire shape —
    # see the convention-boundary comment at the top of this section.
    has_previous_page: bool
    has_next_page: bool
    start_cursor: str | None
    end_cursor: str | None


@dataclass(frozen=True)
class Connection[T]:
    """A page of results plus its `PageInfo` cursors; generic over the node type."""

    # Modelled as a frozen dataclass rather than a TypedDict because:
    #   * Generic dataclasses pair naturally with PEP 695 syntax (`class
    #     Connection[T]:`); the equivalent on a TypedDict requires more
    #     ceremony with `Generic` plus a workaround for runtime subscripting.
    #   * It pairs naturally with `PageInfo` (also a dataclass) so the two
    #     user-facing types in this section share their idiomatic shape.
    # No methods are defined here yet, but keeping it a dataclass leaves
    # room to add e.g. `.is_last_page` or iteration helpers without
    # rewriting callers.
    nodes: list[T]
    page_info: PageInfo
