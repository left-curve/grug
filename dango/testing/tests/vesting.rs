use {
    dango_math::{MultiplyFraction, Udec128, Uint128},
    dango_primitives::{
        Addr, Addressable, Coin, Coins, Duration, Inner, QuerierExt, ResultExt, StdError, Timestamp,
    },
    dango_testing::{TestAccounts, TestSuiteNaive, setup_test_naive},
    dango_types::{
        constants::{dango, usdc},
        vesting::{self, QueryPositionRequest, Schedule, VestingStatus},
    },
    std::sync::LazyLock,
};

static TEST_AMOUNT: LazyLock<Coin> = LazyLock::new(|| Coin {
    denom: dango::DENOM.clone(),
    amount: Uint128::new(100),
});

const ONE_MONTH: Duration = Duration::from_weeks(4);
const ONE_DAY: Duration = Duration::from_days(1);

fn setup_test() -> (TestSuiteNaive, TestAccounts, Addr) {
    let (suite, accounts, _codes, contracts, _) = setup_test_naive(Default::default());

    (suite, accounts, contracts.vesting)
}

#[tokio::test]
async fn missing_funds() {
    let (mut suite, mut accounts, vesting_addr) = setup_test();

    suite
        .execute(
            &mut accounts.owner,
            vesting_addr,
            &vesting::ExecuteMsg::Create {
                user: accounts.user1.address(),
                schedule: Schedule {
                    start_time: Duration::from_seconds(0),
                    cliff: Duration::from_seconds(0),
                    period: Duration::from_seconds(0),
                },
            },
            Coins::default(),
        )
        .await
        .should_fail_with_error("invalid payment: expecting 1, found 0");
}

#[tokio::test]
async fn non_owner_creating_position() {
    let (mut suite, mut accounts, vesting_addr) = setup_test();

    suite
        .execute(
            &mut accounts.user1,
            vesting_addr,
            &vesting::ExecuteMsg::Create {
                user: accounts.owner.address(),
                schedule: Schedule {
                    start_time: Duration::from_seconds(0),
                    cliff: Duration::from_seconds(0),
                    period: Duration::from_seconds(0),
                },
            },
            Coins::one(dango::DENOM.clone(), 100).unwrap(),
        )
        .await
        .should_fail_with_error("you don't have the right");
}

#[tokio::test]
async fn not_dango_token() {
    let (mut suite, mut accounts, vesting_addr) = setup_test();

    suite
        .execute(
            &mut accounts.owner,
            vesting_addr,
            &vesting::ExecuteMsg::Create {
                user: accounts.user1.address(),
                schedule: Schedule {
                    start_time: Duration::from_seconds(0),
                    cliff: Duration::from_seconds(0),
                    period: Duration::from_seconds(0),
                },
            },
            Coins::one(usdc::DENOM.clone(), 100).unwrap(),
        )
        .await
        .should_fail_with_error(StdError::invalid_payment(
            dango::DENOM.clone(),
            usdc::DENOM.clone(),
        ));
}

