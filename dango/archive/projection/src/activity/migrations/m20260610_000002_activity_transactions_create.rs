use {
    crate::activity::idens::ActivityTransactions, sea_orm::DatabaseBackend,
    sea_orm_migration::prelude::*,
};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ActivityTransactions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ActivityTransactions::BlockHeight)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ActivityTransactions::Idx)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ActivityTransactions::Kind)
                            .small_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ActivityTransactions::Hash).binary().null())
                    .col(ColumnDef::new(ActivityTransactions::Sender).binary().null())
                    .col(
                        ColumnDef::new(ActivityTransactions::Success)
                            .boolean()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ActivityTransactions::Timestamp)
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(ActivityTransactions::BlockHeight)
                            .col(ActivityTransactions::Idx)
                            .col(ActivityTransactions::Kind),
                    )
                    .to_owned(),
            )
            .await?;

        // `(sender, block_height, idx)` partial — the sender side of query 1
        // (txs where X is the sender), recency pagination via backward index
        // scan, excludes cron. `(hash)` partial — hash lookup. Postgres
        // expresses the partials with raw SQL; other backends get the plain
        // (non-partial) index.
        match manager.get_database_backend() {
            DatabaseBackend::Postgres => {
                let conn = manager.get_connection();
                conn.execute_unprepared(
                    "CREATE INDEX IF NOT EXISTS idx_activity_transactions_sender \
                     ON activity_transactions (sender, block_height, idx) \
                     WHERE sender IS NOT NULL",
                )
                .await?;
                conn.execute_unprepared(
                    "CREATE INDEX IF NOT EXISTS idx_activity_transactions_hash \
                     ON activity_transactions (hash) \
                     WHERE hash IS NOT NULL",
                )
                .await?;
            },
            _ => {
                manager
                    .create_index(
                        Index::create()
                            .if_not_exists()
                            .name("idx_activity_transactions_sender")
                            .table(ActivityTransactions::Table)
                            .col(ActivityTransactions::Sender)
                            .col(ActivityTransactions::BlockHeight)
                            .col(ActivityTransactions::Idx)
                            .to_owned(),
                    )
                    .await?;
                manager
                    .create_index(
                        Index::create()
                            .if_not_exists()
                            .name("idx_activity_transactions_hash")
                            .table(ActivityTransactions::Table)
                            .col(ActivityTransactions::Hash)
                            .to_owned(),
                    )
                    .await?;
            },
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ActivityTransactions::Table).to_owned())
            .await
    }
}
