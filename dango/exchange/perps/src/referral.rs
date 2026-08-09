mod apply_fee_commissions;
mod commission;
mod force_set_fee_share_ratio;
mod set_commission_rate_override;
mod set_fee_share_ratio;
mod set_referral;

pub use {
    apply_fee_commissions::*, commission::*, force_set_fee_share_ratio::*,
    set_commission_rate_override::*, set_fee_share_ratio::*, set_referral::*,
};

use {
    crate::state::USER_REFERRAL_DATA,
    dango_primitives::{
        Addr, Bound, Order as IterationOrder, QuerierExt, QuerierWrapper, StdResult, Storage,
        Timestamp,
    },
    dango_storage::StorageQuerier,
    dango_types::{
        account_factory::{self, UserIndex},
        perps::UserReferralData,
    },
    std::collections::BTreeMap,
};

/// Load the cumulative referral data for a user with an optional upper bound.
/// If not specified, return the latest data.
fn load_referral_data(
    storage: &dyn Storage,
    user_index: UserIndex,
    upper_bound: Option<Timestamp>,
) -> StdResult<UserReferralData> {
    let upper = upper_bound.map(Bound::Inclusive);

    USER_REFERRAL_DATA
        .prefix(user_index)
        .range(storage, None, upper, IterationOrder::Descending)
        .next()
        .transpose()
        .map(|opt| opt.map(|(_, data)| data).unwrap_or_default())
}

/// Resolve an address to a `UserIndex` via the account factory.
///
/// Returns `None` if the query fails (e.g. address is not a known account,
/// or the querier is not configured — as in unit tests).
// TODO: refactor to raw query (query_wasm_path).
fn retrieve_user_index(
    querier: QuerierWrapper,
    addr: Addr,
    account_factory: Addr,
    cache: &mut BTreeMap<Addr, Option<UserIndex>>,
) -> Option<UserIndex> {
    if let Some(cached) = cache.get(&addr) {
        return *cached;
    }

    querier
        .query_wasm_smart(
            account_factory,
            account_factory::QueryAccountRequest { address: addr },
        )
        .ok()
        .map(|account| account.owner)
}

/// Resolve the master account for a user via raw storage query.
///
/// Returns `None` if the query fails.
fn retrieve_master_account(
    querier: QuerierWrapper,
    user: UserIndex,
    account_factory: Addr,
) -> Option<Addr> {
    querier
        .may_query_wasm_path(account_factory, &dango_account_factory::USERS.path(user))
        .ok()
        .flatten()
        .map(|user: account_factory::User| user.master_account())
}
