use {
    crate::state::{STATE, USER_STATES},
    anyhow::ensure,
    dango_order_book::UsdValue,
    dango_primitives::{MutableCtx, QuerierExt, Response},
};

/// Withdraw the entire accumulated protocol-fee treasury into the chain owner's
/// margin account, then zero the treasury.
///
/// Only callable by the chain owner. The treasury must be positive.
///
/// Mutates: `STATE` (treasury zeroed), `USER_STATES` (owner margin increased).
pub fn withdraw_treasury(ctx: MutableCtx) -> anyhow::Result<Response> {
    // 1. Query and load data.
    let owner = ctx.querier.query_owner()?;
    let mut state = STATE.load(ctx.storage)?;
    let mut user_state = USER_STATES
        .may_load(ctx.storage, owner)?
        .unwrap_or_default();

    // 2. Checks:
    //    - Only the chain owner may withdraw the treasury.
    //    - The treasury must be positive to withdraw.
    ensure!(
        ctx.sender == owner,
        "only the chain owner can withdraw the treasury"
    );

    ensure!(state.treasury.is_positive(), "treasury is empty");

    // 3. Credit the treasury balance to the owner's margin.
    // `amount` is positive, so the resulting `UserState` can never be empty.
    let amount = state.treasury;
    state.treasury = UsdValue::ZERO;
    user_state.margin.checked_add_assign(amount)?;

    // 4. Persist state changes.
    STATE.save(ctx.storage, &state)?;
    USER_STATES.save(ctx.storage, owner, &user_state)?;

    Ok(Response::new())
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_primitives::{
            Addr, Coins, Config, Duration, MockContext, MockQuerier, Permission, Permissions,
            ResultExt, Storage,
        },
        dango_types::perps::{State, UserState},
        std::collections::BTreeMap,
    };

    const OWNER: Addr = Addr::mock(0);
    const NON_OWNER: Addr = Addr::mock(1);

    fn mock_config() -> Config {
        Config {
            owner: OWNER,
            bank: Addr::mock(2),
            gas_token: dango_primitives::Denom::new_unchecked(["ugas"]),
            gas_fee_rate: Default::default(),
            gas_exemptions: Default::default(),
            cronjobs: BTreeMap::new(),
            permissions: Permissions {
                upload: Permission::Nobody,
                instantiate: Permission::Nobody,
            },
            max_orphan_age: Duration::from_seconds(0),
        }
    }

    /// Seed `STATE` with the given treasury balance (all other fields default).
    fn seed_treasury(storage: &mut dyn Storage, treasury: UsdValue) {
        STATE
            .save(
                storage,
                &State {
                    treasury,
                    ..Default::default()
                },
            )
            .unwrap();
    }

    // --------------------------- access control ---------------------------

    #[test]
    fn non_owner_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(NON_OWNER)
            .with_funds(Coins::default());
        seed_treasury(&mut ctx.storage, UsdValue::new_int(1_000));

        withdraw_treasury(ctx.as_mutable())
            .should_fail_with_error("only the chain owner can withdraw the treasury");

        // Access control must prevent any state change.
        assert_eq!(
            STATE.load(&ctx.storage).unwrap().treasury,
            UsdValue::new_int(1_000)
        );
        assert!(USER_STATES.may_load(&ctx.storage, OWNER).unwrap().is_none());
        assert!(
            USER_STATES
                .may_load(&ctx.storage, NON_OWNER)
                .unwrap()
                .is_none()
        );
    }

    // ------------------------------ happy paths ------------------------------

    #[test]
    fn owner_withdraws_to_fresh_margin() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        seed_treasury(&mut ctx.storage, UsdValue::new_int(1_000));

        let resp = withdraw_treasury(ctx.as_mutable()).should_succeed();

        // No tokens move on-chain; the treasury only shifts into internal margin.
        assert!(resp.submsgs.is_empty());
        assert_eq!(STATE.load(&ctx.storage).unwrap().treasury, UsdValue::ZERO);
        assert_eq!(
            USER_STATES.load(&ctx.storage, OWNER).unwrap().margin,
            UsdValue::new_int(1_000)
        );
    }

    #[test]
    fn owner_withdraws_onto_existing_margin() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        seed_treasury(&mut ctx.storage, UsdValue::new_int(1_000));
        USER_STATES
            .save(
                &mut ctx.storage,
                OWNER,
                &UserState {
                    margin: UsdValue::new_int(500),
                    reserved_margin: UsdValue::new_int(100),
                    open_order_count: 2,
                    ..Default::default()
                },
            )
            .unwrap();

        withdraw_treasury(ctx.as_mutable()).should_succeed();

        let user_state = USER_STATES.load(&ctx.storage, OWNER).unwrap();
        assert_eq!(user_state.margin, UsdValue::new_int(1_500));
        assert_eq!(STATE.load(&ctx.storage).unwrap().treasury, UsdValue::ZERO);
        // Fields other than `margin` must be left untouched.
        assert_eq!(user_state.reserved_margin, UsdValue::new_int(100));
        assert_eq!(user_state.open_order_count, 2);
    }

    // ----------------------------- rejections ------------------------------

    #[test]
    fn zero_treasury_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        seed_treasury(&mut ctx.storage, UsdValue::ZERO);

        withdraw_treasury(ctx.as_mutable()).should_fail_with_error("treasury is empty");

        assert_eq!(STATE.load(&ctx.storage).unwrap().treasury, UsdValue::ZERO);
        assert!(USER_STATES.may_load(&ctx.storage, OWNER).unwrap().is_none());
    }

    #[test]
    fn negative_treasury_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        seed_treasury(&mut ctx.storage, UsdValue::new_int(-1));

        withdraw_treasury(ctx.as_mutable()).should_fail_with_error("treasury is empty");

        // State is left untouched.
        assert_eq!(
            STATE.load(&ctx.storage).unwrap().treasury,
            UsdValue::new_int(-1)
        );
        assert!(USER_STATES.may_load(&ctx.storage, OWNER).unwrap().is_none());
    }

    #[test]
    fn idempotent_second_call_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        seed_treasury(&mut ctx.storage, UsdValue::new_int(1_000));

        // First withdrawal drains the treasury.
        withdraw_treasury(ctx.as_mutable()).should_succeed();
        assert_eq!(STATE.load(&ctx.storage).unwrap().treasury, UsdValue::ZERO);

        // Second withdrawal finds nothing left.
        withdraw_treasury(ctx.as_mutable()).should_fail_with_error("treasury is empty");

        // Margin reflects only the single successful withdrawal.
        assert_eq!(
            USER_STATES.load(&ctx.storage, OWNER).unwrap().margin,
            UsdValue::new_int(1_000)
        );
    }
}
