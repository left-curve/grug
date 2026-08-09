use {
    clickhouse::{Client, RowOwned, RowWrite},
    dango_archive_types::AnyResult,
    futures::future::BoxFuture,
    sea_orm::DatabaseTransaction,
};

/// A staged ClickHouse insert, deferred until [`Committer::commit`]. The
/// closure receives a client already carrying the deduplication token for
/// this unit of work.
///
/// [`Committer::commit`]: crate::Committer::commit
pub type ChWrite = Box<dyn FnOnce(Client) -> BoxFuture<'static, AnyResult<()>> + Send>;

/// Write context for one unit of work — one block, or one catch-up batch
/// once the committer batches flushes.
///
/// Created by [`Committer::begin`] and consumed by [`Committer::commit`];
/// a projection only ever borrows it (`&mut Ctx`), so it can stage writes
/// but can never commit them.
///
/// - **Postgres** writes go through [`Ctx::pg`]: they ride the same
///   transaction as the projection's cursor update and commit atomically
///   with it. Exactly-once execution — non-idempotent logic is safe.
/// - **ClickHouse** rows are staged with [`Ctx::insert_ch`] and flushed by
///   the committer *before* the Postgres transaction commits, each insert
///   tagged with the deduplication token
///   `{projection_id}/{height}/{seq}`. At-least-once execution,
///   exactly-once effect: a post-crash replay re-stages the same inserts
///   in the same order, and the server discards them by token.
///
/// [`Committer::begin`]: crate::Committer::begin
/// [`Committer::commit`]: crate::Committer::commit
pub struct Ctx {
    pg: DatabaseTransaction,
    ch_writes: Vec<ChWrite>,
}

impl Ctx {
    pub fn new(pg: DatabaseTransaction) -> Self {
        Self {
            pg,
            ch_writes: Vec::new(),
        }
    }

    /// The Postgres transaction for this unit of work. Domain writes made
    /// through it commit atomically with the cursor update — never call
    /// commit/rollback on it yourself (you can't: that consumes the
    /// transaction, which this method only borrows).
    pub fn pg(&self) -> &DatabaseTransaction {
        &self.pg
    }

    /// Stage `rows` for insertion into the ClickHouse table `table`.
    ///
    /// Nothing is sent here: the committer flushes staged inserts at
    /// commit time, before the Postgres transaction commits. Tokens embed
    /// the stage index, so implementations must stage deterministically —
    /// same blocks, same inserts, same order (true for any straight-line
    /// `process`).
    pub fn insert_ch<T>(&mut self, table: &str, rows: Vec<T>)
    where
        T: RowOwned + RowWrite + Send + Sync,
    {
        if rows.is_empty() {
            return;
        }

        let table = table.to_string();

        self.ch_writes.push(Box::new(move |client| {
            Box::pin(async move {
                let mut insert = client.insert::<T>(&table).await?;
                for row in &rows {
                    insert.write(row).await?;
                }
                insert.end().await?;
                Ok(())
            })
        }));
    }

    /// Tear the context apart for committing. Requires ownership — only
    /// the committer, which owns the context, can reach this.
    pub fn into_parts(self) -> (DatabaseTransaction, Vec<ChWrite>) {
        (self.pg, self.ch_writes)
    }
}
