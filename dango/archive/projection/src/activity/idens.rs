use sea_orm_migration::prelude::DeriveIden;

/// Identifiers for the activity projection's three tables, used by its
/// migrations. A unit variant named `Table` resolves to the enum name in
/// `snake_case`, so these produce `activity_transactions`, `activity_events`
/// and `activity_event_data` — the `…activity…` prefix that keeps names unique
/// across the app's shared schema and migration history.

#[derive(DeriveIden)]
pub(super) enum ActivityTransactions {
    Table,
    BlockHeight,
    Idx,
    Kind,
    Hash,
    Sender,
    Success,
    Timestamp,
}

#[derive(DeriveIden)]
pub(super) enum ActivityEvents {
    Table,
    Address,
    BlockHeight,
    Category,
    CategoryIndex,
    EventIndex,
    EventType,
    Contract,
    ContractEventName,
}

#[derive(DeriveIden)]
pub(super) enum ActivityEventData {
    Table,
    BlockHeight,
    Category,
    CategoryIndex,
    EventIndex,
    Data,
}