#[tokio::test]
async fn before_unlocking_starting_time() {
    let (mut suite, mut accounts, vesting_addr) = setup_test();

    suite
        .execute(
            &mut accounts.owner,
            vesting_addr,
            &vesting::ExecuteMsg::Create {
                user: accounts.user1.address(),
                schedule: Schedule {
                    start_time: suite.block.timestamp - ONE_MONTH,
                    cliff: ONE_MONTH * 9,
                    period: ONE_MONTH * 27,
                },
            },
            TEST_AMOUNT.clone(),
        )
        .await
        .should_succeed();

    let initial_balance = suite
        .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
        .should_succeed();

    // Go 1 day before cliff ends
    {
        suite.block_time = ONE_MONTH * 9 - ONE_DAY;

        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_fail_with_error("nothing to claim");
    }

    // Go at the end of the cliff. Claim should be possible
    {
        suite.block_time = ONE_DAY;

        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_succeed();

        suite
            .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
            .should_succeed_and_equal(
                initial_balance
                    + TEST_AMOUNT
                        .amount
                        .checked_mul_dec_floor(Udec128::checked_from_ratio(1, 3).unwrap())
                        .unwrap(),
            );
    }

    // Go at 66.66% of the vesting period
    {
        suite.block_time = ONE_MONTH * 9;

        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_succeed();

        suite
            .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
            .should_succeed_and_equal(
                initial_balance
                    + TEST_AMOUNT
                        .amount
                        .checked_mul_dec_floor(Udec128::checked_from_ratio(2, 3).unwrap())
                        .unwrap(),
            );
    }

    // Go at the end of the vesting period
    {
        suite.block_time = ONE_MONTH * 9;

        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_succeed();

        suite
            .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
            .should_succeed_and_equal(initial_balance + TEST_AMOUNT.amount);

        // Check if the position is updated
        suite
            .query_wasm_smart(
                vesting_addr,
                vesting::QueryPositionRequest {
                    user: accounts.user1.address(),
                },
            )
            .should_succeed_and(|res| res.position.claimed == res.position.total);
    }
}

#[tokio::test]
async fn after_unlocking_starting_time() {
    let (mut suite, mut accounts, vesting_addr) = setup_test();

    suite
        .execute(
            &mut accounts.owner,
            vesting_addr,
            &vesting::ExecuteMsg::Create {
                user: accounts.user1.address(),
                schedule: Schedule {
                    start_time: suite.block.timestamp + ONE_MONTH,
                    cliff: ONE_MONTH * 9,
                    period: ONE_MONTH * 27,
                },
            },
            TEST_AMOUNT.clone(),
        )
        .await
        .should_succeed();

    let initial_balance = suite
        .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
        .should_succeed();

    // Go 1 day before cliff ends
    {
        suite.block_time = ONE_MONTH * 9 - ONE_DAY;

        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_fail_with_error("nothing to claim");
    }

    // Go at the end of the cliff. Claim should not possible (1 month missing)
    {
        suite.block_time = ONE_DAY;

        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_fail_with_error("nothing to claim");
    }

    // Go at 1 month after unlocking cliff ends
    // This match with the finish of the vesting cliff
    {
        suite.block_time = ONE_MONTH;

        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_succeed();

        suite
            .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
            .should_succeed_and_equal(
                initial_balance
                    + TEST_AMOUNT
                        .amount
                        .checked_mul_dec_floor(Udec128::checked_from_ratio(1, 3).unwrap())
                        .unwrap(),
            );
    }

    // Go at 66.66% of the vesting period
    {
        suite.block_time = ONE_MONTH * 9;

        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_succeed();

        suite
            .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
            .should_succeed_and_equal(
                initial_balance
                    + TEST_AMOUNT
                        .amount
                        .checked_mul_dec_floor(Udec128::checked_from_ratio(2, 3).unwrap())
                        .unwrap(),
            );
    }

    // Go at the end of the vesting period
    {
        suite.block_time = ONE_MONTH * 9;

        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_succeed();

        suite
            .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
            .should_succeed_and_equal(initial_balance + TEST_AMOUNT.amount);

        // Check if the position is updated
        suite
            .query_wasm_smart(
                vesting_addr,
                vesting::QueryPositionRequest {
                    user: accounts.user1.address(),
                },
            )
            .should_succeed_and(|res| res.position.claimed == res.position.total);
    }
}

