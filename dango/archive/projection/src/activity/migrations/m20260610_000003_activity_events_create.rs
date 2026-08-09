use {
    crate::activity::idens::ActivityEvents,
    sea_orm::{ConnectionTrait, DatabaseBackend},
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
                    .table(ActivityEvents::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(ActivityEvents::Address).binary().not_null())
                    .col(
                        ColumnDef::new(ActivityEvents::BlockHeight)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ActivityEvents::Category)
                            .small_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ActivityEvents::CategoryIndex)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ActivityEvents::EventIndex)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ActivityEvents::EventType)
                            .small_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ActivityEvents::Contract).binary().null())
                    .col(
                        ColumnDef::new(ActivityEvents::ContractEventName)
                            .text()
                            .null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(ActivityEvents::Address)
                            .col(ActivityEvents::BlockHeight)
                            .col(ActivityEvents::Category)
                            .col(ActivityEvents::CategoryIndex)
                            .col(ActivityEvents::EventIndex),
                    )
                    .to_owned(),
            )
            .await?;

        // Four secondary indexes serving the eight documented feeds (a fifth,
        // `idx_activity_events_contract_name`, is added by a later migration to
        // seek the name-filtered contract feed — see below). All carry the
        // event-position `(block_height, category, category_index,
        // event_index)`, so each is a backward index scan ordered newest-first
        // (keyset-paginated). On the DISTINCT-ON feeds (by type, by contract)
        // `address` trails the position as the tiebreaker, so an event's
        // participant rows are adjacent and the resolver's `ORDER BY <position>
        // DESC, address DESC` is the backward scan verbatim — Index Scan →
        // Unique → Limit, with **no sort node** (verified via `EXPLAIN`; with
        // `address` ASC, or with `contract_event_name` wedged before `address`,
        // the planner adds an Incremental Sort). `contract_event_name` therefore
        // sits at the very **tail** of the contract index — here a pure
        // in-index `= ANY(...)` filter, not a seek column (the position
        // columns separate it from `contract`, so a name predicate walks the
        // contract's whole slice); a name-*filtered* feed instead seeks via
        // the companion `(contract, contract_event_name, …)` index. Always
        // paired with `contract`; at the tail it costs this index's ordering
        // nothing.
        //
        // The type feeds are plain indexes (`event_type` is NOT NULL). The
        // contract feeds are partial on `contract IS NOT NULL` — only contract
        // events carry a contract — which keeps them small. Plain and partial
        // indexes both work on Postgres and SQLite, so one raw statement per
        // index serves every backend.
        let conn = manager.get_connection();
        // Postgres-only: leave slack in the index leaf pages. Every index here is
        // address- / contract- / type-prefixed (a near-random prefix), so a
        // sustained insert stream scatters across the key space; a lower
        // fillfactor reduces page splits when the index is built or extended —
        // most valuable when an offline backfill (re)builds these on already
        // loaded data. Harmless otherwise; SQLite has no such storage knob.
        let storage = match manager.get_database_backend() {
            DatabaseBackend::Postgres => "WITH (fillfactor = 85)",
            _ => "",
        };
        let indexes: [(&str, &str, &str); 4] = [
            (
                "idx_activity_events_addr_type",
                "address, event_type, block_height, category, category_index, event_index",
                "",
            ),
            (
                "idx_activity_events_type",
                "event_type, block_height, category, category_index, event_index, address",
                "",
            ),
            (
                "idx_activity_events_addr_contract",
                "address, contract, block_height, category, category_index, event_index, contract_event_name",
                "WHERE contract IS NOT NULL",
            ),
            (
                "idx_activity_events_contract",
                "contract, block_height, category, category_index, event_index, address, contract_event_name",
                "WHERE contract IS NOT NULL",
            ),
        ];
        for (name, cols, filter) in indexes {
            conn.execute_unprepared(&format!(
                "CREATE INDEX IF NOT EXISTS {name} ON activity_events ({cols}) {storage} {filter}"
            ))
            .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ActivityEvents::Table).to_owned())
            .await
    }
}
