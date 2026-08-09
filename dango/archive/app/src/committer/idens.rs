use sea_orm_migration::prelude::DeriveIden;

/// Identifiers for the `projection_cursors` table, used by the
/// committer's migrations.
#[derive(DeriveIden)]
pub(super) enum ProjectionCursors {
    Table,
    Id,
    Height,
}
