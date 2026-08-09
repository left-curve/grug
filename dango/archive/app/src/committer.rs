mod entity;
mod idens;
mod migrations;

use {
    anyhow::Context,
    async_trait::async_trait,
    dango_archive_projection::{Committer, Ctx, Projection},
    dango_archive_types::AnyResult,
    entity::projection_cursors,
    sea_orm::{
        ActiveValue::Set, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, Statement,
        TransactionTrait, sea_query::OnConflict,
    },
    sea_orm_migration::{MigrationTrait, SchemaManager},
    std::{collections::HashSet, sync::Arc},
};

/// Single shared migration history for the whole indexer — the
/// committer's migrations and every projection's run under this one
/// table. Same shape as sea-orm-migration's own tracking table, managed
/// here because `MigratorTrait::migrations()` is a static associated
/// function and cannot return a list assembled at runtime from the
/// registered projections.
const CREATE_MIGRATIONS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS seaql_migrations (
    version    TEXT   PRIMARY KEY,
    applied_at BIGINT NOT NULL
)
"#;

const SELECT_APPLIED: &str = r#"
SELECT version FROM seaql_migrations
"#;

const INSERT_APPLIED: &str = r#"
INSERT INTO seaql_migrations (version, applied_at) VALUES ($1, $2)
"#;

/// The concrete [`Committer`]: cursors in the indexer-owned Postgres,
/// staged ClickHouse inserts flushed with per-unit deduplication tokens.
///
/// Commit order is the protocol invariant (`DESIGN.md` § Commit protocol):
/// ClickHouse first — flush + await ack — then the Postgres transaction
/// carrying the projection's domain writes together with its cursor
/// update. A crash anywhere in between replays the block on restart, and
/// the dedup token turns the replayed CH inserts into server-side no-ops.
///
/// The CH client is optional: a deployment whose projections are all
/// PG-backed never connects to ClickHouse. Committing a context that
/// staged CH writes (or migrating projections that declare CH DDL)
/// without a configured client is an error.
pub struct PgChCommitter {
    pg: DatabaseConnection,
    ch: Option<clickhouse::Client>,
}

impl PgChCommitter {
    pub fn new(pg: DatabaseConnection, ch: Option<clickhouse::Client>) -> Self {
        Self { pg, ch }
    }
}

#[async_trait]
impl Committer for PgChCommitter {
    async fn migrate(&self, projections: &[Arc<dyn Projection>]) -> AnyResult<()> {
        // Assemble every owner's migrations: the committer's own
        // (app-level schema) first, then each registered projection's,
        // in registration order — so the schema that gets created
        // always matches the projections that run.
        let mut migrations = migrations::migrations();
        let mut ch_ddl = Vec::new();
        for p in projections {
            migrations.extend(p.migrations());
            ch_ddl.extend(p.ch_migrations());
        }

        // PG: shared history, already-applied versions skipped.
        run_pg_migrations(&self.pg, migrations).await?;

        // CH: idempotent DDL, re-run at every boot, no tracking.
        if !ch_ddl.is_empty() {
            let ch = self.ch.as_ref().context(
                "projections declared ClickHouse DDL, but no ClickHouse client is configured",
            )?;

            for ddl in ch_ddl {
                ch.query(&ddl).execute().await?;
            }
        }

        #[cfg(feature = "tracing")]
        tracing::info!("storage migrated");

        Ok(())
    }

    async fn cursor(&self, projection_id: &str) -> AnyResult<Option<u64>> {
        let row = projection_cursors::Entity::find_by_id(projection_id)
            .one(&self.pg)
            .await?;

        Ok(row.map(|cursor| cursor.height as u64))
    }

    async fn begin(&self, _projection_id: &str) -> AnyResult<Ctx> {
        Ok(Ctx::new(self.pg.begin().await?))
    }

    async fn commit(&self, ctx: Ctx, projection_id: &str, height: u64) -> AnyResult<()> {
        let (pg_txn, ch_writes) = ctx.into_parts();

        // 1. ClickHouse first: flush every staged insert and await its ack.
        // An error here drops `pg_txn`, which rolls the PG side back — the
        // cursor never gets ahead of ClickHouse. The token makes the
        // inevitable replay a server-side no-op.
        if !ch_writes.is_empty() {
            let ch = self.ch.as_ref().with_context(|| {
                format!("projection {projection_id} staged ClickHouse writes, but no ClickHouse client is configured")
            })?;

            for (seq, write) in ch_writes.into_iter().enumerate() {
                let token = format!("{projection_id}/{height}/{seq}");
                let client = ch.clone().with_setting("insert_deduplication_token", token);
                write(client).await?;
            }
        }

        // 2. Postgres: domain writes + cursor commit atomically.
        let cursor = projection_cursors::ActiveModel {
            id: Set(projection_id.to_string()),
            height: Set(height as i64),
        };

        projection_cursors::Entity::insert(cursor)
            .on_conflict(
                OnConflict::column(projection_cursors::Column::Id)
                    .update_column(projection_cursors::Column::Height)
                    .to_owned(),
            )
            .exec(&pg_txn)
            .await?;

        pg_txn.commit().await?;

        // The committer is the one place that sees every projection's cursor —
        // export the per-projection sync anchor (height + blocks) from here.
        #[cfg(feature = "metrics")]
        {
            metrics::gauge!(crate::metrics::PROJECTION_HEIGHT, "projection" => projection_id.to_string())
                .set(height as f64);
            metrics::counter!(crate::metrics::PROJECTION_BLOCKS, "projection" => projection_id.to_string())
                .increment(1);
        }

        #[cfg(feature = "tracing")]
        tracing::debug!(projection = projection_id, height, "unit of work committed");

        Ok(())
    }
}

/// Apply `migrations` in order under the shared `seaql_migrations`
/// history, skipping the ones already applied — same semantics and
/// table shape as sea-orm-migration's `MigratorTrait::up`, except the
/// list is assembled at runtime from the registered owners. Each
/// pending migration runs in its own transaction together with its
/// history row, so a crash can't leave a migration applied but
/// unrecorded (or vice versa).
async fn run_pg_migrations(
    db: &DatabaseConnection,
    migrations: Vec<Box<dyn MigrationTrait>>,
) -> AnyResult<()> {
    db.execute_unprepared(CREATE_MIGRATIONS_TABLE).await?;

    let applied = db
        .query_all(Statement::from_string(DbBackend::Postgres, SELECT_APPLIED))
        .await?
        .into_iter()
        .map(|row| row.try_get::<String>("", "version"))
        .collect::<Result<HashSet<_>, _>>()?;

    for migration in migrations {
        let name = migration.name();
        if applied.contains(name) {
            continue;
        }

        let txn = db.begin().await?;
        let manager = SchemaManager::new(&txn);

        migration
            .up(&manager)
            .await
            .with_context(|| format!("applying migration {name}"))?;

        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            INSERT_APPLIED,
            [name.into(), chrono::Utc::now().timestamp().into()],
        ))
        .await?;
        txn.commit().await?;

        #[cfg(feature = "tracing")]
        tracing::info!(migration = name, "migration applied");
    }

    Ok(())
}
