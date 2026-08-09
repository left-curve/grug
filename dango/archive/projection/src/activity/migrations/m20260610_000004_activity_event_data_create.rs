use {crate::activity::idens::ActivityEventData, sea_orm_migration::prelude::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Payload side-table: shares `activity_events`' positional key, holds
        // only the priority events' `zstd(borsh(event))`. No secondary indexes
        // — it is reached exclusively by point lookup on the event position
        // (the detail view); a missing row means "hydrate from the raw block".
        manager
            .create_table(
                Table::create()
                    .table(ActivityEventData::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ActivityEventData::BlockHeight)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ActivityEventData::Category)
                            .small_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ActivityEventData::CategoryIndex)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ActivityEventData::EventIndex)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ActivityEventData::Data).binary().not_null())
                    .primary_key(
                        Index::create()
                            .col(ActivityEventData::BlockHeight)
                            .col(ActivityEventData::Category)
                            .col(ActivityEventData::CategoryIndex)
                            .col(ActivityEventData::EventIndex),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ActivityEventData::Table).to_owned())
            .await
    }
}
