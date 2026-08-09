/// The compact [`FlatEvent`] discriminant, moved to [`dango_archive_types`] so
/// clients (e.g. the SDK's `ArchiveClient`) share the exact spellings and
/// stored codes; re-exported here for the projection's many internal users.
/// What stays below is the projection-only taxonomy: which columns a
/// `FlatEvent` populates.
pub use dango_archive_types::EventType;
use dango_primitives::{Addr, FlatEvent};

// ---- per-event taxonomy used by the `events` table columns ----

/// The contract that emitted a contract event — the value of
/// `activity_events.contract`. Populated **only for `ContractEvent`**: the
/// contract feeds (queries 3/4/7/8) always filter `contract` together with
/// `type = ContractEvent`, so `contract IS NOT NULL` is made to mean exactly
/// "a contract event", and the other event kinds that happen to carry a
/// contract are deliberately left out (reversible — re-backfill if an "Execute
/// / Migrate by contract" axis is ever wanted). `None` for every other event.
pub(crate) fn event_contract(event: &FlatEvent) -> Option<Addr> {
    match event {
        FlatEvent::ContractEvent(e) => Some(e.contract),
        _ => None,
    }
}

/// The contract-event type string (`CheckedContractEvent.ty`) for the
/// `contract_event_name` column and query 4. `None` for every
/// non-contract-event flat event.
pub(crate) fn contract_event_name(event: &FlatEvent) -> Option<String> {
    match event {
        FlatEvent::ContractEvent(e) => Some(e.ty.clone()),
        _ => None,
    }
}
