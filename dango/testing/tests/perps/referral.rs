use {
    crate::{default_pair_param, default_param},
    dango_math::Uint128,
    dango_order_book::{Dimensionless, OrderKind, Quantity, TimeInForce, UsdPrice, UsdValue},
    dango_primitives::{
        Addr, Addressable, CheckedContractEvent, Coins, Duration, HashExt, JsonDeExt, Op,
        Order as IterationOrder, QuerierExt, ResultExt, SearchEvent, Signer, Timestamp, TxEvents,
        TxOutcome, btree_map,
    },
    dango_testing::{
        Factory, OracleTestEntry, Preset, TestAccount, TestOption, TestSuiteNaive, pair_id,
        setup_test_naive,
    },
    dango_types::{
        account_factory::{self, RegisterUserData},
        constants::usdc,
        perps::{
            self, CommissionRate, FeeDistributed, FeeShareRatio, QueryParamRequest, RateSchedule,
            Referee, ReferrerSettings, ReferrerStatsOrderBy, ReferrerStatsOrderIndex,
            UserReferralData,
        },
    },
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Register a referral relationship during user registration via the account
/// factory (the `referrer` field on `RegisterUser`).
#[tokio::test]
async fn referral_during_user_register() {
    let (mut suite, mut accounts, codes, contracts, ..) =
        setup_test_naive(TestOption::preset_test());

    suite.make_empty_block().await;

    // User1 (index 1) sets a fee share ratio so they can be a referrer.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(50),
    )
    .await
    .should_succeed();

    let chain_id = suite.chain_id.clone();

    // Create a new user and predict its address.
    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        3,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    // Register the new user with User1 as referrer.
    suite
        .execute(
            &mut Factory::new(contracts.account_factory),
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterUser {
                key: user.first_key(),
                key_hash: user.first_key_hash(),
                seed: 3,
                signature: user
                    .sign_arbitrary(RegisterUserData {
                        chain_id: chain_id.clone(),
                        key: user.first_key(),
                        key_hash: user.first_key_hash(),
                        seed: 3,
                        referrer: Some(1),
                    })
                    .unwrap(),
                referrer: Some(1),
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // Look up the new user's index.
    let user_index = suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryAccountRequest {
                address: user.address(),
            },
        )
        .should_succeed()
        .owner;

    // The new user's referrer should be User1 (index 1).
    assert_eq!(
        suite
            .query_wasm_smart(
                contracts.perps,
                perps::QueryReferrerRequest {
                    referee: user_index,
                },
            )
            .should_succeed(),
        Some(1),
    );
}

/// Set a referral relationship after the user has already registered, and
/// verify immutability + multiple referees.
#[tokio::test]
async fn referral_after_user_register() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // User1, User2, and User3 all set fee share ratios.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(50),
    )
    .await
    .should_succeed();

    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user2,
        Dimensionless::new_percent(50),
    )
    .await
    .should_succeed();

    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user3,
        Dimensionless::new_percent(50),
    )
    .await
    .should_succeed();

    // User2 sets User1 as referrer.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify User2's referrer is User1.
    assert_eq!(
        suite
            .query_wasm_smart(contracts.perps, perps::QueryReferrerRequest { referee: 2 },)
            .should_succeed(),
        Some(1),
    );

    // Trying to change User2's referrer should fail (immutable).
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 3,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_fail_with_error("referee 2 already has a referrer");

    // User3 also sets User1 as referrer.
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 3,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    assert_eq!(
        suite
            .query_wasm_smart(contracts.perps, perps::QueryReferrerRequest { referee: 3 },)
            .should_succeed(),
        Some(1),
    );

    // User4 sets User2 as referrer (chain: User1 <- User2 <- User4).
    suite
        .execute(
            &mut accounts.user4,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 2,
                referee: 4,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    assert_eq!(
        suite
            .query_wasm_smart(contracts.perps, perps::QueryReferrerRequest { referee: 4 },)
            .should_succeed(),
        Some(2),
    );
}

/// A user cannot refer themselves.
#[tokio::test]
async fn referral_self_refer_fails() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(50),
    )
    .await
    .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 1,
            }),
            Coins::new(),
        )
        .await
        .should_fail_with_error("a user cannot refer themselves");
}

/// A referral cannot be set if the referrer has no fee share ratio.
#[tokio::test]
async fn referral_without_share_ratio_fails() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // User1 has NOT set a share ratio. Trying to set User1 as referrer should fail.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_fail_with_error("referrer 1 has no fee share ratio set");
}

/// Only the referee (or the account factory) can set the referral relationship.
#[tokio::test]
async fn referral_wrong_caller_fails() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(50),
    )
    .await
    .should_succeed();

    // User3 tries to set the referral for User2 — should fail.
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_fail_with_error(
            "caller is not the account factory, chain owner, or an account owned by the referee",
        );
}

/// The fee share ratio can only increase, never decrease.
#[tokio::test]
async fn modify_share_ratio() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // Set initial share ratio to 20%.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(20),
    )
    .await
    .should_succeed();

    // Verify the stored ratio.
    assert_eq!(
        query_referral_settings(&suite, contracts.perps, 1)
            .unwrap()
            .share_ratio,
        Dimensionless::new_percent(20),
    );

    // Try to lower the share ratio — should fail.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(10),
    )
    .await
    .should_fail_with_error("fee share ratio can only increase");

    // Increase the share ratio — should succeed.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(50),
    )
    .await
    .should_succeed();

    assert_eq!(
        query_referral_settings(&suite, contracts.perps, 1)
            .unwrap()
            .share_ratio,
        Dimensionless::new_percent(50),
    );
}

/// Fee share ratio cannot exceed the maximum (50%).
#[tokio::test]
async fn share_ratio_exceeds_max_fails() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // 51% should fail.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(51),
    )
    .await
    .should_fail_with_error("fee share ratio cannot exceed");

    // 50% should succeed.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(50),
    )
    .await
    .should_succeed();
}

/// A negative fee share ratio must be rejected.
///
/// Without this guard a malicious referrer could set e.g. -50%, causing
/// `credit_commission(referee, negative)` on every trade — silently draining
/// the referee's margin while inflating the referrer's commission.
#[tokio::test]
async fn negative_share_ratio_fails() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // -1% should fail.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(-1),
    )
    .await
    .should_fail_with_error("fee share ratio cannot be negative");
}

/// Zero is a valid share ratio (referrer takes no commission from the referee).
#[tokio::test]
async fn zero_share_ratio_accepted() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::ZERO,
    )
    .await
    .should_succeed();
}

/// Setting the fee share ratio requires sufficient perps trading volume
/// when `volume_to_be_referrer` is non-zero.
#[tokio::test]
async fn set_share_ratio_requires_volume() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // Configure the perps contract to require $10,000 volume to become a
    // referrer.
    let mut param: perps::Param = suite
        .query_wasm_smart(contracts.perps, perps::QueryParamRequest {})
        .should_succeed();

    param.min_referrer_volume = UsdValue::new_int(10_000);

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param,
                pair_params: Default::default(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // User2 has zero volume — should fail.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user2,
        Dimensionless::new_percent(50),
    )
    .await
    .should_fail_with_error("insufficient perps volume to become a referrer");
}

