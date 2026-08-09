use {
    anyhow::{anyhow, ensure},
    dango_math::{IsZero, MultiplyFraction, Number, NumberConst, Uint128},
    dango_primitives::{
        Bound, DEFAULT_PAGE_LIMIT, Denom, Duration, ImmutableCtx, Inner, Order, QuerierExt,
        QuerierWrapper, StdError, StdResult, Storage, Timestamp,
    },
    dango_storage::{Item, Map},
    dango_types::gateway::{RateLimit, RateLimitStatus},
    std::collections::BTreeMap,
};

// --------------------------------- Constants ---------------------------------

/// Trailing window over which outbound withdraws accumulate against the cap.
pub const ROLLING_WINDOW: Duration = Duration::from_hours(24);

/// Cron-time horizon beyond which `WITHDRAW_VOLUMES` entries are pruned.
/// Set to twice the rolling window so that, between cron ticks, baseline
/// lookups at `now − 24h` always have data to read.
pub const PRUNE_HORIZON: Duration = Duration::from_hours(48);

// ---------------------------------- Storage ----------------------------------

pub const RATE_LIMITS: Item<BTreeMap<Denom, RateLimit>> = Item::new("rate_limits");

/// Per-denom supply snapshot, taken by the cron handler each refresh period
/// and seeded by the first `SetRateLimits` call that adds the denom.
/// Combined with the current `RATE_LIMITS` entry, this yields the outbound
/// cap as `supply × limit`. Snapshotting the supply (rather than the cap)
/// means inbound deposits between cron ticks cannot enlarge the cap.
pub const SUPPLY_SNAPSHOTS: Map<&Denom, Uint128> = Map::new("supply_snapshot");

/// Cumulative-to-date withdraw volume per `(denom, hour_bucket)`. The
/// trailing-24h sum is computed by subtracting the latest cumulative from
/// the cumulative recorded at-or-before `now − 24h`.
pub const WITHDRAW_VOLUMES: Map<(&Denom, Timestamp), Uint128> = Map::new("withdraw_volume");

// ---------------------------- Contract-facing API ----------------------------

/// Save the initial rate-limit map at instantiate time. Does not seed
/// supply snapshots — those are populated by the cron handler on its
/// first tick, or by the first `SetRateLimits` admin call that adds the
/// denom, whichever runs first. This matches the contract's pre-refactor
/// startup behavior: between genesis and the first cron tick the snapshot
/// is absent and `enforce` short-circuits, so outbound transfers are not
/// rate-limited yet. Seeding here would record `supply = 0` and freeze
/// every cap at zero until the first cron tick, which is not what genesis
/// callers expect.
pub fn init(
    storage: &mut dyn Storage,
    initial_limits: BTreeMap<Denom, RateLimit>,
) -> StdResult<()> {
    RATE_LIMITS.save(storage, &initial_limits)
}

/// Apply an admin update to the rate-limit map. Diffs the new map against
/// the stored one: for any denom dropped from the map, clears its supply
/// snapshot and its volume history so re-adding it later starts clean;
/// for any denom newly added, seeds a supply snapshot from current bank
/// supply.
///
/// Existing snapshots are left untouched on a configured-limit change so
/// the supply doesn't get refreshed mid-window — the cron tick remains
/// the sole path that moves an already-seeded snapshot.
///
/// Does NOT touch `PERSONAL_QUOTAS`. The caller is responsible for the
/// personal-quota revocation that goes hand-in-hand with a 0% rate limit
/// — personal quotas are not rate-limit machinery and live outside this
/// module.
pub fn apply_admin_update(
    storage: &mut dyn Storage,
    querier: QuerierWrapper,
    new_limits: BTreeMap<Denom, RateLimit>,
) -> StdResult<()> {
    let old_limits = RATE_LIMITS.load(storage)?;

    for denom in old_limits.keys() {
        if !new_limits.contains_key(denom) {
            SUPPLY_SNAPSHOTS.remove(storage, denom);
            clear_volumes(storage, denom);
        }
    }

    for denom in new_limits.keys() {
        if !SUPPLY_SNAPSHOTS.has(storage, denom) {
            let supply = querier.query_supply(denom.clone())?;
            SUPPLY_SNAPSHOTS.save(storage, denom, &supply)?;
        }
    }

    RATE_LIMITS.save(storage, &new_limits)
}

