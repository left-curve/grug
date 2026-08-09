use sea_orm::entity::prelude::*;

/// Compressed payload (`zstd(borsh(event))`) for the configured priority event
/// types, split out of `activity_events` so that table stays narrow and
/// index-dense. Keyed by the event's position (shared with `activity_events`);
/// a **missing** row means the payload was not stored — hydrate it from the raw
/// block instead.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "activity_event_data")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub block_height: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub category: i16,
    #[sea_orm(primary_key, auto_increment = false)]
    pub category_index: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub event_index: i32,
    pub data: Vec<u8>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
