use {
    crate::{Ctx, Projection},
    async_trait::async_trait,
    dango_archive_types::AnyResult,
    std::sync::Arc,
};

/// Owner of projection cursors and of the commit protocol.
///
/// One instance is shared by all projection loops. The cursor table —
/// `projection_cursors (id, height)` in the indexer-owned Postgres — is
/// the single source of truth for every projection's progress.
///
/// The contract (see `DESIGN.md` § Commit protocol):
///
/// 1. [`cursor`] returns the last height whose [`commit`] completed for
///    this projection id; the loop resumes from the next height.
/// 2. [`commit`] flushes the context's ClickHouse buffer and awaits its
///    acknowledgement FIRST, and only then commits the Postgres
///    transaction carrying the domain writes together with the cursor
///    update. Reversing the order can leave the cursor ahead of
///    ClickHouse after a crash — a permanent, unrecoverable hole. The
///    protocol order only ever causes a replay, which the deduplication
///    token absorbs.
///
/// [`cursor`]: Committer::cursor
/// [`commit`]: Committer::commit
#[async_trait]
pub trait Committer: Send + Sync {
    /// Run all schema migrations at boot, before any loop starts,
    /// deriving them from the registered projections: the committer's
    /// own migrations first (app-level schema, an implementation
    /// detail), then each projection's [`migrations`] — Postgres,
    /// single shared `seaql_migrations` history, already-applied
    /// versions skipped, one transaction per migration — and
    /// [`ch_migrations`] — ClickHouse DDL, idempotent, untracked.
    ///
    /// Everything is derived from `projections`, so the schema that
    /// gets created always matches, by construction, the projections
    /// that run — there is no separate list to drift out of sync. The
    /// default does nothing — for backends with no schema to manage
    /// (e.g. test memory committers).
    ///
    /// [`migrations`]: Projection::migrations
    /// [`ch_migrations`]: Projection::ch_migrations
    async fn migrate(&self, _projections: &[Arc<dyn Projection>]) -> AnyResult<()> {
        Ok(())
    }

    /// Last height committed for `projection_id`.
    ///
    /// `None` if this projection never ran (or its id was just bumped).
    async fn cursor(&self, projection_id: &str) -> AnyResult<Option<u64>>;

    /// Open the write context for one unit of work.
    async fn begin(&self, projection_id: &str) -> AnyResult<Ctx>;

    /// Commit everything staged in `ctx`: ClickHouse first (flush + ack),
    /// then the Postgres transaction (domain writes + cursor = `height`).
    async fn commit(&self, ctx: Ctx, projection_id: &str, height: u64) -> AnyResult<()>;
}