/// Check the trailing-24h rolling-window cap against the post-personal-
/// quota residue, without recording anything. The cap is derived from the
/// supply snapshot (frozen at the last cron tick) times the currently-
/// configured limit.
///
/// A missing `SUPPLY_SNAPSHOTS` entry means the denom is not rate-limited
/// at all and this is a no-op. A zero residue is also a no-op — the
/// caller may have fully covered the withdraw with a personal-quota
/// allowance, in which case nothing counts against the global window.
///
/// `requested` is the user-facing withdraw amount (post-fee, pre-PQ-
/// deduction) and is only used to build a debuggable error message;
/// `residue` is what actually counts against the cap. The personal-quota
/// deduction itself is the caller's responsibility — this function never
/// reads or writes `PERSONAL_QUOTAS`.
pub fn check(
    storage: &dyn Storage,
    denom: &Denom,
    now: Timestamp,
    requested: Uint128,
    residue: Uint128,
) -> anyhow::Result<()> {
    if residue.is_zero() {
        return Ok(());
    }

    let Some(supply) = SUPPLY_SNAPSHOTS.may_load(storage, denom)? else {
        return Ok(());
    };

    let limit = RATE_LIMITS
        .load(storage)?
        .get(denom)
        .copied()
        .ok_or_else(|| {
            anyhow!("supply snapshot present without matching rate limit for denom: {denom}")
        })?;

    let cap = supply.checked_mul_dec_floor(limit.into_inner())?;
    let used = rolling_window_sum(storage, denom, now)?;
    let after = used.checked_add(residue)?;

    ensure!(
        after <= cap,
        "insufficient outbound quota! denom: {}, requested: {}, residue after personal quota: {}, rolling sum: {}, cap: {}",
        denom,
        requested,
        residue,
        used,
        cap,
    );

    Ok(())
}

/// Record a residue that passed [`check`] into the current hour bucket,
/// consuming the quota. Called when a withdrawal is actually executed;
/// `check` alone serves the fail-fast validation at request time.
///
/// Records only when the residue actually counts against a tracked window:
/// for a denom that isn't rate-limited, recording would leave stray volume
/// entries that no cron tick ever prunes. A zero residue is a no-op.
pub fn record(
    storage: &mut dyn Storage,
    denom: &Denom,
    now: Timestamp,
    residue: Uint128,
) -> StdResult<()> {
    if !residue.is_zero() && SUPPLY_SNAPSHOTS.has(storage, denom) {
        record_withdraw(storage, denom, now, residue)?;
    }

    Ok(())
}

/// Cron-tick maintenance: refresh every rate-limited denom's supply
/// snapshot from current bank supply, then prune that denom's volume
/// entries older than the 48h retention horizon. Ordering matters — the
/// refresh runs first so that a failure short-circuits without leaving
/// half-pruned state behind.
pub fn tick(storage: &mut dyn Storage, querier: QuerierWrapper, now: Timestamp) -> StdResult<()> {
    refresh_supply_snapshots(storage, querier)?;

    for denom in RATE_LIMITS.load(storage)?.keys() {
        prune_old_volumes(storage, denom, now);
    }

    Ok(())
}

// ---------------------------------- Queries ----------------------------------

pub fn query_rate_limits(ctx: ImmutableCtx) -> StdResult<BTreeMap<Denom, RateLimit>> {
    RATE_LIMITS.load(ctx.storage)
}

pub fn query_rate_limit_status(
    ctx: ImmutableCtx,
    denom: Denom,
) -> StdResult<Option<RateLimitStatus>> {
    let Some(supply) = SUPPLY_SNAPSHOTS.may_load(ctx.storage, &denom)? else {
        return Ok(None);
    };

    let limit =
        RATE_LIMITS
            .load(ctx.storage)?
            .get(&denom)
            .copied()
            .ok_or(StdError::data_not_found::<RateLimit>(
                format!("rate_limit::{denom}").as_bytes(),
            ))?;
    let cap = supply.checked_mul_dec_floor(limit.into_inner())?;

    let used_in_last_24h = rolling_window_sum(ctx.storage, &denom, ctx.block.timestamp)?;

    Ok(Some(RateLimitStatus {
        supply_snapshot: supply,
        cap,
        used_in_last_24h,
    }))
}