/// Volume for referrer eligibility is aggregated across all accounts of a user.
/// User1 has two accounts that each trade below the threshold individually,
/// but together meet the $10,000 minimum.
#[tokio::test]
async fn set_share_ratio_aggregates_volume_across_accounts() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // Configure the perps contract to require $10,000 volume to become a referrer.
    let mut param: perps::Param = suite
        .query_wasm_smart(contracts.perps, perps::QueryParamRequest {})
        .should_succeed();

    param.min_referrer_volume = UsdValue::new_int(10_000);

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param,
                pair_params: Default::default(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Register oracle prices: ETH = $1,000.
    register_oracle_prices(&mut suite, &mut accounts, 1_000).await;

    // Create a second account for user1 (same user_index, different address).
    let mut user1_account2 = accounts
        .user1
        .register_new_account(
            &mut suite,
            contracts.account_factory,
            Coins::one(usdc::DENOM.clone(), 50_000_000_000).unwrap(),
        )
        .await
        .unwrap();

    // Deposit margin: user1 accounts and user2 (counterparty).
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 10_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut user1_account2, 10_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 50_000).await;

    // Trade 1: user2 sells 7 ETH at $1,000, user1 account1 buys → $7,000 notional.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user2,
        UsdPrice::new_int(1_000),
        7,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user1, 7).await;

    // With only $7,000 volume, user1 cannot become a referrer.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(50),
    )
    .await
    .should_fail_with_error("insufficient perps volume to become a referrer");

    // Trade 2: user2 sells 3 ETH at $1,000, user1 account2 buys → $3,000 notional.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user2,
        UsdPrice::new_int(1_000),
        3,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut user1_account2, 3).await;

    // Now the combined volume across both accounts is $10,000 — should succeed.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(50),
    )
    .await
    .should_succeed();
}

/// When `referral.active` is false, no fee commissions are applied.
#[tokio::test]
async fn referral_active_flag() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user8, 100_000).await;

    // User1 becomes a referrer, User2 is the referee.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(50),
    )
    .await
    .should_succeed();

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Set commission rate override so we know the exact commission amount.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetCommissionRateOverride {
                user: 1,
                commission_rate: Op::Insert(CommissionRate::new_percent(50)),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Disable referral system.
    let mut param: perps::Param = suite
        .query_wasm_smart(contracts.perps, perps::QueryParamRequest {})
        .should_succeed();

    param.referral_active = false;

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: param.clone(),
                pair_params: Default::default(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Record initial margins.
    let user1_margin_before = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .map(|s: perps::UserState| s.margin)
        .unwrap();

    // Trade: user8 places ask, user2 buys.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user8,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user2, 1).await;

    // User1 (referrer) should NOT have received any commission.
    let user1_margin_after = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .map(|s: perps::UserState| s.margin)
        .unwrap();

    assert_eq!(user1_margin_before, user1_margin_after);

    // Re-enable referral system.
    param.referral_active = true;

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param,
                pair_params: Default::default(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Trade again.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user8,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user2, 1).await;

    // User1 (referrer) should now have received a commission.
    let user1_margin_final: UsdValue = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .map(|s: perps::UserState| s.margin)
        .unwrap();

    assert!(user1_margin_final > user1_margin_after);
}

/// Commission rate override: only owner can set, overrides volume tiers,
/// removing the override falls back to volume-based calculation.
#[tokio::test]
async fn commission_rate_override() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // Configure commission rate tiers.
    let mut param = suite
        .query_wasm_smart(contracts.perps, perps::QueryParamRequest {})
        .should_succeed();

    param.referrer_commission_rates = RateSchedule {
        base: CommissionRate::new_percent(10),
        tiers: btree_map! {
            UsdValue::new_int(100) => CommissionRate::new_percent(20),
        },
    };

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param,
                pair_params: Default::default(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;

    // User1 becomes a referrer.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(20),
    )
    .await
    .should_succeed();

    // User2 sets User1 as referrer.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Default commission rate is 10%.
    let settings = query_referral_settings(&suite, contracts.perps, 1).unwrap();
    assert_eq!(settings.commission_rate, CommissionRate::new_percent(10));

    // Non-owner cannot set override.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetCommissionRateOverride {
                user: 1,
                commission_rate: Op::Insert(CommissionRate::new_percent(50)),
            }),
            Coins::new(),
        )
        .await
        .should_fail_with_error("you don't have the right");

    // Owner sets override to 50%.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetCommissionRateOverride {
                user: 1,
                commission_rate: Op::Insert(CommissionRate::new_percent(50)),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let settings = query_referral_settings(&suite, contracts.perps, 1).unwrap();
    assert_eq!(settings.commission_rate, CommissionRate::new_percent(50));

    // Trade to generate volume past the 100 USD tier.
    create_perps_fill(&mut suite, &mut accounts, contracts.perps, 2_000, 1).await;

    // Override still applies (ignores volume tier).
    let settings = query_referral_settings(&suite, contracts.perps, 1).unwrap();
    assert_eq!(settings.commission_rate, CommissionRate::new_percent(50));

    // Owner removes override.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetCommissionRateOverride {
                user: 1,
                commission_rate: Op::Delete,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Falls back to volume-based tier (>= 100 → 20%).
    let settings = query_referral_settings(&suite, contracts.perps, 1).unwrap();
    assert_eq!(settings.commission_rate, CommissionRate::new_percent(20));
}

/// Query per-referee statistics sorted by volume.
#[tokio::test]
async fn referrer_stats() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user3, 100_000).await;

    // User1 becomes a referrer.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(20),
    )
    .await
    .should_succeed();

    // User2 and User3 set User1 as referrer.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 3,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // User2 trades (volume = 1 * 2000 = $2,000).
    create_perps_fill(&mut suite, &mut accounts, contracts.perps, 2_000, 1).await;

    // User3 trades more (volume = 2 * 2000 = $4,000).
    // Need user3 as taker — swap user1 as maker, user3 as taker.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        UsdPrice::new_int(2_000),
        2,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user3, 2).await;

    // Query stats sorted by volume descending.
    let stats: Vec<(Referee, perps::RefereeStats)> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryReferrerToRefereeStatsRequest {
                referrer: 1,
                order_by: ReferrerStatsOrderBy {
                    order: IterationOrder::Descending,
                    limit: None,
                    index: ReferrerStatsOrderIndex::Volume { start_after: None },
                },
            },
        )
        .should_succeed();

    assert_eq!(stats.len(), 2);
    // User3 has more volume, should be first in descending order.
    assert_eq!(stats[0].0, 3);
    assert_eq!(stats[1].0, 2);

    // Query ascending.
    let stats: Vec<(Referee, perps::RefereeStats)> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryReferrerToRefereeStatsRequest {
                referrer: 1,
                order_by: ReferrerStatsOrderBy {
                    order: IterationOrder::Ascending,
                    limit: None,
                    index: ReferrerStatsOrderIndex::Volume { start_after: None },
                },
            },
        )
        .should_succeed();

    assert_eq!(stats[0].0, 2);
    assert_eq!(stats[1].0, 3);

    let user2_volume = stats[0].1.volume;
    let user3_volume = stats[1].1.volume;

    // start_after: skip user3's volume in descending order → only user2 remains.
    let stats: Vec<(Referee, perps::RefereeStats)> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryReferrerToRefereeStatsRequest {
                referrer: 1,
                order_by: ReferrerStatsOrderBy {
                    order: IterationOrder::Descending,
                    limit: None,
                    index: ReferrerStatsOrderIndex::Volume {
                        start_after: Some(user3_volume),
                    },
                },
            },
        )
        .should_succeed();

    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].0, 2);
    assert_eq!(stats[0].1.volume, user2_volume);

    // limit: only return 1 result in descending order → user3 (highest volume).
    let stats: Vec<(Referee, perps::RefereeStats)> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryReferrerToRefereeStatsRequest {
                referrer: 1,
                order_by: ReferrerStatsOrderBy {
                    order: IterationOrder::Descending,
                    limit: Some(1),
                    index: ReferrerStatsOrderIndex::Volume { start_after: None },
                },
            },
        )
        .should_succeed();

    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].0, 3);
}