#[tokio::test]
async fn terminate_before_unlocking_starting_time_never_claimed() {
    let (mut suite, mut accounts, vesting_addr) = setup_test();

    suite
        .execute(
            &mut accounts.owner,
            vesting_addr,
            &vesting::ExecuteMsg::Create {
                user: accounts.user1.address(),
                schedule: Schedule {
                    start_time: suite.block.timestamp - ONE_MONTH,
                    cliff: ONE_MONTH * 9,
                    period: ONE_MONTH * 27,
                },
            },
            TEST_AMOUNT.clone(),
        )
        .await
        .should_succeed();

    let epoch = epoch(ONE_MONTH * 27, TEST_AMOUNT.amount);

    let initial_balance = suite
        .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
        .should_succeed();

    // Go 1 month after unlocking cliff finish.
    // Terminate user position.
    // In this situation, the vested amount so far of the user is
    // 1 + 9 + 1 / 27 * 100 = 11 / 27 * 100 = 40
    // The unlocked amount is
    // 9 + 1 / 27 * 100 = 10 / 27 * 100 = 37
    {
        suite.block_time = ONE_MONTH * 10;

        suite
            .execute(
                &mut accounts.owner,
                vesting_addr,
                &vesting::ExecuteMsg::Terminate {
                    user: accounts.user1.address(),
                },
                Coins::default(),
            )
            .await
            .should_succeed();

        // Check the status of the position after terminate
        suite
            .query_wasm_smart(
                vesting_addr,
                QueryPositionRequest {
                    user: accounts.user1.address(),
                },
            )
            .should_succeed_and(|res| {
                res.position.vesting_status == VestingStatus::Terminated(Uint128::new(40))
                    && res.claimable == Uint128::new(37)
            });

        suite.block_time = Timestamp::default();

        // Claim
        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_succeed();

        // Check the balance of the user
        suite
            .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
            .should_succeed_and_equal(initial_balance + Uint128::new(37));

        // Go forward 3 epoch to claim all tokens
        suite.block_time = epoch * 3;

        // Claim
        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_succeed();

        // Check if the position is removed
        suite
            .query_wasm_smart(
                vesting_addr,
                vesting::QueryPositionRequest {
                    user: accounts.user1.address(),
                },
            )
            .should_succeed_and(|res| {
                res.position.vesting_status == VestingStatus::Terminated(Uint128::new(40))
                    && res.position.claimed == Uint128::new(40)
            });

        // Check the balance of the user
        suite
            .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
            .should_succeed_and_equal(initial_balance + Uint128::new(40));
    }
}

#[tokio::test]
async fn terminate_before_unlocking_starting_time_with_claimed() {
    let (mut suite, mut accounts, vesting_addr) = setup_test();

    suite
        .execute(
            &mut accounts.owner,
            vesting_addr,
            &vesting::ExecuteMsg::Create {
                user: accounts.user1.address(),
                schedule: Schedule {
                    start_time: suite.block.timestamp - ONE_MONTH,
                    cliff: ONE_MONTH * 9,
                    period: ONE_MONTH * 27,
                },
            },
            TEST_AMOUNT.clone(),
        )
        .await
        .should_succeed();

    let epoch = epoch(ONE_MONTH * 27, TEST_AMOUNT.amount);

    let initial_balance = suite
        .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
        .should_succeed();

    // Go 1 month after unlocking cliff finish.
    // The user claim the tokens
    // In this situation, the vested amount so far of the user is
    // 1 + 9 + 1 / 27 * 100 = 11 / 27 * 100 = 40
    // The unlocked amount is
    // 9 + 1 / 27 * 100 = 10 / 27 * 100 = 37
    {
        suite.block_time = ONE_MONTH * 10;

        // Claim
        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_succeed();

        // Check the balance of the user
        suite
            .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
            .should_succeed_and_equal(initial_balance + Uint128::new(37));
    }

    // Go 1 month after
    // Terminate user position.
    // In this situation, the vested amount so far of the user is
    // 1 + 9 + 2 / 27 * 100 = 12 / 27 * 100 = 44
    // The unlocked amount is
    // 9 + 2 / 27 * 100 = 11 / 27 * 100 = 40
    {
        suite.block_time = ONE_MONTH;

        suite
            .execute(
                &mut accounts.owner,
                vesting_addr,
                &vesting::ExecuteMsg::Terminate {
                    user: accounts.user1.address(),
                },
                Coins::default(),
            )
            .await
            .should_succeed();

        // Check the status of the position after terminate
        suite
            .query_wasm_smart(
                vesting_addr,
                QueryPositionRequest {
                    user: accounts.user1.address(),
                },
            )
            .should_succeed_and(|res| {
                res.position.vesting_status == VestingStatus::Terminated(Uint128::new(44))
                    && res.position.claimed == Uint128::new(37)
                    && res.claimable == Uint128::new(3)
            });

        // 4 epoch is needed to claim all tokens
        // Instead wait for 8 epoch to check if there are any problems waiting more than needed
        suite.block_time = epoch * 8;

        // Claim
        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_succeed();

        // Check if the position is removed
        suite
            .query_wasm_smart(
                vesting_addr,
                vesting::QueryPositionRequest {
                    user: accounts.user1.address(),
                },
            )
            .should_succeed_and(|res| {
                res.position.vesting_status == VestingStatus::Terminated(Uint128::new(44))
                    && res.position.claimed == Uint128::new(44)
            });

        // Check the balance of the user
        suite
            .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
            .should_succeed_and_equal(initial_balance + Uint128::new(44));
    }
}