pub fn query_rate_limit_statuses(
    ctx: ImmutableCtx,
    start_after: Option<Denom>,
    limit: Option<u32>,
) -> StdResult<BTreeMap<Denom, RateLimitStatus>> {
    let start = start_after.as_ref().map(Bound::Exclusive);
    let page = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;
    let rate_limits = RATE_LIMITS.load(ctx.storage)?;

    SUPPLY_SNAPSHOTS
        .range(ctx.storage, start, None, Order::Ascending)
        .take(page)
        .map(|res| {
            let (denom, supply) = res?;

            let limit = rate_limits
                .get(&denom)
                .copied()
                .ok_or(StdError::data_not_found::<RateLimit>(
                    format!("rate_limit::{denom}").as_bytes(),
                ))?;
            let cap = supply.checked_mul_dec_floor(limit.into_inner())?;

            let used_in_last_24h = rolling_window_sum(ctx.storage, &denom, ctx.block.timestamp)?;

            Ok((
                denom,
                RateLimitStatus {
                    supply_snapshot: supply,
                    cap,
                    used_in_last_24h,
                },
            ))
        })
        .collect()
}

// ----------------------------- Internal helpers ------------------------------

/// Refresh each rate-limited denom's supply snapshot from the bank's
/// current supply. The effective cap on the next withdraw is
/// `supply_snapshot × current_limit`, so deposits between cron ticks
/// cannot enlarge the cap.
fn refresh_supply_snapshots(storage: &mut dyn Storage, querier: QuerierWrapper) -> StdResult<()> {
    for denom in RATE_LIMITS.load(storage)?.keys() {
        let supply = querier.query_supply(denom.clone())?;
        SUPPLY_SNAPSHOTS.save(storage, denom, &supply)?;
    }

    Ok(())
}

/// Sum of withdraws in the trailing 24h, computed as
/// `latest_cumulative − cumulative_at_or_before(now − 24h)`.
fn rolling_window_sum(storage: &dyn Storage, denom: &Denom, now: Timestamp) -> StdResult<Uint128> {
    let latest = WITHDRAW_VOLUMES
        .prefix(denom)
        .values(storage, None, None, Order::Descending)
        .next()
        .transpose()?
        .unwrap_or(Uint128::ZERO);

    let baseline_ts = now.saturating_sub(ROLLING_WINDOW);
    let baseline = WITHDRAW_VOLUMES
        .prefix(denom)
        .values(
            storage,
            None,
            Some(Bound::Inclusive(baseline_ts)),
            Order::Descending,
        )
        .next()
        .transpose()?
        .unwrap_or(Uint128::ZERO);

    Ok(latest.checked_sub(baseline)?)
}

/// Add `amount` to the cumulative for the bucket at `now.truncate_to_hour()`.
/// Stored as `latest_cumulative + amount`, mirroring the perps volume pattern.
fn record_withdraw(
    storage: &mut dyn Storage,
    denom: &Denom,
    now: Timestamp,
    amount: Uint128,
) -> StdResult<()> {
    if amount.is_zero() {
        return Ok(());
    }

    let latest = WITHDRAW_VOLUMES
        .prefix(denom)
        .values(storage, None, None, Order::Descending)
        .next()
        .transpose()?
        .unwrap_or_default();

    WITHDRAW_VOLUMES.save(
        storage,
        (denom, now.truncate_to_hour()),
        &latest.checked_add(amount)?,
    )
}

/// Drop entries for `denom` strictly older than `now − 48h`. Keeps enough
/// history to serve baseline lookups for the next 24h of withdraws.
fn prune_old_volumes(storage: &mut dyn Storage, denom: &Denom, now: Timestamp) {
    let cutoff = now.saturating_sub(PRUNE_HORIZON);

    WITHDRAW_VOLUMES
        .prefix(denom)
        .clear(storage, None, Some(Bound::Exclusive(cutoff)));
}

/// Drop every entry for `denom`. Used when admin removes a denom from the
/// rate-limit map so re-adding it later starts with a clean rolling window.
fn clear_volumes(storage: &mut dyn Storage, denom: &Denom) {
    WITHDRAW_VOLUMES.prefix(denom).clear(storage, None, None);
}