/// Verify that fee commissions are correctly credited to margins across a
/// multi-level referral chain (up to MAX_REFERRAL_CHAIN_DEPTH = 5).
///
/// Chain: user1 ← user2 ← user3 ← user4 ← user5 ← user6 ← user7
///
/// Commission rate overrides:
///   user6 = 10%, user5 = 20%, user4 = 20%, user3 = 10%, user2 = 60%
///
/// When user7 trades (notional = $2,000, fee = $2, vault_fee = $2):
///   - user7 (referee):      vault_fee × 10% × 20% = $0.04
///   - user6 (1st referrer): vault_fee × 10% × 80% = $0.16
///   - user5 (2nd referrer): vault_fee × (20% - 10%) = $0.20 (marginal)
///   - user4 (3rd referrer): 20% ≤ max(20%) → $0
///   - user3 (4th referrer): 10% < max(20%) → $0
///   - user2 (5th referrer): vault_fee × (60% - 20%) = $0.80 (marginal)
///   - user1 (6th referrer): outside chain depth → $0
#[tokio::test]
async fn commission_rate_margins() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    let params = suite
        .query_wasm_smart(contracts.perps, QueryParamRequest {})
        .unwrap();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // Deposit margin for user8 (maker) and all referee/referrer users.
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user3, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user4, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user5, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user6, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user7, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user8, 100_000).await;

    // All users become referrers with 20% share ratio.
    for user in [
        &mut accounts.user1 as &mut (dyn Signer + Send + Sync),
        &mut accounts.user2,
        &mut accounts.user3,
        &mut accounts.user4,
        &mut accounts.user5,
        &mut accounts.user6,
    ] {
        set_fee_share_ratio(
            &mut suite,
            contracts.perps,
            user,
            Dimensionless::new_percent(20),
        )
        .await
        .should_succeed();
    }

    // Build referral chain: user1 ← user2 ← user3 ← user4 ← user5 ← user6 ← user7.
    for (referrer, referee) in [(1, 2), (2, 3), (3, 4), (4, 5), (5, 6), (6, 7)] {
        let sender: &mut (dyn Signer + Send + Sync) = match referee {
            2 => &mut accounts.user2,
            3 => &mut accounts.user3,
            4 => &mut accounts.user4,
            5 => &mut accounts.user5,
            6 => &mut accounts.user6,
            7 => &mut accounts.user7,
            _ => unreachable!(),
        };
        suite
            .execute(
                sender,
                contracts.perps,
                &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral { referrer, referee }),
                Coins::new(),
            )
            .await
            .should_succeed();
    }

    // Set commission rate overrides (owner-only).
    for (user, rate) in [(6, 10), (5, 20), (4, 20), (3, 10), (2, 60)] {
        suite
            .execute(
                &mut accounts.owner,
                contracts.perps,
                &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetCommissionRateOverride {
                    user,
                    commission_rate: Op::Insert(CommissionRate::new_percent(rate)),
                }),
                Coins::new(),
            )
            .await
            .should_succeed();
    }

    // Record initial margins and referral data.
    let initial_margins: Vec<UsdValue> = [
        accounts.user1.address(),
        accounts.user2.address(),
        accounts.user3.address(),
        accounts.user4.address(),
        accounts.user5.address(),
        accounts.user6.address(),
        accounts.user7.address(),
    ]
    .iter()
    .map(|addr| {
        suite
            .query_wasm_smart(
                contracts.perps,
                perps::QueryUserStateRequest { user: *addr },
            )
            .should_succeed()
            .map(|s: perps::UserState| s.margin)
            .unwrap_or(UsdValue::ZERO)
    })
    .collect();

    let initial_referral_data: Vec<UserReferralData> = (1..=7)
        .map(|i| query_referral_data(&suite, contracts.perps, i, None))
        .collect();

    // User7 trades: user8 places ask (maker), user7 buys (taker).
    // Notional = 1 × $2,000 = $2,000. Fee = $2,000 × 0.1% = $2.
    let price = UsdPrice::new_int(2_000);
    let size = 1;
    let trade_value = price.checked_mul(Quantity::new_int(size)).unwrap();
    place_ask_order(&mut suite, contracts.perps, &mut accounts.user8, price, 1).await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user7, 1).await;

    // Read post-trade margins.
    let post_margins: Vec<UsdValue> = [
        accounts.user1.address(),
        accounts.user2.address(),
        accounts.user3.address(),
        accounts.user4.address(),
        accounts.user5.address(),
        accounts.user6.address(),
        accounts.user7.address(),
    ]
    .iter()
    .map(|addr| {
        suite
            .query_wasm_smart(
                contracts.perps,
                perps::QueryUserStateRequest { user: *addr },
            )
            .should_succeed()
            .map(|s: perps::UserState| s.margin)
            .unwrap_or(UsdValue::ZERO)
    })
    .collect();

    // vault_fee = trade_value * taker_fee = $2.00
    let vault_fee = trade_value
        .checked_mul(params.taker_fee_rates.base)
        .unwrap();

    assert_eq!(vault_fee, UsdValue::new_int(2));

    // User7 (referee, index 6): gets vault_fee × 10% × 20% = $0.04
    let referee_share = vault_fee
        .checked_mul(CommissionRate::new_percent(10))
        .unwrap()
        .checked_mul(Dimensionless::new_percent(20))
        .unwrap();
    assert_eq!(referee_share, UsdValue::new_percent(4));

    // User6 (1st referrer, index 5): gets vault_fee × 10% × 80% = $0.16
    let referrer_commission = vault_fee
        .checked_mul(CommissionRate::new_percent(10))
        .unwrap()
        .checked_sub(referee_share)
        .unwrap();
    assert_eq!(referrer_commission, UsdValue::new_percent(16)); // $0.16

    // User5 (2nd referrer, index 4): marginal = 20% - 10% = 10% → $0.20
    let user5_commission = vault_fee
        .checked_mul(CommissionRate::new_percent(10))
        .unwrap();
    assert_eq!(user5_commission, UsdValue::new_percent(20)); // $0.20

    // User4 (3rd referrer, index 3): 20% ≤ max(20%) → $0
    // User3 (4th referrer, index 2): 10% < max(20%) → $0

    // User2 (5th referrer, index 1): marginal = 60% - 20% = 40% → $0.80
    let user2_commission = vault_fee
        .checked_mul(CommissionRate::new_percent(40))
        .unwrap();
    assert_eq!(user2_commission, UsdValue::new_percent(80)); // $0.80

    // User1 (6th referrer, index 0): outside MAX_REFERRAL_CHAIN_DEPTH → $0

    // Verify margin changes for each user.
    // User7 paid fee ($2) but got referee_share ($0.04). Also has position change.

    assert_eq!(
        post_margins[6],
        initial_margins[6]
            .checked_add(referee_share)
            .unwrap()
            .checked_sub(vault_fee)
            .unwrap()
    );

    // User6: margin increased by referrer_commission.
    assert_eq!(
        post_margins[5].checked_sub(initial_margins[5]).unwrap(),
        referrer_commission,
    );

    // User5: margin increased by marginal commission.
    assert_eq!(
        post_margins[4].checked_sub(initial_margins[4]).unwrap(),
        user5_commission,
    );

    // User4: no change.
    assert_eq!(post_margins[3], initial_margins[3]);

    // User3: no change.
    assert_eq!(post_margins[2], initial_margins[2]);

    // User2: margin increased by marginal commission.
    assert_eq!(
        post_margins[1].checked_sub(initial_margins[1]).unwrap(),
        user2_commission,
    );

    // User1 (6th referrer): outside chain depth → no margin change.
    assert_eq!(post_margins[0], initial_margins[0]);

    // Verify referral data updates.
    let post_referral_data: Vec<UserReferralData> = (1..=7)
        .map(|i| query_referral_data(&suite, contracts.perps, i, None))
        .collect();

    // User7 (payer): volume increased, commission_shared_by_referrer increased.
    assert_eq!(
        post_referral_data[6].volume,
        initial_referral_data[6]
            .volume
            .checked_add(trade_value)
            .unwrap()
    );
    assert_eq!(
        post_referral_data[6].commission_shared_by_referrer,
        initial_referral_data[6]
            .commission_shared_by_referrer
            .checked_add(referee_share)
            .unwrap()
    );

    // User6 (1st referrer): referees_volume += trade_value, commission_earned_from_referees += referrer_commission.
    assert_eq!(
        post_referral_data[5].referees_volume,
        initial_referral_data[5]
            .referees_volume
            .checked_add(trade_value)
            .unwrap()
    );
    assert_eq!(
        post_referral_data[5].commission_earned_from_referees,
        initial_referral_data[5]
            .commission_earned_from_referees
            .checked_add(referrer_commission)
            .unwrap()
    );

    // User5 (2nd referrer): referees_volume unchanged, commission_earned_from_referees += user5_commission.
    assert_eq!(
        post_referral_data[4].referees_volume,
        initial_referral_data[4].referees_volume
    );
    assert_eq!(
        post_referral_data[4].commission_earned_from_referees,
        initial_referral_data[4]
            .commission_earned_from_referees
            .checked_add(user5_commission)
            .unwrap()
    );

    // User4 (3rd): no referral data change.
    assert_eq!(post_referral_data[3], initial_referral_data[3]);

    // User3 (4th): no referral data change.
    assert_eq!(post_referral_data[2], initial_referral_data[2]);

    // User2 (5th referrer): referees_volume unchanged, commission_earned_from_referees += user2_commission.
    assert_eq!(
        post_referral_data[1].referees_volume,
        initial_referral_data[1].referees_volume
    );
    assert_eq!(
        post_referral_data[1].commission_earned_from_referees,
        initial_referral_data[1]
            .commission_earned_from_referees
            .checked_add(user2_commission)
            .unwrap()
    );

    // User1 (6th): no referral data change (outside chain depth).
    assert_eq!(post_referral_data[0], initial_referral_data[0]);
}

