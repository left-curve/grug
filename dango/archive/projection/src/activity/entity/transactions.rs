use sea_orm::entity::prelude::*;

/// One row per executed unit — a transaction (`kind = 1`) or a cronjob
/// (`kind = 0`, matching [`dango_primitives::FlatCategory`]). Identity is the
/// position `(block_height, idx, kind)`; the `hash` is an indexed, non-unique
/// attribute (NULL for cron). No payload column: a unit's messages / credential
/// / error are hydrated from the raw block on the detail view.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "activity_transactions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub block_height: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub idx: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub kind: i16,
    /// Content hash (32 bytes); indexed, **not** unique. NULL for cron.
    pub hash: Option<Vec<u8>>,
    /// Account or contract sender (20 bytes). NULL for cron.
    pub sender: Option<Vec<u8>>,
    pub success: bool,
    /// Block time, unix nanoseconds.
    pub timestamp: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
