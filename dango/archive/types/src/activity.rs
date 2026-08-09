//! The activity read API's **wire types**: the `UnitKind` / `AddressRole` /
//! `EventType` enums used as feed arguments, and the `Transaction` / `Event`
//! JSON objects the feeds return.
//!
//! These are shared verbatim between the server side (the activity projection's
//! handlers, which serialize them) and clients (e.g. the SDK's `ArchiveClient`,
//! which deserializes them), so the wire format cannot drift between the two.
//! The projection's storage glue — row structs, column-code decoding into these
//! types, hydration — stays in the projection.
//!
//! Addresses and hashes are grug's own `Addr` / `Hash256`: their serde already
//! speaks the canonical text dialect — an address is lowercase `0x`-prefixed, a
//! hash bare uppercase — so the read API needs no wrapper newtype.

use {
    dango_primitives::{Addr, FlatEvent, Hash256},
    serde::{Deserialize, Serialize},
};

// ---- argument enums ----

/// How an `address` argument must relate to a unit in the
/// `transactionsInvolving` feed. Omitted ⇒ **either** — the address sent the
/// unit *or* was a party to one of its events (their union).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AddressRole {
    /// The address sent the unit (`transactions.sender`). Cron units have no
    /// sender, so this never matches a cronjob.
    Sender,
    /// The address is a party to one of the unit's events (the participation
    /// rows in `events`).
    Participant,
}

/// The kind of an executed unit — mirrors [`dango_primitives::FlatCategory`].
/// Serialized / parsed as `"cron"` / `"transaction"`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    Cron,
    Transaction,
}

impl UnitKind {
    /// The stored discriminant for this kind (the value in `transactions.kind`
    /// / `events.category`).
    #[must_use]
    pub fn code(self) -> i16 {
        match self {
            Self::Cron => dango_primitives::FlatCategory::Cron as i16,
            Self::Transaction => dango_primitives::FlatCategory::Tx as i16,
        }
    }

    /// The variant for a stored discriminant, or `None` if `code` is unknown —
    /// the inverse of [`code`](Self::code).
    #[must_use]
    pub fn from_code(code: i16) -> Option<Self> {
        match code {
            c if c == dango_primitives::FlatCategory::Cron as i16 => Some(Self::Cron),
            c if c == dango_primitives::FlatCategory::Tx as i16 => Some(Self::Transaction),
            _ => None,
        }
    }
}

/// Compact discriminant for a [`FlatEvent`], stored in `activity_events.event_type`
/// and used to key the projection's priority / involvement configuration.
///
/// Mirrors `FlatEvent`'s variants one-to-one. The `#[repr(i16)]` values are the
/// **stored codes**: they are part of the on-disk schema, so existing codes must
/// never be renumbered (append-only). [`From<&FlatEvent>`] is exhaustive, so a
/// new upstream variant is a compile error here until it is given a code.
///
/// Serialized snake_case (`transfer`, `contract_event`, …): the spelling the
/// read API accepts to filter the activity feeds by type and surfaces as an
/// event's `type`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
#[repr(i16)]
pub enum EventType {
    Configure = 0,
    Upgrade = 1,
    Transfer = 2,
    Upload = 3,
    Instantiate = 4,
    Execute = 5,
    Migrate = 6,
    Reply = 7,
    Authenticate = 8,
    Backrun = 9,
    Withhold = 10,
    Finalize = 11,
    Cron = 12,
    Guest = 13,
    ContractEvent = 14,
}

impl EventType {
    /// The stored discriminant (the value written to `event_type`).
    #[must_use]
    pub fn code(self) -> i16 {
        self as i16
    }

    /// The variant for a stored discriminant, or `None` if `code` is unknown —
    /// the inverse of [`code`](Self::code), used to surface `event_type` in the
    /// read API. Spelled out (not `transmute`) so an out-of-range value read
    /// from the database is a clean `None`, never undefined behaviour.
    #[must_use]
    pub fn from_code(code: i16) -> Option<Self> {
        Some(match code {
            0 => Self::Configure,
            1 => Self::Upgrade,
            2 => Self::Transfer,
            3 => Self::Upload,
            4 => Self::Instantiate,
            5 => Self::Execute,
            6 => Self::Migrate,
            7 => Self::Reply,
            8 => Self::Authenticate,
            9 => Self::Backrun,
            10 => Self::Withhold,
            11 => Self::Finalize,
            12 => Self::Cron,
            13 => Self::Guest,
            14 => Self::ContractEvent,
            _ => return None,
        })
    }
}