/// Referee count increments when referral relationships are set.
#[tokio::test]
async fn referee_count() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // User1 becomes a referrer.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(20),
    )
    .await
    .should_succeed();

    // Initially, referee_count is 0.
    let data = query_referral_data(&suite, contracts.perps, 1, None);
    assert_eq!(data.referee_count, 0);

    // User2 sets User1 as referrer → referee_count = 1.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let data = query_referral_data(&suite, contracts.perps, 1, None);
    assert_eq!(data.referee_count, 1);

    // User3 also sets User1 as referrer → referee_count = 2.
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 3,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let data = query_referral_data(&suite, contracts.perps, 1, None);
    assert_eq!(data.referee_count, 2);

    // User4 sets User2 as referrer → User1's count stays 2, User2's count = 1.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user2,
        Dimensionless::new_percent(20),
    )
    .await
    .should_succeed();

    suite
        .execute(
            &mut accounts.user4,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 2,
                referee: 4,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let data_user1 = query_referral_data(&suite, contracts.perps, 1, None);
    assert_eq!(data_user1.referee_count, 2);

    let data_user2 = query_referral_data(&suite, contracts.perps, 2, None);
    assert_eq!(data_user2.referee_count, 1);
}

/// Active users count increments once per referee per day.
#[tokio::test]
async fn active_referral() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user3, 100_000).await;

    // User1 becomes a referrer.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(20),
    )
    .await
    .should_succeed();

    // User2 and User3 set User1 as referrer.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 3,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // User2 trades — cumulative_daily_active_referees should be 1, cumulative_global_active_referees should be 1.
    create_perps_fill(&mut suite, &mut accounts, contracts.perps, 2_000, 1).await;

    let data = query_referral_data(&suite, contracts.perps, 1, None);
    assert_eq!(data.cumulative_daily_active_referees, 1);
    assert_eq!(data.cumulative_global_active_referees, 1);

    // User2 trades again same day — both counters unchanged.
    create_perps_fill(&mut suite, &mut accounts, contracts.perps, 2_000, 1).await;

    let data = query_referral_data(&suite, contracts.perps, 1, None);
    assert_eq!(data.cumulative_daily_active_referees, 1);
    assert_eq!(data.cumulative_global_active_referees, 1);

    // User3 trades — cumulative_daily_active_referees should be 2, cumulative_global_active_referees should be 2.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user3, 1).await;

    let data = query_referral_data(&suite, contracts.perps, 1, None);
    assert_eq!(data.cumulative_daily_active_referees, 2);
    assert_eq!(data.cumulative_global_active_referees, 2);

    // Next day, User2 trades — cumulative_daily_active_referees should be 3 (cumulative),
    // but cumulative_global_active_referees stays at 2 (User2 already traded before).
    suite.block_time = Duration::from_days(1);
    suite.make_empty_block().await;

    create_perps_fill(&mut suite, &mut accounts, contracts.perps, 2_000, 1).await;

    let data = query_referral_data(&suite, contracts.perps, 1, None);
    assert_eq!(data.cumulative_daily_active_referees, 3);
    assert_eq!(data.cumulative_global_active_referees, 2);
}

