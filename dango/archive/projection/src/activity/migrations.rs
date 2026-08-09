mod m20260610_000002_activity_transactions_create;
mod m20260610_000003_activity_events_create;
mod m20260610_000004_activity_event_data_create;
mod m20260709_000005_activity_events_contract_name_index;

use sea_orm_migration::MigrationTrait;

/// The activity projection's Postgres migrations, in order. Surfaced through
/// [`Projection::migrations`](crate::Projection::migrations) and run by the
/// committer under the app's shared `seaql_migrations` history; file names are
/// `…activity…`-prefixed to stay unique across that history.
pub(super) fn migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![
        Box::new(m20260610_000002_activity_transactions_create::Migration),
        Box::new(m20260610_000003_activity_events_create::Migration),
        Box::new(m20260610_000004_activity_event_data_create::Migration),
        Box::new(m20260709_000005_activity_events_contract_name_index::Migration),
    ]
}
