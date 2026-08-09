use {
    crate::state::USER_STATES,
    dango_order_book::UsdValue,
    dango_primitives::{EventBuilder, Order as IterationOrder, StdResult, Storage, Timestamp},
    dango_storage::PrefixBound,
    dango_types::perps::{LiquidityReleased, UserState},
};

/// Pop matured unlocks from each user and credit the released USD value back
/// to their trading margin.
pub fn process_unlocks(
    storage: &mut dyn Storage,
    current_time: Timestamp,
    events: &mut EventBuilder,
) -> anyhow::Result<()> {
    // Load all users whose earliest unlock has matured.
    let users = USER_STATES
        .idx
        .earliest_unlock_end_time
        .prefix_range(
            storage,
            None,
            Some(PrefixBound::Inclusive(current_time)),
            IterationOrder::Ascending,
        )
        .map(|res| {
            let (_timestamp, user, user_state) = res?;
            Ok((user, user_state))
        })
        .collect::<StdResult<Vec<_>>>()?;

    #[cfg(feature = "tracing")]
    let num_users = users.len();

    for (user, user_state) in users {
        let UnlockOutcome {
            user_state,
            amount_usd,
        } = process_unlock_for_user(current_time, &user_state)?;

        if amount_usd.is_positive() {
            events.push(LiquidityReleased {
                user,
                amount: amount_usd,
            })?;
        }

        if user_state.is_empty() {
            USER_STATES.remove(storage, user)?;
        } else {
            USER_STATES.save(storage, user, &user_state)?;
        }
    }

    #[cfg(feature = "tracing")]
    {
        tracing::info!(num_users, "Processed matured unlocks");
    }

    Ok(())
}

/// Owned outcome of a `process_unlock_for_user` call. Returns the
/// updated `user_state` (with matured unlocks popped and `margin`
/// credited) and the total USD value released (used to emit the
/// `LiquidityReleased` event and decide whether to delete the user
/// state at the caller site).
#[derive(Debug)]
pub struct UnlockOutcome {
    pub user_state: UserState,
    pub amount_usd: UsdValue,
}