/// With a negative maker fee (rebate), the maker's referrer must NOT be
/// debited, and the taker's referrer must earn commission computed from
/// the **net** vault fee — not from the taker's gross fee (which would
/// overpay commissions beyond what the protocol actually collected).
#[tokio::test]
async fn negative_maker_fee_does_not_debit_referrers() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // Configure negative maker fee. taker = 3 bps, maker = -1 bps, protocol = 20%.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: perps::Param {
                    taker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(300),
                        ..Default::default()
                    },
                    maker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(-100),
                        ..Default::default()
                    },
                    protocol_fee_rate: Dimensionless::new_percent(20),
                    referrer_commission_rates: RateSchedule {
                        base: Dimensionless::new_percent(10),
                        ..Default::default()
                    },
                    referral_active: true,
                    ..default_param()
                },
                pair_params: btree_map! {
                    pair.clone() => default_pair_param(),
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Deposit for users 1-4.
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user3, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user4, 100_000).await;

    // user1 is the taker's referrer; user2 is the maker's referrer. Both
    // become referrers by setting a 20% fee share ratio.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(20),
    )
    .await
    .should_succeed();
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user2,
        Dimensionless::new_percent(20),
    )
    .await
    .should_succeed();

    // Wire the referral relationships:
    //   user4 (taker) ← user1 (referrer)
    //   user3 (maker) ← user2 (referrer)
    for (sender, referrer, referee) in [(4u32, 1u32, 4u32), (3, 2, 3)] {
        let signer: &mut (dyn Signer + Send + Sync) = match sender {
            3 => &mut accounts.user3,
            4 => &mut accounts.user4,
            _ => unreachable!(),
        };
        suite
            .execute(
                signer,
                contracts.perps,
                &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral { referrer, referee }),
                Coins::new(),
            )
            .await
            .should_succeed();
    }

    // Snapshot margins for all four users before the rebate trade.
    let user_addrs = [
        accounts.user1.address(),
        accounts.user2.address(),
        accounts.user3.address(),
        accounts.user4.address(),
    ];
    let margin_before: Vec<UsdValue> = user_addrs
        .iter()
        .map(|addr| {
            suite
                .query_wasm_smart(
                    contracts.perps,
                    perps::QueryUserStateRequest { user: *addr },
                )
                .should_succeed()
                .map(|s: perps::UserState| s.margin)
                .unwrap_or(UsdValue::ZERO)
        })
        .collect();

    // user3 places a post-only ask (1 ETH @ $2,000); user4 markets into it.
    //   Notional       = $2,000
    //   Taker fee      = 3 bps * $2,000    = $0.60
    //   Maker fee      = -1 bps * $2,000   = -$0.20 (rebate)
    //   Net fee        = $0.40
    //   Protocol fee   = $0.40 * 20%       = $0.08
    //   Vault fee      = $0.40 * 80%       = $0.32
    //   Total positive = $0.60 (only taker contributes weight)
    //   Taker referrer (10%): $0.32 * 10% * 80% = $0.0256
    //   Maker referrer     : 0 (weight(maker) = max(-0.20, 0) = 0)
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user3,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user4, 1).await;

    // Snapshot margins after.
    let margin_after: Vec<UsdValue> = user_addrs
        .iter()
        .map(|addr| {
            suite
                .query_wasm_smart(
                    contracts.perps,
                    perps::QueryUserStateRequest { user: *addr },
                )
                .should_succeed()
                .map(|s: perps::UserState| s.margin)
                .unwrap_or(UsdValue::ZERO)
        })
        .collect();

    // Maker's referrer (user2) must NOT be debited — this is the core fix.
    // Under the old per-user algorithm a negative maker vault_fee would
    // multiply through the commission math and *remove* margin here.
    assert_eq!(
        margin_after[1], margin_before[1],
        "maker's referrer must not be debited for a maker rebate"
    );

    // Taker's referrer (user1) earned commission from the *net* vault fee,
    // not from the taker's gross fee. With vault_fee = $0.32 and a 10%
    // commission rate shared 80/20 with the referee, user1 gets $0.0256.
    let trade_value = UsdValue::new_int(2_000);
    let expected_vault_fee = trade_value
        .checked_mul(Dimensionless::new_raw(300))
        .unwrap()
        .checked_sub(
            trade_value
                .checked_mul(Dimensionless::new_raw(100))
                .unwrap(),
        )
        .unwrap()
        .checked_mul(Dimensionless::new_percent(80))
        .unwrap();
    let expected_user1_gain = expected_vault_fee
        .checked_mul(Dimensionless::new_percent(10))
        .unwrap()
        .checked_mul(Dimensionless::new_percent(80))
        .unwrap();
    assert_eq!(
        margin_after[0].checked_sub(margin_before[0]).unwrap(),
        expected_user1_gain,
        "taker's referrer earns proportional commission on the net vault fee"
    );

    // Maker's own margin — user3 is the rebating maker, so they gained
    // the rebate ($0.20). They are also a referee of user2, so they
    // received a 20% share of user2's zero commission = $0 extra.
    let rebate = trade_value
        .checked_mul(Dimensionless::new_raw(100))
        .unwrap();
    assert_eq!(
        margin_after[2].checked_sub(margin_before[2]).unwrap(),
        rebate,
        "rebating maker's margin increases by the full rebate and nothing more"
    );
}

/// A payer without a referrer still gets a `FeeDistributed` event with
/// correct protocol_fee, vault_fee, and empty commissions.
#[tokio::test]
async fn fee_distributed_event_without_referrer() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // Set protocol fee to 50%.
    let mut param: perps::Param = suite
        .query_wasm_smart(contracts.perps, perps::QueryParamRequest {})
        .should_succeed();

    param.protocol_fee_rate = Dimensionless::new_percent(50);

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param,
                pair_params: Default::default(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;

    // Trade without any referral relationship.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    let events =
        place_market_buy_with_events(&mut suite, contracts.perps, &mut accounts.user2, 1).await;

    let fee_events: Vec<FeeDistributed> = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "fee_distributed")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json().unwrap())
        .collect();

    // Maker fee is zero, so only the taker gets a FeeDistributed event.
    assert_eq!(
        fee_events.len(),
        1,
        "One FeeDistributed event must be emitted for the taker"
    );

    // Notional = 1 × $2,000 = $2,000. Taker fee = 0.1% = $2. Protocol fee = 50%.
    // With no commissions: protocol_fee = $1, vault_fee = $1.
    let expected_vault_fee = UsdValue::new_int(1);
    let expected_protocol_fee = UsdValue::new_int(1);

    let total_vault_fee: UsdValue = fee_events
        .iter()
        .map(|e| e.vault_fee)
        .try_fold(UsdValue::ZERO, |acc, v| acc.checked_add(v))
        .unwrap();
    let total_protocol_fee: UsdValue = fee_events
        .iter()
        .map(|e| e.protocol_fee)
        .try_fold(UsdValue::ZERO, |acc, v| acc.checked_add(v))
        .unwrap();

    assert_eq!(total_vault_fee, expected_vault_fee);
    assert_eq!(total_protocol_fee, expected_protocol_fee);

    for event in &fee_events {
        assert!(event.commissions.is_empty());
    }
}

/// A payer with a referrer gets a `FeeDistributed` event with correct
/// protocol_fee, vault_fee (reduced by commissions), and commissions.
#[tokio::test]
async fn fee_distributed_event_with_referrer() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // Set protocol fee to 50%.
    let mut param: perps::Param = suite
        .query_wasm_smart(contracts.perps, perps::QueryParamRequest {})
        .should_succeed();

    param.protocol_fee_rate = Dimensionless::new_percent(50);

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param,
                pair_params: Default::default(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user8, 100_000).await;

    // User1 becomes a referrer with 20% share ratio.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(20),
    )
    .await
    .should_succeed();

    // User2 sets User1 as referrer.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Set a known commission rate override for deterministic math.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetCommissionRateOverride {
                user: 1,
                commission_rate: Op::Insert(CommissionRate::new_percent(50)),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // User8 places ask (maker), User2 (referee) buys (taker).
    // Notional = 1 × $2,000 = $2,000. Taker fee = 0.1% = $2.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user8,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    let events =
        place_market_buy_with_events(&mut suite, contracts.perps, &mut accounts.user2, 1).await;

    let fee_events: Vec<FeeDistributed> = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "fee_distributed")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json().unwrap())
        .collect();

    // The taker (user2) has a referrer → non-empty commissions.
    let taker_event = fee_events
        .iter()
        .find(|e| e.payer_addr == accounts.user2.address())
        .expect("taker must have a FeeDistributed event");

    // Notional = $2,000. Taker fee = 0.1% = $2.
    // protocol_fee = $2 × 50% = $1. vault_fee (before commissions) = $1.
    // commission_rate = 50%, share_ratio = 20%.
    // total_commission = $1 × 50% = $0.50
    // referee_share    = $0.50 × 20% = $0.10
    // referrer_share   = $0.50 × 80% = $0.40
    // vault_fee (after) = $1 − $0.50 = $0.50
    let expected_protocol_fee = UsdValue::new_int(1);
    let expected_vault_fee_before = UsdValue::new_int(1);
    let expected_total_commission = expected_vault_fee_before
        .checked_mul(CommissionRate::new_percent(50))
        .unwrap();
    let expected_referee_share = expected_total_commission
        .checked_mul(Dimensionless::new_percent(20))
        .unwrap();
    let expected_referrer_share = expected_total_commission
        .checked_sub(expected_referee_share)
        .unwrap();
    let expected_vault_fee = expected_vault_fee_before
        .checked_sub(expected_total_commission)
        .unwrap();

    assert_eq!(taker_event.protocol_fee, expected_protocol_fee);
    assert_eq!(taker_event.vault_fee, expected_vault_fee);
    assert_eq!(taker_event.commissions.len(), 2);
    assert_eq!(taker_event.commissions[0], expected_referee_share);
    assert_eq!(taker_event.commissions[1], expected_referrer_share);
}

