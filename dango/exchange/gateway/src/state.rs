use {
    dango_math::Uint128,
    dango_primitives::{Addr, Denom},
    dango_storage::{Counter, Item, Map},
    dango_types::gateway::{PersonalQuota, Remote, WithdrawalRequest},
};

pub const ROUTES: Map<(Addr, Remote), Denom> = Map::new("route");

pub const REVERSE_ROUTES: Map<(&Denom, Remote), Addr> = Map::new("reverse_route");

pub const WITHDRAWAL_FEES: Map<(&Denom, Remote), Uint128> = Map::new("withdrawal_fee");

pub const RESERVES: Map<(Addr, Remote), Uint128> = Map::new("reserve");

pub const PERSONAL_QUOTAS: Map<(Addr, &Denom), PersonalQuota> = Map::new("personal_quota");

/// The whitelisted address that responds to withdrawal requests. If unset,
/// only the chain owner can respond.
pub const WITHDRAWAL_GUARDIAN: Item<Addr> = Item::new("withdrawal_guardian");

pub const NEXT_WITHDRAWAL_REQUEST_ID: Counter<u64> = Counter::new("withdrawal_request_id", 0, 1);

/// Pending withdrawal requests, keyed by ID — the guardian's work queue.
/// A frozen request moves to `FROZEN_WITHDRAWAL_REQUESTS`; a request that
/// reaches a terminal response (approved, rejected, or confiscated) is
/// deleted.
pub const WITHDRAWAL_REQUESTS: Map<u64, WithdrawalRequest> = Map::new("withdrawal_request");

/// Frozen withdrawal requests, keyed by ID — the owner's work queue. Kept
/// in a separate map so that the guardian, polling `WITHDRAWAL_REQUESTS`,
/// doesn't re-read requests it has already flagged.
pub const FROZEN_WITHDRAWAL_REQUESTS: Map<u64, WithdrawalRequest> =
    Map::new("frozen_withdrawal_request");