/// Pure: takes `&UserState`, clones, pops matured unlocks, credits
/// margin, returns the updated copy in [`UnlockOutcome`]. Storage and
/// event emission happen at the caller site.
fn process_unlock_for_user(
    current_time: Timestamp,
    user_state: &UserState,
) -> anyhow::Result<UnlockOutcome> {
    let mut user_state = user_state.clone();
    let mut amount_usd = UsdValue::ZERO;

    // Loop through unlocks, pop the ones that have matured, sum up USD value
    // of all that have matured.
    while let Some(unlock) = user_state.unlocks.front() {
        if unlock.end_time > current_time {
            break;
        }

        amount_usd.checked_add_assign(unlock.amount_to_release)?;
        user_state.unlocks.pop_front();
    }

    // Credit the released USD value back to the user's trading margin.
    user_state.margin.checked_add_assign(amount_usd)?;

    Ok(UnlockOutcome {
        user_state,
        amount_usd,
    })
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_primitives::{Addr, MockStorage},
        dango_types::perps::Unlock,
        std::collections::VecDeque,
    };

    const USER_A: Addr = Addr::mock(1);
    const USER_B: Addr = Addr::mock(2);

    /// Build unlocks from `(usd_amount, end_time_seconds)` pairs.
    ///
    /// Mutates: nothing.
    /// Returns: a `VecDeque<Unlock>` for use in `UserState`.
    fn unlocks_from(entries: &[(i128, u128)]) -> VecDeque<Unlock> {
        entries
            .iter()
            .map(|&(amount, secs)| Unlock {
                amount_to_release: UsdValue::new_int(amount),
                end_time: Timestamp::from_seconds(secs),
            })
            .collect()
    }

    #[test]
    fn no_matured_unlocks_unchanged() {
        let mut storage = MockStorage::new();

        let user_state = UserState {
            unlocks: unlocks_from(&[(1000, 200), (2000, 300)]),
            ..Default::default()
        };
        USER_STATES.save(&mut storage, USER_A, &user_state).unwrap();

        process_unlocks(
            &mut storage,
            Timestamp::from_seconds(100),
            &mut EventBuilder::new(),
        )
        .unwrap();

        let loaded = USER_STATES.load(&storage, USER_A).unwrap();
        assert_eq!(loaded.unlocks.len(), 2);
        assert_eq!(loaded.margin, UsdValue::ZERO);
    }

    #[test]
    fn single_user_single_matured_unlock() {
        let mut storage = MockStorage::new();

        let user_state = UserState {
            unlocks: unlocks_from(&[(1000, 100)]),
            ..Default::default()
        };
        USER_STATES.save(&mut storage, USER_A, &user_state).unwrap();

        // At t=100 the unlock matures (end_time > current_time is false).
        process_unlocks(
            &mut storage,
            Timestamp::from_seconds(100),
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Margin credited, unlocks cleared. User state persists because margin > 0.
        let loaded = USER_STATES.load(&storage, USER_A).unwrap();
        assert_eq!(loaded.margin, UsdValue::new_int(1000));
        assert!(loaded.unlocks.is_empty());
    }

    #[test]
    fn single_user_partial_maturation() {
        let mut storage = MockStorage::new();

        let user_state = UserState {
            unlocks: unlocks_from(&[(1000, 100), (2000, 200), (3000, 300)]),
            ..Default::default()
        };
        USER_STATES.save(&mut storage, USER_A, &user_state).unwrap();

        // At t=200 the first two unlocks mature ($1000 + $2000 = $3000).
        process_unlocks(
            &mut storage,
            Timestamp::from_seconds(200),
            &mut EventBuilder::new(),
        )
        .unwrap();

        let loaded = USER_STATES.load(&storage, USER_A).unwrap();
        assert_eq!(loaded.margin, UsdValue::new_int(3000));
        assert_eq!(loaded.unlocks.len(), 1);
        assert_eq!(loaded.unlocks[0].amount_to_release, UsdValue::new_int(3000));
    }

    #[test]
    fn multiple_users_margin_credited() {
        let mut storage = MockStorage::new();

        USER_STATES
            .save(
                &mut storage,
                USER_A,
                &UserState {
                    unlocks: unlocks_from(&[(500, 50)]),
                    ..Default::default()
                },
            )
            .unwrap();
        USER_STATES
            .save(
                &mut storage,
                USER_B,
                &UserState {
                    unlocks: unlocks_from(&[(700, 60)]),
                    ..Default::default()
                },
            )
            .unwrap();

        process_unlocks(
            &mut storage,
            Timestamp::from_seconds(100),
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Both users get margin credited.
        let loaded_a = USER_STATES.load(&storage, USER_A).unwrap();
        assert_eq!(loaded_a.margin, UsdValue::new_int(500));
        assert!(loaded_a.unlocks.is_empty());

        let loaded_b = USER_STATES.load(&storage, USER_B).unwrap();
        assert_eq!(loaded_b.margin, UsdValue::new_int(700));
        assert!(loaded_b.unlocks.is_empty());
    }

    #[test]
    fn user_with_margin_preserved_after_unlock() {
        let mut storage = MockStorage::new();

        // User has unlocks AND nonzero margin.
        let user_state = UserState {
            margin: UsdValue::new_int(500),
            unlocks: unlocks_from(&[(1000, 100)]),
            ..Default::default()
        };
        USER_STATES.save(&mut storage, USER_A, &user_state).unwrap();

        process_unlocks(
            &mut storage,
            Timestamp::from_seconds(200),
            &mut EventBuilder::new(),
        )
        .unwrap();

        // User state persists, margin = original $500 + released $1000 = $1500.
        let loaded = USER_STATES.load(&storage, USER_A).unwrap();
        assert_eq!(loaded.margin, UsdValue::new_int(1500));
        assert!(loaded.unlocks.is_empty());
    }

    #[test]
    fn no_users_no_error() {
        let mut storage = MockStorage::new();

        process_unlocks(
            &mut storage,
            Timestamp::from_seconds(100),
            &mut EventBuilder::new(),
        )
        .unwrap();
    }
}