/// Regression test: when the chain owner uses the `set_referral` bypass to
/// wire up a referee-to-referrer relationship without the referrer having
/// opted in (no `FEE_SHARE_RATIO` entry), the referee's trades must still
/// settle. The referrer's share ratio defaults to zero, so the full
/// commission flows to the referrer and the referee receives no rebate.
#[tokio::test]
async fn fee_distributed_event_with_referrer_without_share_ratio() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    // Set protocol fee to 50%.
    let mut param: perps::Param = suite
        .query_wasm_smart(contracts.perps, perps::QueryParamRequest {})
        .should_succeed();

    param.protocol_fee_rate = Dimensionless::new_percent(50);

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param,
                pair_params: Default::default(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user8, 100_000).await;

    // Note: deliberately do NOT call `set_fee_share_ratio` for user1 — the
    // chain owner is wiring up the relationship via the bypass below.

    // Owner sets User1 as User2's referrer (bypasses the opt-in requirement).
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Set a known commission rate override for deterministic math.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetCommissionRateOverride {
                user: 1,
                commission_rate: Op::Insert(CommissionRate::new_percent(50)),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // User8 places ask (maker), User2 (referee) buys (taker).
    // Notional = 1 × $2,000 = $2,000. Taker fee = 0.1% = $2.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user8,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    let events =
        place_market_buy_with_events(&mut suite, contracts.perps, &mut accounts.user2, 1).await;

    let fee_events: Vec<FeeDistributed> = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "fee_distributed")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json().unwrap())
        .collect();

    let taker_event = fee_events
        .iter()
        .find(|e| e.payer_addr == accounts.user2.address())
        .expect("taker must have a FeeDistributed event");

    // Notional = $2,000. Taker fee = 0.1% = $2.
    // protocol_fee = $2 × 50% = $1. vault_fee (before commissions) = $1.
    // commission_rate = 50%, share_ratio = 0% (defaulted, no opt-in).
    // total_commission = $1 × 50% = $0.50
    // referee_share    = $0.50 × 0%   = $0
    // referrer_share   = $0.50 × 100% = $0.50
    // vault_fee (after) = $1 − $0.50 = $0.50
    let expected_protocol_fee = UsdValue::new_int(1);
    let expected_vault_fee_before = UsdValue::new_int(1);
    let expected_total_commission = expected_vault_fee_before
        .checked_mul(CommissionRate::new_percent(50))
        .unwrap();
    let expected_referee_share = UsdValue::ZERO;
    let expected_referrer_share = expected_total_commission;
    let expected_vault_fee = expected_vault_fee_before
        .checked_sub(expected_total_commission)
        .unwrap();

    assert_eq!(taker_event.protocol_fee, expected_protocol_fee);
    assert_eq!(taker_event.vault_fee, expected_vault_fee);
    assert_eq!(taker_event.commissions.len(), 2);
    assert_eq!(taker_event.commissions[0], expected_referee_share);
    assert_eq!(taker_event.commissions[1], expected_referrer_share);
}

// ---------------------------------------------------------------------------
// Edge-case: a referrer with commission_rate_override = 100% (the cap).
//
// On mainnet a referrer A is being given the maximum commission rate of 100%
// while sitting at level 2 of the chain (A → B → trader), with B at 45% and
// both A and B running share_ratio = 0. The interesting consequence is that
// the *entire* vault fee on a fee-paying fill is funnelled to the referrers
// (B gets 45%, A gets the marginal 55%) and the vault's net cut on that fill
// is zero. The tests below pin this behavior across direct/indirect chains
// and taker/maker fills (positive and rebate maker fee).
// ---------------------------------------------------------------------------

/// Direct chain A → trader. A.cr = 100%, A.sr = 0. Trader is the taker.
///
/// Per book/perps/6-referral.md §5a:
///   referee_share      = vault_fee × 100% × 0   = 0
///   referrer_commission = vault_fee × 100% × 1  = vault_fee
///
/// All of vault_fee flows to A; the vault's net cut is zero.
#[tokio::test]
async fn cr_100_direct_referrer_taker() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    let params = suite
        .query_wasm_smart(contracts.perps, QueryParamRequest {})
        .unwrap();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user3, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user4, 100_000).await;

    // A opts in as a referrer with share_ratio = 0.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(0),
    )
    .await
    .should_succeed();

    // Trader (user3) is referee of A (user1).
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 3,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    set_commission_rate_override(&mut suite, contracts.perps, &mut accounts.owner, 1, 100)
        .await
        .should_succeed();

    let user_addrs = [accounts.user1.address(), accounts.user3.address()];
    let margin_before = snapshot_margins(&suite, contracts.perps, &user_addrs);

    // user4 (no referrer) places ask; trader (taker) market buys.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user4,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user3, 1).await;

    let margin_after = snapshot_margins(&suite, contracts.perps, &user_addrs);

    // vault_fee = $2,000 × 0.1% = $2.
    let vault_fee = UsdValue::new_int(2_000)
        .checked_mul(params.taker_fee_rates.base)
        .unwrap();
    assert_eq!(vault_fee, UsdValue::new_int(2));

    // A receives the entire vault_fee.
    assert_eq!(
        margin_after[0].checked_sub(margin_before[0]).unwrap(),
        vault_fee,
        "A (cr=100%, sr=0) should receive the entire vault_fee",
    );

    // Trader paid the full taker fee, no rebate share.
    assert_eq!(
        margin_before[1].checked_sub(margin_after[1]).unwrap(),
        vault_fee,
        "trader pays the full taker fee with no rebate (sr=0)",
    );
}

/// **Mainnet scenario.** Indirect chain A → B → trader. A.cr = 100%, B.cr =
/// 45%, both share_ratio = 0. Trader is the taker.
///
/// Per book/perps/6-referral.md §§5a-5b:
///   level 1 (B): vault_fee × 45% × 1   = vault_fee × 45%
///   level 2 (A): vault_fee × (100% − 45%) = vault_fee × 55%
///   total = vault_fee × 100% → entire vault_fee distributed; vault net = 0.
///
/// Also asserts the `FeeDistributed` event reflects this (vault_fee in the
/// event drops to zero post-commission, commissions = [0, 0.90, 1.10]).
#[tokio::test]
async fn cr_100_indirect_referrer_taker() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    let params = suite
        .query_wasm_smart(contracts.perps, QueryParamRequest {})
        .unwrap();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user3, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user4, 100_000).await;

    // A and B both opt in as referrers with share_ratio = 0.
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(0),
    )
    .await
    .should_succeed();
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user2,
        Dimensionless::new_percent(0),
    )
    .await
    .should_succeed();

    // Wire chain: user1 (A) ← user2 (B) ← user3 (trader).
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 2,
                referee: 3,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    set_commission_rate_override(&mut suite, contracts.perps, &mut accounts.owner, 1, 100)
        .await
        .should_succeed();
    set_commission_rate_override(&mut suite, contracts.perps, &mut accounts.owner, 2, 45)
        .await
        .should_succeed();

    let user_addrs = [
        accounts.user1.address(),
        accounts.user2.address(),
        accounts.user3.address(),
    ];
    let margin_before = snapshot_margins(&suite, contracts.perps, &user_addrs);

    // user4 (no referrer) places ask; trader (taker) market buys.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user4,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    let events =
        place_market_buy_with_events(&mut suite, contracts.perps, &mut accounts.user3, 1).await;

    let margin_after = snapshot_margins(&suite, contracts.perps, &user_addrs);

    let vault_fee = UsdValue::new_int(2_000)
        .checked_mul(params.taker_fee_rates.base)
        .unwrap();
    assert_eq!(vault_fee, UsdValue::new_int(2));

    let expected_b = vault_fee
        .checked_mul(CommissionRate::new_percent(45))
        .unwrap();
    let expected_a = vault_fee
        .checked_mul(CommissionRate::new_percent(55))
        .unwrap();
    assert_eq!(expected_b, UsdValue::new_percent(90));
    assert_eq!(
        expected_a,
        UsdValue::new_int(1)
            .checked_add(UsdValue::new_percent(10))
            .unwrap()
    );

    assert_eq!(
        margin_after[0].checked_sub(margin_before[0]).unwrap(),
        expected_a,
        "A (level-2, cr=100%) should receive marginal commission",
    );
    assert_eq!(
        margin_after[1].checked_sub(margin_before[1]).unwrap(),
        expected_b,
        "B (level-1, cr=45%, sr=0) should receive the full level-1 commission",
    );
    assert_eq!(
        margin_before[2].checked_sub(margin_after[2]).unwrap(),
        vault_fee,
        "trader pays the full taker fee with no rebate (sr=0)",
    );

    // Verify the FeeDistributed event: protocol_fee = 0, vault_fee post-
    // commission = 0 (vault keeps nothing), commissions = [0, B, A].
    let fee_events: Vec<FeeDistributed> = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "fee_distributed")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json().unwrap())
        .collect();
    let trader_event = fee_events
        .iter()
        .find(|e| e.payer_addr == accounts.user3.address())
        .expect("trader must have a FeeDistributed event");
    assert_eq!(trader_event.protocol_fee, UsdValue::ZERO);
    assert_eq!(
        trader_event.vault_fee,
        UsdValue::ZERO,
        "vault keeps nothing — entire vault_fee distributed to A and B",
    );
    assert_eq!(
        trader_event.commissions,
        vec![UsdValue::ZERO, expected_b, expected_a]
    );
}

