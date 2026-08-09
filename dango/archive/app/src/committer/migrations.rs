//! Migrations for the tables the committer owns (the app-level schema).
//!
//! Declared here, executed — together with every projection's
//! migrations — by the runner in the parent module, under the single
//! shared `seaql_migrations` history.

mod m20260610_000001_create_projection_cursors;

use sea_orm_migration::MigrationTrait;

pub(super) fn migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![Box::new(
        m20260610_000001_create_projection_cursors::Migration,
    )]
}
