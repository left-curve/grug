use {
    sea_orm::{ConnectionTrait, DatabaseBackend},
    sea_orm_migration::prelude::*,
};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // A companion to `idx_activity_events_contract` for the **name-filtered**
        // contract-event feeds (`/events/contract?names=…`, `/events/perps`).
        //
        // That first index carries `contract_event_name` at its **tail** (after
        // the event position), which is right for the un-filtered feed — an
        // emitting contract's rows sit in position order, a clean backward scan.
        // But it leaves the name a *residual* filter: Postgres seeks to the
        // contract, then scans its whole slice checking the name per row. For a
        // name matching few or none of that contract's events (say a name only a
        // *different* contract emits) the scan walks the entire slice — tens of
        // millions of rows on a hot contract, seconds-to-minutes for what is
        // often an empty page.
        //
        // Leading with `(contract, contract_event_name)` turns the name into a
        // **seek**: the query jumps straight to the (contract, name) group,
        // whose rows are already in position order (the trailing columns are
        // `POS_DESC` + the `address` tiebreaker), so it stays a no-sort backward
        // scan and a zero-match page returns at once. The two indexes sit side
        // by side — no single column order serves both the filtered and the
        // un-filtered feed without a sort.
        //
        // Partial on `contract IS NOT NULL` (only contract events carry a
        // contract / name) to match its sibling and stay small; `fillfactor`
        // leaves leaf slack for the near-random `contract`-led prefix, exactly
        // like the other event indexes (Postgres only — SQLite has no such knob).
        let conn = manager.get_connection();
        let storage = match manager.get_database_backend() {
            DatabaseBackend::Postgres => "WITH (fillfactor = 85)",
            _ => "",
        };
        conn.execute_unprepared(&format!(
            "CREATE INDEX IF NOT EXISTS idx_activity_events_contract_name \
             ON activity_events \
             (contract, contract_event_name, block_height, category, category_index, event_index, address) \
             {storage} WHERE contract IS NOT NULL"
        ))
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_activity_events_contract_name")
            .await?;

        Ok(())
    }
}