impl From<&FlatEvent> for EventType {
    fn from(event: &FlatEvent) -> Self {
        match event {
            FlatEvent::Configure(_) => Self::Configure,
            FlatEvent::Upgrade(_) => Self::Upgrade,
            FlatEvent::Transfer(_) => Self::Transfer,
            FlatEvent::Upload(_) => Self::Upload,
            FlatEvent::Instantiate(_) => Self::Instantiate,
            FlatEvent::Execute(_) => Self::Execute,
            FlatEvent::Migrate(_) => Self::Migrate,
            FlatEvent::Reply(_) => Self::Reply,
            FlatEvent::Authenticate(_) => Self::Authenticate,
            FlatEvent::Backrun(_) => Self::Backrun,
            FlatEvent::Withhold(_) => Self::Withhold,
            FlatEvent::Finalize(_) => Self::Finalize,
            FlatEvent::Cron(_) => Self::Cron,
            FlatEvent::Guest(_) => Self::Guest,
            FlatEvent::ContractEvent(_) => Self::ContractEvent,
        }
    }
}

// ---- feed objects ----

/// An indexed event, identified by its position. The participant set is **not**
/// a field — the address feeds return one row per (event × address) and the
/// attribute feeds collapse to the event. `contract` / `name` are present only
/// for contract events; `data` is the decoded payload, hydrated eagerly by the
/// server, `null` only if the source no longer holds the block.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// Height of the block the event was emitted in.
    pub block_height: u64,
    /// Kind of the enclosing unit (a transaction or a cronjob).
    pub category: UnitKind,
    /// Index of the enclosing unit within the block.
    pub category_index: u32,
    /// The event's 0-based position within its unit.
    pub event_index: u32,
    /// The event's type.
    #[serde(rename = "type")]
    pub ty: EventType,
    /// Emitting contract — present only for contract events.
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<String>))]
    pub contract: Option<Addr>,
    /// The contract-event name (`order_filled`, …) — present only for contract
    /// events.
    pub name: Option<String>,
    /// The decoded payload as JSON — a `FlatEvent`. Hydrated by the server;
    /// `null` when the block is unavailable.
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub data: Option<serde_json::Value>,
}

/// An executed unit — a transaction (`kind = "transaction"`) or a cronjob
/// (`kind = "cron"`). `hash` and `sender` are absent for cron units. The indexed
/// columns are cheap; the full submitted `tx` and the execution `outcome` are
/// hydrated eagerly by the server from the unit's block — `tx` is `null` for
/// cron units, both are `null` if the block is unavailable.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    /// Height of the block the unit executed in.
    pub block_height: u64,
    /// Index of the unit within the block (per kind).
    pub idx: u32,
    /// Whether this is a transaction or a cronjob.
    pub kind: UnitKind,
    /// Content hash — absent for cron units.
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<String>))]
    pub hash: Option<Hash256>,
    /// Account or contract that sent the unit — absent for cron units.
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<String>))]
    pub sender: Option<Addr>,
    /// Whether the unit's outcome was `Ok`.
    pub success: bool,
    /// Block time, RFC 3339 / ISO 8601 (UTC). A string, not an integer, so the
    /// nanosecond value never loses precision in 64-bit-lossy clients.
    pub timestamp: String,
    /// The full transaction as submitted on-chain (the `Tx` JSON the node
    /// serializes) — `null` for cron units. Hydrated from the block.
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub tx: Option<serde_json::Value>,
    /// The unit's execution outcome as JSON — externally tagged
    /// `{"transaction": …}` / `{"cron": …}`. Hydrated from the block.
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub outcome: Option<serde_json::Value>,
}

// ---- tests ----

#[cfg(test)]
mod tests {
    use super::*;

    /// The snake_case spellings are the wire format the read API accepts in
    /// `type` filters and surfaces in an event's `type` — pinned here so a
    /// rename cannot slip through unnoticed.
    #[test]
    fn event_type_serializes_snake_case() {
        for (ty, spelling) in [
            (EventType::Transfer, "\"transfer\""),
            (EventType::ContractEvent, "\"contract_event\""),
            (EventType::Cron, "\"cron\""),
        ] {
            assert_eq!(serde_json::to_string(&ty).unwrap(), spelling);
            assert_eq!(
                serde_json::from_str::<EventType>(spelling).unwrap(),
                ty,
                "the spelling must round-trip",
            );
        }
    }

    /// Every stored code round-trips through `from_code`, and codes are stable.
    #[test]
    fn event_type_codes_round_trip() {
        for code in 0..=14 {
            let ty = EventType::from_code(code).expect("known code");
            assert_eq!(ty.code(), code);
        }
        assert_eq!(EventType::from_code(15), None);
        assert_eq!(EventType::Transfer.code(), 2);
        assert_eq!(EventType::ContractEvent.code(), 14);
    }

    #[test]
    fn unit_kind_codes_round_trip() {
        for kind in [UnitKind::Cron, UnitKind::Transaction] {
            assert_eq!(UnitKind::from_code(kind.code()), Some(kind));
        }
    }
}
