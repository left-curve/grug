#[cfg(feature = "tracing")]
use tracing::instrument;
use {
    anyhow::bail,
    dango_archive_block_source::{BlockSource, GENESIS_HEIGHT},
    dango_archive_projection::{Committer, Projection},
    dango_archive_types::{AnyResult, BlockData, BlockDataExt},
    std::{cmp::Ordering, sync::Arc},
    tokio::sync::broadcast,
};

/// Drive a single projection: pull catch-up while behind the source's
/// frontier, then live-tail via the broadcast receiver. Transitions between
/// the two phases happen transparently — see `DESIGN.md` for the rationale.
///
/// The whole task runs in a `projection{id}` span, so every event it (and the
/// code it calls) emits is attributable to this projection.
#[cfg_attr(feature = "tracing", instrument(skip_all, name = "projection", fields(id = p.id())))]
pub async fn projection_loop(
    p: Arc<dyn Projection>,
    source: Arc<dyn BlockSource>,
    committer: Arc<dyn Committer>,
) -> AnyResult<()> {
    // Resume just past the committed cursor, or from the projection's
    // `min_height` on a cold start. `min_height` is `NonZero` (block 0 does not
    // exist), and we additionally clamp to `GENESIS_HEIGHT` — the source's own
    // floor, the authority on the lowest servable height — so `get(cursor)` can
    // never sit below what the store can serve while every broadcast is
    // `Greater`, a livelock where the projection never advances.
    let mut cursor = committer
        .cursor(p.id())
        .await?
        .map(|h| h + 1)
        .unwrap_or_else(|| p.min_height().get())
        .max(GENESIS_HEIGHT);

    #[cfg(feature = "tracing")]
    tracing::info!(projection = p.id(), cursor, "projection_loop starting");

    let mut maybe_rx = None;

    loop {
        // PHASE 1 — catch-up via pull. Keep reading until the source has
        // no block at `cursor` (i.e. we caught up to whatever it can serve
        // right now).
        while let Some(block) = source.get(cursor).await? {
            process_one(p.as_ref(), committer.as_ref(), &block).await?;
            cursor += 1;
        }

        let rx = maybe_rx.get_or_insert_with(|| source.subscribe());

        // PHASE 2 — live tail via broadcast.
        loop {
            match rx.recv().await {
                Ok(block) => match block.height().cmp(&cursor) {
                    Ordering::Less => {
                        continue;
                    },
                    Ordering::Equal => {
                        process_one(p.as_ref(), committer.as_ref(), &block).await?;
                        cursor += 1;
                    },
                    Ordering::Greater => {
                        break;
                    },
                },

                Err(broadcast::error::RecvError::Lagged(_skipped)) => {
                    // Broadcast buffer overflowed — we missed N blocks. Drop
                    // back to Phase 1 to recover via `get()` instead of
                    // racing the broadcast again.
                    #[cfg(feature = "metrics")]
                    metrics::counter!(crate::metrics::PROJECTION_LAGGED, "projection" => p.id())
                        .increment(1);
                    #[cfg(feature = "tracing")]
                    tracing::warn!(
                        projection = p.id(),
                        skipped = _skipped,
                        "broadcast lagged, falling back to catch-up"
                    );
                    break;
                },
                Err(broadcast::error::RecvError::Closed) => {
                    // All senders dropped while we're still running — this
                    // shouldn't happen with the current source lifecycle and
                    // indicates an upstream bug.
                    bail!(
                        "source broadcast closed unexpectedly for projection {}",
                        p.id()
                    );
                },
            }
        }
    }
}

/// One unit of work: open a write context, let the projection stage this
/// block's writes, then commit through the committer — ClickHouse flush +
/// ack first, then the Postgres transaction carrying the domain writes
/// together with the cursor update. See `DESIGN.md` § Commit protocol.
async fn process_one(
    p: &dyn Projection,
    committer: &dyn Committer,
    block: &BlockData,
) -> AnyResult<()> {
    let mut ctx = committer.begin(p.id()).await?;

    // Time staging and committing separately (both incl. their error paths), so
    // a slow projection is told apart from a slow database.
    #[cfg(feature = "metrics")]
    let process_start = std::time::Instant::now();
    let process_result = p.process(&mut ctx, block).await;
    #[cfg(feature = "metrics")]
    metrics::histogram!(crate::metrics::PROJECTION_PROCESS_DURATION, "projection" => p.id())
        .record(process_start.elapsed().as_secs_f64());
    process_result?;

    #[cfg(feature = "metrics")]
    let commit_start = std::time::Instant::now();
    let commit_result = committer.commit(ctx, p.id(), block.height()).await;
    #[cfg(feature = "metrics")]
    metrics::histogram!(crate::metrics::PROJECTION_COMMIT_DURATION, "projection" => p.id())
        .record(commit_start.elapsed().as_secs_f64());
    commit_result
}
