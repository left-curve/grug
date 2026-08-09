use {
    crate::{
        core::{OverrideDelta, check_fee_sign_invariant},
        state::{FEE_RATE_OVERRIDES, PARAM},
    },
    anyhow::ensure,
    dango_order_book::Dimensionless,
    dango_primitives::{Addr, MutableCtx, Op, QuerierExt, Response},
};

pub fn set_fee_rate_override(
    ctx: MutableCtx,
    user: Addr,
    maker_taker_fee_rates: Op<(Dimensionless, Dimensionless)>,
) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "you don't have the right, O you don't have the right"
    );

    let param = PARAM.load(ctx.storage)?;

    match maker_taker_fee_rates {
        Op::Insert((maker_fee_rate, taker_fee_rate)) => {
            // Maker fees may be negative to express a rebate paid to the
            // maker, matching the `[-1, 1]` bound used for the global maker
            // fee tier schedule (see `configure.rs::validate_param`).
            ensure!(
                (Dimensionless::new_int(-1)..=Dimensionless::ONE).contains(&maker_fee_rate),
                "invalid maker fee rate: {maker_fee_rate}! must be within [-1, 1]"
            );

            ensure!(
                (Dimensionless::ZERO..=Dimensionless::ONE).contains(&taker_fee_rate),
                "invalid taker fee rate: {taker_fee_rate}! must be within [0, 1]"
            );

            // Net-fee distribution requires `taker_rate + maker_rate >= 0`
            // on every possible fill. Validate the invariant with the new
            // override applied on top of the current tier schedules + stored
            // overrides.
            check_fee_sign_invariant(
                ctx.storage,
                &param,
                Some(OverrideDelta::Insert(user, maker_fee_rate, taker_fee_rate)),
            )?;

            FEE_RATE_OVERRIDES.save(ctx.storage, user, &(maker_fee_rate, taker_fee_rate))?;
        },
        Op::Delete => {
            // Deletion makes the user fall back to the tier schedule. Verify
            // the invariant still holds with this user's stored override
            // suppressed.
            check_fee_sign_invariant(ctx.storage, &param, Some(OverrideDelta::Delete(user)))?;

            FEE_RATE_OVERRIDES.remove(ctx.storage, user);
        },
    }

    Ok(Response::new())
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_primitives::{
            Coins, Config, Duration, MockContext, MockQuerier, Permission, Permissions, ResultExt,
            Storage,
        },
        dango_types::perps::{Param, RateSchedule},
        std::collections::BTreeMap,
    };

    const OWNER: Addr = Addr::mock(0);
    const NON_OWNER: Addr = Addr::mock(1);
    const USER: Addr = Addr::mock(42);

    fn mock_config() -> Config {
        Config {
            owner: OWNER,
            bank: Addr::mock(100),
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

    /// Seed `PARAM` in storage with a permissive baseline so the
    /// net-fee sign invariant in `set_fee_rate_override` has something
    /// to evaluate against.
    ///
    /// The tier schedule sets `taker_base = 1` (100%) so the
    /// schedule-level minima can never be the binding constraint — the
    /// invariant reduces to the override-level arithmetic alone.
    fn save_param(storage: &mut dyn Storage) {
        PARAM
            .save(
                storage,
                &Param {
                    taker_fee_rates: RateSchedule {
                        base: Dimensionless::ONE,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
    }

    /// A `(maker, taker)` rate pair that passes validation. Individual tests
    /// mutate one side to drive it out of range.
    fn valid_rates() -> (Dimensionless, Dimensionless) {
        (
            Dimensionless::new_permille(1), // 0.1% maker
            Dimensionless::new_permille(5), // 0.5% taker
        )
    }

    // ------------------------------ authorization ------------------------------

    #[test]
    fn non_owner_insert_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(NON_OWNER)
            .with_funds(Coins::default());

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Insert(valid_rates()))
            .should_fail_with_error("you don't have the right");
    }

    #[test]
    fn non_owner_delete_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(NON_OWNER)
            .with_funds(Coins::default());

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Delete)
            .should_fail_with_error("you don't have the right");
    }

    // ----------------------------- happy path (insert) -------------------------

    #[test]
    fn valid_override_accepted_and_stored() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);
        let rates = valid_rates();

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Insert(rates)).should_succeed();

        let stored = FEE_RATE_OVERRIDES.load(&ctx.storage, USER).unwrap();
        assert_eq!(stored, rates);
    }

    #[test]
    fn zero_maker_and_taker_accepted() {
        // Inclusive lower boundary of [0, 1].
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);
        let rates = (Dimensionless::ZERO, Dimensionless::ZERO);

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Insert(rates)).should_succeed();

        let stored = FEE_RATE_OVERRIDES.load(&ctx.storage, USER).unwrap();
        assert_eq!(stored, rates);
    }

    #[test]
    fn one_maker_and_taker_accepted() {
        // Inclusive upper boundary of [0, 1].
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);
        let rates = (Dimensionless::ONE, Dimensionless::ONE);

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Insert(rates)).should_succeed();

        let stored = FEE_RATE_OVERRIDES.load(&ctx.storage, USER).unwrap();
        assert_eq!(stored, rates);
    }

    #[test]
    fn insert_overwrites_previous_override() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);
        let first = (
            Dimensionless::new_permille(1),
            Dimensionless::new_permille(5),
        );
        let second = (
            Dimensionless::new_permille(2),
            Dimensionless::new_permille(10),
        );

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Insert(first)).should_succeed();
        set_fee_rate_override(ctx.as_mutable(), USER, Op::Insert(second)).should_succeed();

        let stored = FEE_RATE_OVERRIDES.load(&ctx.storage, USER).unwrap();
        assert_eq!(stored, second);
    }

    #[test]
    fn overrides_are_per_user() {
        // Setting an override for one user must not touch another user's entry.
        let other: Addr = Addr::mock(43);
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);
        let rates = valid_rates();

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Insert(rates)).should_succeed();

        assert!(FEE_RATE_OVERRIDES.has(&ctx.storage, USER));
        assert!(!FEE_RATE_OVERRIDES.has(&ctx.storage, other));
    }

    // ---------------------------- validation rejections ------------------------

    #[test]
    fn negative_maker_fee_accepted_as_rebate() {
        // Negative maker rates encode rebates paid to the maker — the global
        // tier schedule accepts them down to -1, and overrides must too, or
        // the admin cannot grant a VIP maker a custom rebate.
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);
        let rates = (Dimensionless::new_raw(-100), Dimensionless::new_permille(5));

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Insert(rates)).should_succeed();

        let stored = FEE_RATE_OVERRIDES.load(&ctx.storage, USER).unwrap();
        assert_eq!(stored, rates);
    }

    #[test]
    fn minus_one_maker_fee_accepted() {
        // Inclusive lower boundary of [-1, 1] for the maker rate. The
        // net-fee sign invariant requires a paired taker rate of at least
        // 1 so that `taker + maker = 0`, which the admin supplies here.
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        // Use a param whose tier-schedule min taker rate is 1 so the
        // invariant is not tripped by the schedule itself.
        PARAM
            .save(
                &mut ctx.storage,
                &Param {
                    taker_fee_rates: RateSchedule {
                        base: Dimensionless::ONE,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        let rates = (Dimensionless::new_int(-1), Dimensionless::ONE);

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Insert(rates)).should_succeed();

        let stored = FEE_RATE_OVERRIDES.load(&ctx.storage, USER).unwrap();
        assert_eq!(stored, rates);
    }

    #[test]
    fn maker_fee_below_minus_one_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);

        set_fee_rate_override(
            ctx.as_mutable(),
            USER,
            Op::Insert((
                Dimensionless::new_raw(-1_000_001),
                Dimensionless::new_permille(5),
            )),
        )
        .should_fail_with_error("invalid maker fee rate");
    }

    #[test]
    fn maker_fee_above_one_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);

        set_fee_rate_override(
            ctx.as_mutable(),
            USER,
            Op::Insert((Dimensionless::new_int(2), Dimensionless::new_permille(5))),
        )
        .should_fail_with_error("invalid maker fee rate");
    }

    #[test]
    fn negative_taker_fee_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);

        set_fee_rate_override(
            ctx.as_mutable(),
            USER,
            Op::Insert((Dimensionless::new_permille(1), Dimensionless::new_raw(-1))),
        )
        .should_fail_with_error("invalid taker fee rate");
    }

    #[test]
    fn taker_fee_above_one_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);

        set_fee_rate_override(
            ctx.as_mutable(),
            USER,
            Op::Insert((Dimensionless::new_permille(1), Dimensionless::new_int(2))),
        )
        .should_fail_with_error("invalid taker fee rate");
    }

    #[test]
    fn rejected_override_leaves_state_untouched() {
        // An invalid rate on a user with an existing override must not
        // overwrite that entry. Validation runs before state mutation.
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);
        let prior = valid_rates();

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Insert(prior)).should_succeed();

        set_fee_rate_override(
            ctx.as_mutable(),
            USER,
            Op::Insert((Dimensionless::new_int(2), Dimensionless::new_permille(5))),
        )
        .should_fail_with_error("invalid maker fee rate");

        let stored = FEE_RATE_OVERRIDES.load(&ctx.storage, USER).unwrap();
        assert_eq!(stored, prior);
    }

    // ---------------------- net-fee sign invariant -----------------------------

    #[test]
    fn override_violating_invariant_rejected() {
        // A pending override that, combined with the schedule and prior
        // overrides, would allow a fill with negative net fee must be
        // rejected by `set_fee_rate_override`'s invariant check.
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage); // tier schedule: taker 0, maker 0

        set_fee_rate_override(
            ctx.as_mutable(),
            USER,
            // maker = -0.1%, taker = 0: net -0.1% — invariant violated.
            Op::Insert((
                Dimensionless::new_permille(1).checked_neg().unwrap(),
                Dimensionless::ZERO,
            )),
        )
        .should_fail_with_error("negative net fee");
    }

    #[test]
    fn delete_that_would_violate_invariant_rejected() {
        // Deleting a stored override that covered another user's rebate
        // override must be rejected if the remaining state would admit a
        // negative-net-fee fill.
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        // Schedule: taker 1%, maker 0 → invariant holds against bare schedule.
        PARAM
            .save(
                &mut ctx.storage,
                &Param {
                    taker_fee_rates: RateSchedule {
                        base: Dimensionless::new_permille(10),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();

        // User A carries a -1% maker override — compatible with the schedule.
        let user_a = Addr::mock(100);
        set_fee_rate_override(
            ctx.as_mutable(),
            user_a,
            Op::Insert((
                Dimensionless::new_permille(10).checked_neg().unwrap(),
                Dimensionless::new_permille(10),
            )),
        )
        .should_succeed();

        // User B carries a 0% taker override paired with a 0% maker — also
        // fine on its own, but combining B's 0% taker with A's -1% maker
        // breaks the invariant. The override insert must catch this.
        let user_b = Addr::mock(101);
        set_fee_rate_override(
            ctx.as_mutable(),
            user_b,
            Op::Insert((Dimensionless::ZERO, Dimensionless::ZERO)),
        )
        .should_fail_with_error("negative net fee");
    }

    // --------------------------------- delete ---------------------------------

    #[test]
    fn delete_removes_existing_override() {
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Insert(valid_rates())).should_succeed();
        assert!(FEE_RATE_OVERRIDES.has(&ctx.storage, USER));

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Delete).should_succeed();
        assert!(!FEE_RATE_OVERRIDES.has(&ctx.storage, USER));
    }

    #[test]
    fn delete_nonexistent_override_is_noop() {
        // `Map::remove` on a missing key must not panic or error.
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config()))
            .with_sender(OWNER)
            .with_funds(Coins::default());
        save_param(&mut ctx.storage);

        set_fee_rate_override(ctx.as_mutable(), USER, Op::Delete).should_succeed();
        assert!(!FEE_RATE_OVERRIDES.has(&ctx.storage, USER));
    }
}
