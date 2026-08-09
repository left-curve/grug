//! The activity read API's **storage glue**: decoding stored rows into the
//! shared wire types.
//!
//! The wire types themselves — the `UnitKind` / `AddressRole` argument enums
//! and the `Transaction` / `Event` JSON objects — live in
//! [`dango_archive_types`], shared with clients (e.g. the SDK's
//! `ArchiveClient`) so the wire format cannot drift; they are re-exported here
//! for the handlers and feeds. This module keeps what only the server needs:
//! the sea-orm row structs and the stored-code / stored-bytes decoders.
//!
//! The heavy detail — a unit's full `tx` / `outcome`, an event's decoded `data`
//! — is **always** hydrated eagerly from the unit's block (see [`super::hydrate`])
//! and lands in the `tx` / `outcome` / `data` fields; the feeds in
//! [`super::feeds`] leave them `None` and the handler fills them before
//! responding.

pub(crate) use dango_archive_types::{AddressRole, Event, Transaction, UnitKind};
use {
    crate::activity::{decompress_event, entity::transactions, event_type::EventType},
    dango_archive_httpd::ApiError,
    dango_primitives::{Addr, Hash256, Timestamp},
    sea_orm::FromQueryResult,
    serde::Serialize,
};

// ---- stored-bytes decoders ----

/// `Addr` from stored bytes — a stored address is always 20 bytes, so a
/// wrong-length value is a data-integrity error surfaced as a 500.
fn addr_from_bytes(bytes: Vec<u8>) -> Result<Addr, ApiError> {
    Addr::try_from(bytes)
        .map_err(|err| ApiError::Internal(format!("invalid stored address: {err}")))
}

/// `Hash256` from stored bytes — same contract as [`addr_from_bytes`].
fn hash_from_bytes(bytes: Vec<u8>) -> Result<Hash256, ApiError> {
    Hash256::try_from(bytes)
        .map_err(|err| ApiError::Internal(format!("invalid stored hash: {err}")))
}

/// `UnitKind` from its stored discriminant — an unknown code is a
/// data-integrity error surfaced as a 500.
fn unit_kind_from_code(code: i16) -> Result<UnitKind, ApiError> {
    UnitKind::from_code(code)
        .ok_or_else(|| ApiError::Internal(format!("unknown unit kind: {code}")))
}

// ---- event rows + output ----

/// One event feed row: the event's stored columns plus its payload blob from
/// the `event_data` join — `None` for non-priority events not stored there. The
/// `address` column the join also returns is unused (the `Event` object is
/// per-event, not per-participant), so it is simply not a field here.
#[derive(FromQueryResult)]
pub(crate) struct EventRow {
    pub block_height: i64,
    pub category: i16,
    pub category_index: i32,
    pub event_index: i32,
    pub event_type: i16,
    pub contract: Option<Vec<u8>>,
    pub contract_event_name: Option<String>,
    pub data: Option<Vec<u8>>,
}

/// Build an [`Event`] from a feed row, decoding the priority payload that rode
/// along in the `event_data` join. Non-priority rows (no joined blob) leave
/// `data` `None` for [`super::hydrate`] to fill from the block.
pub(crate) fn event_from_row(row: EventRow) -> Result<Event, ApiError> {
    let data = match &row.data {
        Some(blob) => Some(serde_json::to_value(decompress_event(blob)?)?),
        None => None,
    };
    Ok(Event {
        block_height: row.block_height as u64,
        category: unit_kind_from_code(row.category)?,
        category_index: row.category_index as u32,
        event_index: row.event_index as u32,
        ty: EventType::from_code(row.event_type)
            .ok_or_else(|| ApiError::Internal(format!("unknown event_type: {}", row.event_type)))?,
        contract: row.contract.map(addr_from_bytes).transpose()?,
        name: row.contract_event_name,
        data,
    })
}

/// Build a [`Transaction`] from its stored row, leaving `tx` / `outcome` `None`
/// for [`super::hydrate`] to fill from the block.
///
/// A free function, not `TryFrom`: with [`Transaction`] living in
/// [`dango_archive_types`], `impl TryFrom<transactions::Model> for Transaction`
/// would be an orphan impl.
pub(crate) fn transaction_from_model(model: transactions::Model) -> Result<Transaction, ApiError> {
    Ok(Transaction {
        block_height: model.block_height as u64,
        idx: model.idx as u32,
        kind: unit_kind_from_code(model.kind)?,
        hash: model.hash.map(hash_from_bytes).transpose()?,
        sender: model.sender.map(addr_from_bytes).transpose()?,
        success: model.success,
        timestamp: Timestamp::from_nanos(model.timestamp as u128).to_rfc3339_string(),
        tx: None,
        outcome: None,
    })
}

/// A unit's execution outcome, as carried in its block: a [`TxOutcome`] for a
/// transaction, a [`CronOutcome`] for a cronjob. Serialized externally tagged —
/// `{"transaction": …}` / `{"cron": …}` — so the payload says which it is,
/// alongside the unit's `kind`. Built by [`super::hydrate`].
///
/// [`TxOutcome`]: dango_primitives::TxOutcome
/// [`CronOutcome`]: dango_primitives::CronOutcome
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UnitOutcome {
    // Boxed: a `TxOutcome` (with its full event tree) dwarfs a `CronOutcome`, so
    // an unboxed enum would carry the larger everywhere. Serialization is
    // transparent through the `Box`.
    Transaction(Box<dango_primitives::TxOutcome>),
    Cron(Box<dango_primitives::CronOutcome>),
}
