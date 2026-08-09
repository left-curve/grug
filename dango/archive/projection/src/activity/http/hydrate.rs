//! Eager raw-payload hydration for a feed's page.
//!
//! The feeds in [`super::feeds`] return cheap columns only; the read API then
//! fills each row's heavy detail from the unit's block before responding —
//! `tx` + `outcome` for a [`Transaction`], `data` for a non-priority [`Event`]
//! (priority events already carry their payload from the `event_data` join).
//! All of a page's rows are in hand at once, so the loads batch: one
//! [`load_blocks`] over the page's distinct heights, then a pass that hydrates
//! every row from the shared map.
//!
//! A height the source no longer holds — an old block pruned from the node's
//! cache under [`LocalBlockSource`](dango_archive_block_source::LocalBlockSource),
//! say — simply leaves that row's detail `null`, never failing the page. A block
//! that *is* present but is internally inconsistent with an indexed row (a unit
//! or event missing at its recorded index) is a data-integrity error, surfaced
//! as a 500.

use {
    super::types::{Event, Transaction, UnitKind, UnitOutcome},
    crate::activity::flatten_unit,
    dango_archive_block_source::{BlockSource, load_blocks},
    dango_archive_httpd::ApiError,
    std::sync::Arc,
};

/// Hydrate each transaction's `tx` + `outcome` from its block, batched. Every
/// unit needs its block (a cronjob has no `tx` but still has an `outcome`), so
/// every height is loaded.
pub(crate) async fn hydrate_transactions(
    source: &Arc<dyn BlockSource>,
    items: &mut [Transaction],
) -> Result<(), ApiError> {
    if items.is_empty() {
        return Ok(());
    }

    let heights = items.iter().map(|item| item.block_height);
    let blocks = load_blocks(source, heights).await;

    for item in items.iter_mut() {
        let Some(block) = blocks.get(&item.block_height) else {
            // The source no longer holds this block; `tx` / `outcome` stay null.
            continue;
        };
        let idx = item.idx as usize;

        let outcome = match item.kind {
            UnitKind::Transaction => {
                let (tx, _hash) = block
                    .block
                    .txs
                    .get(idx)
                    .ok_or_else(|| missing("transaction", item.idx, item.block_height))?;
                item.tx = Some(serde_json::to_value(tx)?);

                let out =
                    block.outcome.tx_outcomes.get(idx).cloned().ok_or_else(|| {
                        missing("transaction outcome", item.idx, item.block_height)
                    })?;
                UnitOutcome::Transaction(Box::new(out))
            },
            UnitKind::Cron => {
                let out = block
                    .outcome
                    .cron_outcomes
                    .get(idx)
                    .cloned()
                    .ok_or_else(|| missing("cron outcome", item.idx, item.block_height))?;
                UnitOutcome::Cron(Box::new(out))
            },
        };
        item.outcome = Some(serde_json::to_value(&outcome)?);
    }

    Ok(())
}

/// Hydrate each non-priority event's `data` from its block, batched. Priority
/// events already decoded their payload from the `event_data` join (their `data`
/// is `Some`), so only the rest load a block.
pub(crate) async fn hydrate_events(
    source: &Arc<dyn BlockSource>,
    items: &mut [Event],
) -> Result<(), ApiError> {
    let mut heights = items
        .iter()
        .filter(|event| event.data.is_none())
        .map(|event| event.block_height)
        .peekable();

    if heights.peek().is_none() {
        return Ok(());
    }
    let blocks = load_blocks(source, heights).await;

    for item in items.iter_mut() {
        if item.data.is_some() {
            // Priority payload already decoded in the feed.
            continue;
        }
        let Some(block) = blocks.get(&item.block_height) else {
            // The source no longer holds this block; `data` stays null.
            continue;
        };
        // Re-flatten the unit and pick the event by its recorded `event_index` —
        // the same numbering the write path stored (see `ActivityProjection`).
        let flat = flatten_unit(block, item.category.code(), item.category_index as usize);
        let info = flat
            .iter()
            .find(|info| info.id.event_index == item.event_index)
            .ok_or_else(|| {
                ApiError::Internal(format!(
                    "event {} missing from unit ({}, {}) of block {}",
                    item.event_index,
                    item.category.code(),
                    item.category_index,
                    item.block_height
                ))
            })?;
        item.data = Some(serde_json::to_value(&info.event)?);
    }

    Ok(())
}

/// A "`<what>` N missing from block H" error for a payload the block should have
/// held at this unit's index — a data-integrity failure, not a routine absence.
fn missing(what: &str, idx: u32, block_height: u64) -> ApiError {
    ApiError::Internal(format!("{what} {idx} missing from block {block_height}"))
}
