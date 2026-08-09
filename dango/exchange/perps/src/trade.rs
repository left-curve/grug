mod batch_update_orders;
mod cancel_conditional_order;
mod cancel_order;
mod deposit;
mod resize_reduce_only;
mod submit_conditional_order;
mod submit_order;
mod withdraw;

pub use {
    batch_update_orders::*, cancel_conditional_order::*, cancel_order::*, deposit::*,
    resize_reduce_only::*, submit_conditional_order::*, submit_order::*, withdraw::*,
};

use {
    crate::USER_STATES,
    anyhow::ensure,
    dango_primitives::{Addr, StdResult, Storage},
    dango_types::perps::{Param, UserState},
};

/// Trading must be enabled.
///
/// Guards the actions that would open new risk on the exchange: placing an
/// order, moving funds from a spot account into margin, and moving margin into
/// the counterparty vault. Actions that reduce risk or return funds to the
/// user are deliberately not guarded.
pub fn ensure_trading_enabled(param: &Param) -> anyhow::Result<()> {
    ensure!(
        param.trading_enabled,
        "trading is disabled; the exchange has been wound down"
    );

    Ok(())
}

/// 1. Load the user's state.
/// 2. Perform a mutable action on the user state. The action may have side
///    effect on the storage.
/// 3. If the user state becomes empty, delete it from storage; otherwise, save
///    the updated user state to storage.
fn update_user_state_with<F, T>(storage: &mut dyn Storage, user: Addr, action: F) -> StdResult<T>
where
    F: FnOnce(&mut dyn Storage, &mut UserState) -> StdResult<T>,
{
    let mut user_state = USER_STATES.load(storage, user)?;

    let result = action(storage, &mut user_state)?;

    if user_state.is_empty() {
        USER_STATES.remove(storage, user)?;
    } else {
        USER_STATES.save(storage, user, &user_state)?;
    }

    Ok(result)
}