#[tokio::test]
async fn terminate_after_unlocking_starting_time() {
    let (mut suite, mut accounts, vesting_addr) = setup_test();

    suite
        .execute(
            &mut accounts.owner,
            vesting_addr,
            &vesting::ExecuteMsg::Create {
                user: accounts.user1.address(),
                schedule: Schedule {
                    start_time: suite.block.timestamp + ONE_MONTH,
                    cliff: ONE_MONTH * 9,
                    period: ONE_MONTH * 27,
                },
            },
            TEST_AMOUNT.clone(),
        )
        .await
        .should_succeed();

    let initial_balance = suite
        .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
        .should_succeed();

    // Go 2 month after unlocking cliff finish.
    // Terminate user position.
    // In this situation, the vested amount so far of the user is
    // -1 + 9 + 2 / 27 * 100 = 9 / 27 * 100 = 37
    // The unlocked amount is
    // 9 + 2 / 27 * 100 = 11 / 27 * 100 = 40
    {
        suite.block_time = ONE_MONTH * 11;

        suite
            .execute(
                &mut accounts.owner,
                vesting_addr,
                &vesting::ExecuteMsg::Terminate {
                    user: accounts.user1.address(),
                },
                Coins::default(),
            )
            .await
            .should_succeed();

        // Check the status of the position after terminate
        suite
            .query_wasm_smart(
                vesting_addr,
                QueryPositionRequest {
                    user: accounts.user1.address(),
                },
            )
            .should_succeed_and(|res| {
                res.position.vesting_status == VestingStatus::Terminated(Uint128::new(37))
                    && res.claimable == Uint128::new(37)
            });

        suite.block_time = Timestamp::default();

        // Claim
        suite
            .execute(
                &mut accounts.user1,
                vesting_addr,
                &vesting::ExecuteMsg::Claim {},
                Coins::default(),
            )
            .await
            .should_succeed();

        // Check if the position is removed
        suite
            .query_wasm_smart(
                vesting_addr,
                vesting::QueryPositionRequest {
                    user: accounts.user1.address(),
                },
            )
            .should_succeed_and(|res| {
                res.position.vesting_status == VestingStatus::Terminated(Uint128::new(37))
                    && res.position.claimed == Uint128::new(37)
            });

        // Check the balance of the user
        suite
            .query_balance(&accounts.user1, TEST_AMOUNT.denom.clone())
            .should_succeed_and_equal(initial_balance + Uint128::new(37));
    }
}

// Duration for unlock 1 token
fn epoch(total_duration: Duration, vesting_amount: Uint128) -> Duration {
    Duration::from_nanos(total_duration.into_nanos() / vesting_amount.into_inner())
}
