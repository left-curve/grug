use {
    crate::{
        account_factory,
        query::compute_user_volume,
        state::{COMMISSION_RATE_OVERRIDES, FEE_SHARE_RATIO, PARAM},
    },
    anyhow::ensure,
    dango_primitives::{MutableCtx, QuerierExt, Response},
    dango_types::{
        account_factory::{self},
        perps::FeeShareRatio,
    },
};

/// Maximum fee share ratio a referrer can set.
const MAX_FEE_SHARE_RATIO: FeeShareRatio = FeeShareRatio::new_percent(50);

/// Set or update the fee share ratio for the calling user (referrer).
///
/// The share ratio can only increase, never decrease, once set.
/// The caller must have traded at least `volume_to_be_referrer` in lifetime
/// perps volume.
pub fn set_fee_share_ratio(
    ctx: MutableCtx,
    share_ratio: FeeShareRatio,
) -> anyhow::Result<Response> {
    // Share ratio must be non-negative.
    ensure!(
        !share_ratio.is_negative(),
        "fee share ratio cannot be negative"
    );

    // Share ratio must not exceed the maximum.
    ensure!(
        share_ratio <= MAX_FEE_SHARE_RATIO,
        "fee share ratio cannot exceed {MAX_FEE_SHARE_RATIO}"
    );

    // Look up the caller's user index via the account factory.
    let account_factory = account_factory(ctx.querier);

    // TODO: refactor to raw query (query_wasm_path).
    let account = ctx.querier.query_wasm_smart(
        account_factory,
        account_factory::QueryAccountRequest {
            address: ctx.sender,
        },
    )?;

    let user_index = account.owner;

    // Users with a commission rate override bypass the volume requirement.
    // Otherwise, the caller must have enough lifetime perps volume across all
    // accounts belonging to the user.
    if !COMMISSION_RATE_OVERRIDES.has(ctx.storage, user_index) {
        let param = PARAM.load(ctx.storage)?;
        let volume =
            compute_user_volume(ctx.storage, ctx.querier, account_factory, user_index, None)?;

        ensure!(
            volume >= param.min_referrer_volume,
            "insufficient perps volume to become a referrer (required: {}, current: {})",
            param.min_referrer_volume,
            volume,
        );
    }

    // If already set, the new ratio must be >= the existing one.
    if let Some(existing) = FEE_SHARE_RATIO.may_load(ctx.storage, user_index)? {
        ensure!(
            share_ratio >= existing,
            "fee share ratio can only increase (current: {existing}, proposed: {share_ratio})"
        );
    }

    FEE_SHARE_RATIO.save(ctx.storage, user_index, &share_ratio)?;

    Ok(Response::new())
}