/// Direct chain A → trader. A.cr = 100%, A.sr = 0. Trader is the maker on a
/// fill where both sides pay a positive fee, so the maker contributes weight
/// to the per-party split.
///
/// With taker = maker = 5 bps and protocol = 0, total_positive splits the
/// vault_fee 50/50, so the maker's vault_fee portion = trade_value × 5 bps
/// = $1.00. A receives that full $1.00 (cr=100%, sr=0).
#[tokio::test]
async fn cr_100_direct_referrer_maker_positive_fee() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    let pair = pair_id();

    // taker = +5 bps, maker = +5 bps, protocol = 0.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: perps::Param {
                    taker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(500),
                        ..Default::default()
                    },
                    maker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(500),
                        ..Default::default()
                    },
                    protocol_fee_rate: Dimensionless::ZERO,
                    referral_active: true,
                    ..default_param()
                },
                pair_params: btree_map! {
                    pair.clone() => default_pair_param(),
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user3, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user4, 100_000).await;

    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(0),
    )
    .await
    .should_succeed();
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 3,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();
    set_commission_rate_override(&mut suite, contracts.perps, &mut accounts.owner, 1, 100)
        .await
        .should_succeed();

    let user_addrs = [accounts.user1.address(), accounts.user3.address()];
    let margin_before = snapshot_margins(&suite, contracts.perps, &user_addrs);

    // Trader places post-only ask; user4 (no referrer) market buys.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user3,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user4, 1).await;

    let margin_after = snapshot_margins(&suite, contracts.perps, &user_addrs);

    let trade_value = UsdValue::new_int(2_000);
    let maker_fee = trade_value
        .checked_mul(Dimensionless::new_raw(500))
        .unwrap();
    // Maker's vault_fee portion = vault_fee × maker_weight = (taker_fee + maker_fee)
    // × (maker_fee / (taker_fee + maker_fee)) = maker_fee, with protocol = 0.
    let expected_maker_vault_fee = maker_fee;
    let expected_a = expected_maker_vault_fee
        .checked_mul(CommissionRate::new_percent(100))
        .unwrap();

    assert_eq!(expected_maker_vault_fee, UsdValue::new_int(1));

    assert_eq!(
        margin_after[0].checked_sub(margin_before[0]).unwrap(),
        expected_a,
        "A (cr=100%) should receive maker's full vault_fee portion",
    );
    // Trader paid maker_fee, got referee_share = 0 (sr = 0).
    assert_eq!(
        margin_before[1].checked_sub(margin_after[1]).unwrap(),
        maker_fee,
        "trader pays the full maker fee with no rebate (sr=0)",
    );
}

/// Indirect chain A → B → trader. A.cr = 100%, B.cr = 45%, both sr = 0.
/// Trader is the maker on a positive-fee fill.
///
/// Maker's vault_fee portion = $1.00 (as in the direct case). Of that:
///   level 1 (B): $1.00 × 45% = $0.45
///   level 2 (A): $1.00 × 55% = $0.55
#[tokio::test]
async fn cr_100_indirect_referrer_maker_positive_fee() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    let pair = pair_id();

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: perps::Param {
                    taker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(500),
                        ..Default::default()
                    },
                    maker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(500),
                        ..Default::default()
                    },
                    protocol_fee_rate: Dimensionless::ZERO,
                    referral_active: true,
                    ..default_param()
                },
                pair_params: btree_map! {
                    pair.clone() => default_pair_param(),
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user3, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user4, 100_000).await;

    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(0),
    )
    .await
    .should_succeed();
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user2,
        Dimensionless::new_percent(0),
    )
    .await
    .should_succeed();
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 2,
                referee: 3,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();
    set_commission_rate_override(&mut suite, contracts.perps, &mut accounts.owner, 1, 100)
        .await
        .should_succeed();
    set_commission_rate_override(&mut suite, contracts.perps, &mut accounts.owner, 2, 45)
        .await
        .should_succeed();

    let user_addrs = [
        accounts.user1.address(),
        accounts.user2.address(),
        accounts.user3.address(),
    ];
    let margin_before = snapshot_margins(&suite, contracts.perps, &user_addrs);

    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user3,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user4, 1).await;

    let margin_after = snapshot_margins(&suite, contracts.perps, &user_addrs);

    let trade_value = UsdValue::new_int(2_000);
    let maker_fee = trade_value
        .checked_mul(Dimensionless::new_raw(500))
        .unwrap();
    let expected_maker_vault_fee = maker_fee;
    let expected_b = expected_maker_vault_fee
        .checked_mul(CommissionRate::new_percent(45))
        .unwrap();
    let expected_a = expected_maker_vault_fee
        .checked_mul(CommissionRate::new_percent(55))
        .unwrap();

    assert_eq!(
        margin_after[0].checked_sub(margin_before[0]).unwrap(),
        expected_a,
        "A (level-2, cr=100%) should receive marginal commission on maker's portion",
    );
    assert_eq!(
        margin_after[1].checked_sub(margin_before[1]).unwrap(),
        expected_b,
        "B (level-1, cr=45%, sr=0) should receive its full level-1 commission",
    );
    assert_eq!(
        margin_before[2].checked_sub(margin_after[2]).unwrap(),
        maker_fee,
        "trader pays the full maker fee with no rebate (sr=0)",
    );
}

/// Direct chain A → trader. A.cr = 100%, A.sr = 0. Trader is the maker on a
/// fill where the maker fee is *negative* (rebate). The trader's vault_fee
/// portion clamps to zero (per the proportional split in submit_order.rs),
/// so A's commission must be zero — and crucially A must NOT be debited.
#[tokio::test]
async fn cr_100_direct_referrer_maker_rebate() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    let pair = pair_id();

    // taker = +3 bps, maker = -1 bps (rebate), protocol = 20%.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: perps::Param {
                    taker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(300),
                        ..Default::default()
                    },
                    maker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(-100),
                        ..Default::default()
                    },
                    protocol_fee_rate: Dimensionless::new_percent(20),
                    referral_active: true,
                    ..default_param()
                },
                pair_params: btree_map! {
                    pair.clone() => default_pair_param(),
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user3, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user4, 100_000).await;

    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(0),
    )
    .await
    .should_succeed();
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 3,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();
    set_commission_rate_override(&mut suite, contracts.perps, &mut accounts.owner, 1, 100)
        .await
        .should_succeed();

    let user_addrs = [accounts.user1.address(), accounts.user3.address()];
    let margin_before = snapshot_margins(&suite, contracts.perps, &user_addrs);

    // Trader (rebating maker) places post-only ask; user4 markets in.
    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user3,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user4, 1).await;

    let margin_after = snapshot_margins(&suite, contracts.perps, &user_addrs);

    // A must NOT be debited despite cr=100% — maker's vault_fee portion is 0
    // (rebater contributes zero weight), so every commission term is 0.
    assert_eq!(
        margin_after[0], margin_before[0],
        "A (cr=100%) must not be debited when trader is a rebating maker",
    );

    // Trader's margin gain = full rebate (no referee_share since vault_fee=0).
    let rebate = UsdValue::new_int(2_000)
        .checked_mul(Dimensionless::new_raw(100))
        .unwrap();
    assert_eq!(
        margin_after[1].checked_sub(margin_before[1]).unwrap(),
        rebate,
        "trader receives the full maker rebate ($0.20)",
    );
}

/// Indirect chain A → B → trader. A.cr = 100%, B.cr = 45%, both sr = 0.
/// Trader is the maker on a rebate fill. Both A and B must NOT be debited.
#[tokio::test]
async fn cr_100_indirect_referrer_maker_rebate() {
    let (mut suite, mut accounts, _, contracts, ..) = setup_test_naive(TestOption::preset_test());

    let pair = pair_id();

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: perps::Param {
                    taker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(300),
                        ..Default::default()
                    },
                    maker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(-100),
                        ..Default::default()
                    },
                    protocol_fee_rate: Dimensionless::new_percent(20),
                    referral_active: true,
                    ..default_param()
                },
                pair_params: btree_map! {
                    pair.clone() => default_pair_param(),
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user1, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user2, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user3, 100_000).await;
    deposit_margin(&mut suite, contracts.perps, &mut accounts.user4, 100_000).await;

    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user1,
        Dimensionless::new_percent(0),
    )
    .await
    .should_succeed();
    set_fee_share_ratio(
        &mut suite,
        contracts.perps,
        &mut accounts.user2,
        Dimensionless::new_percent(0),
    )
    .await
    .should_succeed();
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 2,
                referee: 3,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();
    set_commission_rate_override(&mut suite, contracts.perps, &mut accounts.owner, 1, 100)
        .await
        .should_succeed();
    set_commission_rate_override(&mut suite, contracts.perps, &mut accounts.owner, 2, 45)
        .await
        .should_succeed();

    let user_addrs = [
        accounts.user1.address(),
        accounts.user2.address(),
        accounts.user3.address(),
    ];
    let margin_before = snapshot_margins(&suite, contracts.perps, &user_addrs);

    place_ask_order(
        &mut suite,
        contracts.perps,
        &mut accounts.user3,
        UsdPrice::new_int(2_000),
        1,
    )
    .await;
    place_market_buy(&mut suite, contracts.perps, &mut accounts.user4, 1).await;

    let margin_after = snapshot_margins(&suite, contracts.perps, &user_addrs);

    assert_eq!(
        margin_after[0], margin_before[0],
        "A (level-2, cr=100%) must not be debited on a maker rebate",
    );
    assert_eq!(
        margin_after[1], margin_before[1],
        "B (level-1, cr=45%) must not be debited on a maker rebate",
    );

    let rebate = UsdValue::new_int(2_000)
        .checked_mul(Dimensionless::new_raw(100))
        .unwrap();
    assert_eq!(
        margin_after[2].checked_sub(margin_before[2]).unwrap(),
        rebate,
        "trader receives the full maker rebate ($0.20)",
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn set_fee_share_ratio(
    suite: &mut TestSuiteNaive,
    perps: Addr,
    user: &mut (dyn Signer + Send + Sync),
    ratio: FeeShareRatio,
) -> TxOutcome {
    suite
        .execute(
            user,
            perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetFeeShareRatio {
                share_ratio: ratio,
            }),
            Coins::new(),
        )
        .await
}

async fn set_commission_rate_override(
    suite: &mut TestSuiteNaive,
    perps: Addr,
    owner: &mut (dyn Signer + Send + Sync),
    user: u32,
    rate_percent: i128,
) -> TxOutcome {
    suite
        .execute(
            owner,
            perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetCommissionRateOverride {
                user,
                commission_rate: Op::Insert(CommissionRate::new_percent(rate_percent)),
            }),
            Coins::new(),
        )
        .await
}

fn snapshot_margins(suite: &TestSuiteNaive, perps: Addr, addrs: &[Addr]) -> Vec<UsdValue> {
    addrs
        .iter()
        .map(|addr| {
            suite
                .query_wasm_smart(perps, perps::QueryUserStateRequest { user: *addr })
                .should_succeed()
                .map(|s: perps::UserState| s.margin)
                .unwrap_or(UsdValue::ZERO)
        })
        .collect()
}

async fn register_oracle_prices(
    suite: &mut TestSuiteNaive,
    accounts: &mut dango_testing::TestAccounts,
    eth_price: u128,
) {
    suite
        .seed_oracle_prices(
            &mut accounts.owner,
            btree_map! {
                usdc::DENOM.clone() => OracleTestEntry {
                    pyth_id: 1,
                    humanized_price: UsdPrice::new_int(1),
                },
                pair_id() => OracleTestEntry {
                    pyth_id: 2,
                    humanized_price: UsdPrice::new_int(eth_price as i128),
                },
            },
        )
        .await;
}

async fn deposit_margin(
    suite: &mut TestSuiteNaive,
    perps: Addr,
    user: &mut (dyn Signer + Send + Sync),
    usd_amount: u128,
) {
    let amount = Uint128::new(usd_amount * 1_000_000);
    suite
        .execute(
            user,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), amount).unwrap(),
        )
        .await
        .should_succeed();
}

async fn place_ask_order(
    suite: &mut TestSuiteNaive,
    perps: Addr,
    user: &mut (dyn Signer + Send + Sync),
    price: UsdPrice,
    size: u128,
) {
    suite
        .execute(
            user,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id(),
                size: Quantity::new_int(-(size as i128)),
                kind: OrderKind::Limit {
                    limit_price: price,
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();
}

async fn place_market_buy(
    suite: &mut TestSuiteNaive,
    perps: Addr,
    user: &mut (dyn Signer + Send + Sync),
    size: u128,
) {
    suite
        .execute(
            user,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id(),
                size: Quantity::new_int(size as i128),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();
}

/// Place a limit ask (user1) then a market buy (user2) to produce a fill.
async fn create_perps_fill(
    suite: &mut TestSuiteNaive,
    accounts: &mut dango_testing::TestAccounts,
    perps: Addr,
    price: u128,
    size: u128,
) {
    place_ask_order(
        suite,
        perps,
        &mut accounts.user1,
        UsdPrice::new_int(price as i128),
        size,
    )
    .await;
    place_market_buy(suite, perps, &mut accounts.user2, size).await;
}

async fn place_market_buy_with_events(
    suite: &mut TestSuiteNaive,
    perps: Addr,
    user: &mut (dyn Signer + Send + Sync),
    size: u128,
) -> TxEvents {
    suite
        .execute(
            user,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id(),
                size: Quantity::new_int(size as i128),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events
}

fn query_referral_settings(
    suite: &TestSuiteNaive,
    perps: Addr,
    user: u32,
) -> Option<ReferrerSettings> {
    suite
        .query_wasm_smart(perps, perps::QueryReferralSettingsRequest { user })
        .should_succeed()
}

fn query_referral_data(
    suite: &TestSuiteNaive,
    perps: Addr,
    user: u32,
    since: Option<Timestamp>,
) -> UserReferralData {
    suite
        .query_wasm_smart(perps, perps::QueryReferralDataRequest { user, since })
        .should_succeed()
}
