use {
    dango_genesis::{Contracts, GatewayOption, GenesisOption},
    dango_hyperlane_types::{Addr32, isms},
    dango_math::{MathError, NumberConst, Udec128, Uint128},
    dango_primitives::{
        Addr, Addressable, CheckedContractEvent, Coin, Coins, Duration, JsonDeExt, Op, QuerierExt,
        ResultExt, SearchEvent, btree_map, btree_set, coins,
    },
    dango_testing::{
        BalanceChange, DefaultHyperlaneTestSuite, HyperlaneTestSuite, MockValidatorSet, Preset,
        TestAccounts, TestOption, TestSuite, mock_arbitrum, mock_ethereum, setup_test,
        setup_test_naive_with_custom_genesis,
    },
    dango_types::{
        constants::{dango, usdc},
        gateway::{
            self, Origin, RateLimit, Remote, SetPersonalQuotaRequest, WithdrawalFee,
            WithdrawalResponse, WithdrawalStatus,
        },
    },
};

/// USDC withdrawal fee charged on the Arbitrum Warp route. This mirrors the
/// value configured in the test genesis at `dango/testing/src/genesis.rs:336`;
/// keep the two in sync.
const ARBITRUM_USDC_WITHDRAWAL_FEE: u128 = 10_000;

#[tokio::test]
async fn rate_limit() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    suite.balances().record(receiver);

    // Receive some tokens.
    // eth_usdc => 100
    // sol_usdc => 200
    {
        for (domain, origin_warp, amount) in [
            (mock_ethereum::DOMAIN, mock_ethereum::USDC_WARP, 100_000_000),
            (mock_arbitrum::DOMAIN, mock_arbitrum::USDC_WARP, 200_000_000),
        ] {
            suite
                .receive_warp_transfer(relayer, domain, origin_warp, receiver, amount)
                .await
                .should_succeed();
        }

        // Check balances.
        suite.balances().should_change(
            receiver,
            btree_map! {
                usdc::DENOM.clone() => BalanceChange::Increased(300_000_000),
            },
        );
    }

    suite
        .query_supply(usdc::DENOM.clone())
        .should_succeed_and_equal(300_000_000.into());

    // Total supply = 300 usdc
    // Set rate limit.
    // alloy_usdc => 10%
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Make 1 day pass letting the cron job to reset the rate limits.
    advance_to_next_day(&mut suite).await;

    // Try send back exact tokens to don't trigger rate limit.
    // Current limit = 10% of 300 = 30
    // alloy_usdc => 300 * 0.1 = 30
    // Send 30 alloy_usdc back to arbitrum.

    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            gateway::Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                30_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // Trigger the rate limit sending 1 more token.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            gateway::Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota! denom: bridge/usdc, requested: 1, residue after personal quota: 1");

    // Inflows must no longer replenish the outbound quota. Receive 100M more
    // USDC from Ethereum; the quota should stay at zero.
    suite
        .receive_warp_transfer(
            relayer,
            mock_ethereum::DOMAIN,
            mock_ethereum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    // Supply is now 300M + 100M = 400M minus the 30M already sent back to
    // arbitrum = 370M. Receiver holds everything except the 10_000 fee paid.
    {
        suite
            .query_supply(usdc::DENOM.clone())
            .should_succeed_and_equal(370_000_000.into());

        suite.balances().should_change(
            receiver,
            btree_map! {
                usdc::DENOM.clone() => BalanceChange::Increased(369_990_000),
            },
        );
    }

    // Quota was not bumped by the inbound transfer — sending even 1 token
    // fails with the same error as before.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            gateway::Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota! denom: bridge/usdc, requested: 1, residue after personal quota: 1");

    // Advance one day so the cron seeds a fresh quota of 10% × 370M = 37M.
    advance_to_next_day(&mut suite).await;

    // Reserves: ethereum received 100M twice (no outflow) → 200M.
    //           arbitrum received 200M, sent 30M back → 170M.
    for (remote, amount) in [
        (
            Remote::Warp {
                domain: mock_ethereum::DOMAIN,
                contract: mock_ethereum::USDC_WARP,
            },
            200_000_000,
        ),
        (
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            170_000_000,
        ),
    ] {
        suite
            .query_wasm_smart(
                contracts.gateway,
                gateway::QueryReserveRequest {
                    bridge: contracts.warp,
                    remote,
                },
            )
            .should_succeed_and_equal(amount.into());
    }

    // Drain the full 37M quota to arbitrum.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            gateway::Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                37_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // One more token fails — quota is depleted and inflow can't refill it.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            gateway::Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota! denom: bridge/usdc, requested: 1, residue after personal quota: 1");

    // Raise the rate limit to 99%. In phase 1 this still only takes effect
    // after the next cron tick.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(99)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Another day. Supply is 370M - 37M = 333M; quota is 333M × 99%.
    advance_to_next_day(&mut suite).await;

    // Arbitrum reserve after the previous 37M withdraw is 170M - 37M = 133M.
    // Drain it completely in a single transfer (well under the new quota).
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            gateway::Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                133_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // Arbitrum reserve is now empty; the next transfer fails on reserve, not
    // quota.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            gateway::Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient reserve!");
}

#[tokio::test]
async fn boundary_attack() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // Mint 300M USDC into the chain via arbitrum so the receiver has 300M to
    // send back over the same route, and the arbitrum reserve can cover up to
    // 300M of outbound transfers.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            300_000_000,
        )
        .await
        .should_succeed();

    suite
        .query_supply(usdc::DENOM.clone())
        .should_succeed_and_equal(300_000_000.into());

    // Configure a 10% per-day rate limit. Cap = 30M against the 300M supply.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Cron tick: seeds the outbound quota at 30M.
    advance_to_next_day(&mut suite).await;

    // Advance to one minute before the next cron tick.
    advance_by(
        &mut suite,
        Duration::from_hours(23) + Duration::from_minutes(59),
    )
    .await;

    // Drain 25M, well under the 30M window cap.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            gateway::Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                25_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // Two minutes pass, crossing the cron tick. Supply is now 275M; cron
    // reseeds the cap to 10% × 275M = 27.5M.
    advance_by(&mut suite, Duration::from_minutes(2)).await;

    // Drain another 25M immediately after the cron tick. The trailing-24h
    // sum (25M from one minute earlier) plus the new 25M is 50M, exceeding
    // the 27.5M cap, so this is rejected.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            gateway::Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 25_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota! denom: bridge/usdc, requested: 25000000, residue after personal quota: 25000000, rolling sum: 25000000, cap: 27500000");

    // Wait until the first drain falls outside the trailing 24h window. The
    // bucket from `t = 1d 23h59m` rolls out at `t = 2d 23h59m`; advance one
    // additional day (well past that boundary, also crossing another cron
    // tick) and confirm a 25M drain succeeds again.
    advance_by(&mut suite, Duration::from_hours(24)).await;

    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            gateway::Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                25_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();
}

#[tokio::test]
async fn native_denom() {
    let (mut suite, mut accounts, _, contracts, mut valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    let remote_domain = 123;
    let remote_warp = Addr::mock(123);

    // Register a native denom in the gateway
    {
        suite
            .execute(
                &mut accounts.owner,
                contracts.gateway,
                &gateway::ExecuteMsg::SetRoutes(btree_set! {
                    (
                        Origin::Local(dango::DENOM.clone()),
                        contracts.warp,
                        Remote::Warp {
                            domain: remote_domain,
                            contract: remote_warp.into(),
                        },
                    ),
                }),
                Coins::default(),
            )
            .await
            .should_succeed();
    }

    // Register the validator set for the remote domain.
    {
        let validator_set = MockValidatorSet::new_preset(remote_domain, false);

        suite
            .execute(
                &mut accounts.owner,
                contracts.hyperlane.ism,
                &isms::multisig::ExecuteMsg::SetValidators {
                    domain: remote_domain,
                    threshold: 2,
                    validators: validator_set.validator_addresses(),
                },
                Coins::default(),
            )
            .await
            .should_succeed();

        valset.insert(remote_domain, validator_set);
    }

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    // Try receive a warp transfer.
    // This should fail because the gateway should not have any reserve for the native denom.
    {
        suite
            .receive_warp_transfer(
                &mut accounts.user3,
                remote_domain,
                remote_warp.into(),
                &accounts.user2,
                100,
            )
            .await
            .should_fail_with_error(MathError::overflow_sub(0_u128, 100_u128));
    }

    suite
        .balances()
        .record_many([&accounts.user1, &accounts.user2]);

    // Send some tokens with user1 to the remote domain.
    {
        suite
            .transfer_remote(
                &mut accounts.user1,
                &mut accounts.owner,
                contracts.gateway,
                Remote::Warp {
                    domain: remote_domain,
                    contract: remote_warp.into(),
                },
                Addr::mock(124).into(),
                coins! { dango::DENOM.clone() => 100 },
            )
            .await
            .should_succeed();
    }

    // Try to receive the tokens back to user2.
    {
        suite
            .receive_warp_transfer(
                &mut accounts.user3,
                remote_domain,
                remote_warp.into(),
                &accounts.user2,
                100,
            )
            .await
            .should_succeed();
    }

    // check the balances.
    suite.balances().should_change(
        &accounts.user1,
        btree_map! {
            dango::DENOM.clone() => BalanceChange::Decreased(100),
        },
    );

    suite.balances().should_change(
        &accounts.user2,
        btree_map! {
            dango::DENOM.clone() => BalanceChange::Increased(100),
        },
    );
}

#[tokio::test]
async fn deposit_and_withdraw_events() {
    let (suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let remote = Remote::Warp {
        domain: mock_arbitrum::DOMAIN,
        contract: mock_arbitrum::USDC_WARP,
    };

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // Receiving a warp transfer should emit a `deposited` event from the
    // gateway.
    {
        let (_, outcome) = suite
            .receive_warp_transfer_with_outcome(
                &mut accounts.user1,
                mock_arbitrum::DOMAIN,
                mock_arbitrum::USDC_WARP,
                &accounts.user2,
                100_000_000_u128,
            )
            .await
            .unwrap();

        let deposits = outcome
            .events
            .search_event::<CheckedContractEvent>()
            .with_predicate(move |e| e.contract == contracts.gateway && e.ty == "deposited")
            .take()
            .all()
            .into_iter()
            .map(|e| {
                e.event
                    .data
                    .deserialize_json::<gateway::Deposited>()
                    .unwrap()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            deposits,
            vec![gateway::Deposited {
                user: accounts.user2.address(),
                bridge: contracts.warp,
                remote,
                denom: usdc::DENOM.clone(),
                amount: Uint128::new(100_000_000),
            }]
        );
    }

    // Sending a remote transfer should emit a `withdrawn` event from the
    // gateway, reporting the bridged amount net of the withdrawal fee.
    {
        let events = suite
            .transfer_remote(
                &mut accounts.user2,
                &mut accounts.owner,
                contracts.gateway,
                remote,
                mock_arbitrum_recipient,
                Coin::new(
                    usdc::DENOM.clone(),
                    50_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
                )
                .unwrap(),
            )
            .await
            .should_succeed()
            .events;

        let withdrawals = events
            .search_event::<CheckedContractEvent>()
            .with_predicate(move |e| e.contract == contracts.gateway && e.ty == "withdrawn")
            .take()
            .all()
            .into_iter()
            .map(|e| {
                e.event
                    .data
                    .deserialize_json::<gateway::Withdrawn>()
                    .unwrap()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            withdrawals,
            vec![gateway::Withdrawn {
                id: 0,
                user: accounts.user2.address(),
                bridge: contracts.warp,
                remote,
                recipient: mock_arbitrum_recipient,
                denom: usdc::DENOM.clone(),
                amount: Uint128::new(50_000_000),
                fee: Uint128::new(ARBITRUM_USDC_WITHDRAWAL_FEE),
            }]
        );
    }
}

#[tokio::test]
async fn set_rate_limits_resets_quota() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // Receive 100M USDC from arbitrum so we have reserves + supply to work with.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    // Set a 10% rate limit. Supply is 100M, so the quota should be seeded to
    // 10M immediately — no cron tick needed.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Drain the full 10M quota.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                10_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // Next token fails — the quota is now exhausted.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota! denom: bridge/usdc, requested: 1, residue after personal quota: 1");

    // Owner raises the rate limit to 50%. The change takes effect immediately
    // — the cap is `supply_snapshot × limit`, and only `limit` moved. The
    // snapshot (still 100M from the initial seed) yields cap = 50M. With 10M
    // already drained, 40M of headroom is now available in the same block.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(50)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // 1 more token now succeeds — admin's raise is honored immediately.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_succeed();

    // Advance one day so the cron fires and re-snapshots supply (~90M after
    // the 10M+1 drained). New cap = 90M × 50% = 45M; rolling sum reset to 0.
    advance_to_next_day(&mut suite).await;

    // A further 1-unit withdraw still succeeds — fresh headroom after the cron.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_succeed();

    // Removing USDC from the rate limits map should drop its cap entry. A
    // transfer far above the old 50% cap should now succeed — reserves are
    // the only remaining constraint.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {}),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                50_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // Advance a day so the cron fires. The cron iterates RATE_LIMITS, which
    // no longer contains USDC, so no snapshot should be resurrected for it.
    // A subsequent large transfer must still succeed — reserves remain the
    // only constraint.
    advance_to_next_day(&mut suite).await;

    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                20_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();
}

/// `SetRateLimits` takes effect on the configured limit immediately, but
/// never refreshes the supply snapshot on a denom that is already tracked —
/// the snapshot only moves at cron ticks. Covers the no-op, lower, and raise
/// cases plus the cron-driven refresh.
#[tokio::test]
async fn set_rate_limits_does_not_refresh_supply_snapshot() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // Setup: 100M USDC supply, reserve 100M on arbitrum.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    // Seed a 50% rate limit. Snapshot = 100M, cap = 50M.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(50)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Drain 30M → supply 70M, rolling sum 30M. Snapshot remains 100M.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                30_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // Case 1: re-set the same limit. Snapshot is preserved (50M cap holds),
    // so headroom is 50M − 30M = 20M. 20M − 1 must succeed; 1 more must fail.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(50)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                19_999_999 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 2 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota!");

    // Case 2: lower the limit to 10%. Cap = 100M × 10% = 10M (snapshot still
    // 100M). Rolling sum is 49_999_999, already over the new 10M cap, so any
    // further withdraw fails.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota!");

    // Case 3: raise the limit to 80%. Cap = 100M × 80% = 80M (snapshot still
    // 100M, deposits between cron ticks do NOT enlarge it). Rolling sum is
    // 49_999_999, so headroom is ~30M and a 30M transfer must succeed.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(80)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                30_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // Case 4: cron tick re-snapshots supply. Supply is now ~20M after total
    // ~80M drained; rolling sum carries through the 24h boundary in the
    // baseline calculation, so the new headroom comes from the fresh snapshot.
    advance_to_next_day(&mut suite).await;

    // A withdraw that would have failed pre-cron (cap was 80M, rolling sum
    // ~80M) now sees a smaller cap from the fresh snapshot. Any meaningful
    // amount above the fresh cap still fails; tiny amount succeeds against the
    // fresh headroom.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_succeed();
}

#[tokio::test]
async fn personal_quota() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver_addr = accounts.user2.address();
    let owner_addr = accounts.owner.address();
    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // Seed 200M USDC from arbitrum so there's both stock and a non-trivial
    // reserve for withdrawals to hit.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            200_000_000,
        )
        .await
        .should_succeed();

    // Tight 1% rate limit. Supply is 200M → global quota = 2M.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(1)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // ---- Auth ----
    suite
        .execute(
            receiver,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(10_000_000),
                    available_for: None,
                }),
            },
            Coins::default(),
        )
        .await
        .should_fail_with_error("only the owner can set personal quotas");

    // ---- Overwrite + query ----
    // Initial 100M with a 1h lifetime.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(100_000_000),
                    available_for: Some(Duration::from_hours(1)),
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    // Overwrite with a smaller, permanent allowance.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(50_000_000),
                    available_for: None,
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    let pq = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("entry present");
    assert_eq!(pq.amount, Uint128::new(50_000_000));
    assert_eq!(pq.expire_at, None);
    assert_eq!(pq.granted_by, owner_addr);
    // Captured so the next assertion can check that partial consumption
    // preserves the grant's `granted_at`.
    let granted_at_after_overwrite = pq.granted_at;

    // ---- Consumption fully within personal quota ----
    // Personal = 50M, global = 2M. Withdraw 40M; only personal is touched.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                40_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    let pq = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("entry present");
    assert_eq!(pq.amount, Uint128::new(10_000_000));
    assert_eq!(pq.expire_at, None);
    assert_eq!(pq.granted_by, owner_addr);
    // Partial consumption must preserve `granted_at` from the previous grant
    // — it's an audit field, not an activity timestamp.
    assert_eq!(pq.granted_at, granted_at_after_overwrite);

    // ---- Overflow into global quota ----
    // Withdraw 12M; personal (10M) is fully consumed, global absorbs 2M.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                12_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // Fully consumed personal quotas are removed from storage.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed_and_equal(None);

    // Global is now depleted. The error mentions the remainder after any
    // personal quota would have been consumed — here that is the full amount.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota! denom: bridge/usdc, requested: 1, residue after personal quota: 1");

    // ---- Expired personal quota is ignored ----
    // Grant 100M more, this time with a 1h lifetime.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(100_000_000),
                    available_for: Some(Duration::from_hours(1)),
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    // 2h later — under the 24h cron interval, so the global quota stays at 0.
    advance_by(&mut suite, Duration::from_hours(2)).await;

    // The personal quota is now expired and must be skipped. Withdrawing 1
    // token falls through to the global quota (still 0) and fails.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota! denom: bridge/usdc, requested: 1, residue after personal quota: 1");

    // The expired entry is left in storage; the handler doesn't scrub it. The
    // caller can still query it to reason about `expire_at`.
    let stored = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed();
    assert_eq!(
        stored.as_ref().map(|q| q.amount),
        Some(Uint128::new(100_000_000))
    );
    assert!(stored.and_then(|q| q.expire_at).is_some());

    // ---- Pagination query ----
    let mut page = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotasRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed();
    assert_eq!(page.len(), 1);
    let entry = page.pop().unwrap();
    assert_eq!(entry.user, receiver_addr);
    assert_eq!(entry.denom, usdc::DENOM.clone());
    assert_eq!(entry.quota.amount, Uint128::new(100_000_000));
}

/// `Op::Delete` must remove the personal quota entry outright — not just
/// flip its amount to zero — so subsequent withdrawals see no personal
/// allowance at all and fall straight to the global quota.
#[tokio::test]
async fn personal_quota_revoke_via_op_delete() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver_addr = accounts.user2.address();
    let owner_addr = accounts.owner.address();
    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // 100M supply. 10% rate limit → global quota = 10M.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Grant a 50M personal allowance.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(50_000_000),
                    available_for: None,
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    let pq = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("entry present");
    assert_eq!(pq.amount, Uint128::new(50_000_000));
    assert_eq!(pq.expire_at, None);
    assert_eq!(pq.granted_by, owner_addr);

    // Revoke via `Op::Delete`.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Delete,
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    // Entry is gone — not just zeroed.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed_and_equal(None);

    // Try to withdraw 20M — above the 10M global quota. Pre-revocation the
    // personal quota would have covered it; post-revocation the transfer
    // must fall straight to the global quota and fail. The error message's
    // `remaining` equals the full request because no personal quota was
    // consumed.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 20_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error(
            "insufficient outbound quota! denom: bridge/usdc, requested: 20000000, residue after personal quota: 20000000",
        );

    // The 10M global quota still applies — 10M succeeds, 10M + 1 fails.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                10_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();
}

/// A 0% rate limit is a hard freeze: the cap goes to zero AND every
/// outstanding personal quota for that denom is revoked, so a granted user
/// can't keep withdrawing through their per-account allowance.
#[tokio::test]
async fn zero_rate_limit_revokes_personal_quotas() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // 100M supply, 10% rate limit → cap 10M.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Grant the receiver a 5M personal quota for USDC.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver.address(),
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(5_000_000),
                    available_for: None,
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    // Confirm the quota is in place.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver.address(),
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed_and(|q| q.is_some());

    // Owner sets the USDC rate limit to 0 — a hard freeze.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::ZERO),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // The personal quota must have been wiped by the freeze.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver.address(),
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed_and_equal(None);

    // A 1-unit withdraw fails — no personal quota left and the global cap is 0.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota!");
}

/// Granting a personal quota for a denom that is NOT rate-limited globally
/// must still behave correctly: the personal allowance is consumed first,
/// and any overflow falls through to an absent global entry (which means
/// unrestricted, not "blocked").
#[tokio::test]
async fn personal_quota_on_un_rate_limited_denom() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver_addr = accounts.user2.address();
    let owner_addr = accounts.owner.address();
    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // Seed 100M reserve + supply. No SetRateLimits call anywhere — USDC is
    // not globally rate-limited.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    // Grant a 50M personal allowance.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(50_000_000),
                    available_for: None,
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    // Consume 30M — fully from the personal quota.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                30_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    let pq = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("entry present");
    assert_eq!(pq.amount, Uint128::new(20_000_000));
    assert_eq!(pq.expire_at, None);
    assert_eq!(pq.granted_by, owner_addr);

    // Withdraw 50M: 20M from personal (fully consumed), 30M falls through
    // to the global quota. Because the denom is not in RATE_LIMITS, the
    // fall-through finds no entry and the transfer succeeds unrestricted.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                50_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // Personal quota is now fully consumed and removed from storage.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed_and_equal(None);

    // Without any personal or global restriction, a further transfer just
    // works. Reserves are the only remaining constraint.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                10_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();
}

/// Overwriting a partially-consumed personal quota must replace the
/// record wholesale — no carry-over of the leftover balance, no
/// preservation of the old expiry. The stored amount and expiry reflect
/// the admin's most recent decision.
#[tokio::test]
async fn personal_quota_mid_consumption_overwrite() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver_addr = accounts.user2.address();
    let owner_addr = accounts.owner.address();
    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // Reserve / supply seed.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            200_000_000,
        )
        .await
        .should_succeed();

    // Tight global rate limit (1%) so the test leans on the personal quota.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(1)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Grant 100M with no expiry.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(100_000_000),
                    available_for: None,
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    // Consume 40M — fully within personal. 60M remains.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                40_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    let pq = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("entry present");
    assert_eq!(pq.amount, Uint128::new(60_000_000));
    assert_eq!(pq.expire_at, None);
    assert_eq!(pq.granted_by, owner_addr);
    let granted_at_before_overwrite = pq.granted_at;

    // Overwrite: admin replaces with 10M + 1h expiry. The 60M leftover is
    // discarded; the expiry is newly computed from the current block time.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(10_000_000),
                    available_for: Some(Duration::from_hours(1)),
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    let stored = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("entry present after overwrite");

    // Amount is the new 10M — no carry-over.
    assert_eq!(stored.amount, Uint128::new(10_000_000));
    // Expiry is Some — the new lifetime replaced the previous `None`.
    assert!(stored.expire_at.is_some());
    // Overwrites must reset `granted_at` — the record should reflect the
    // latest admin decision, not the first grant.
    assert!(stored.granted_at >= granted_at_before_overwrite);
    assert_eq!(stored.granted_by, owner_addr);
}

/// Paginated queries must return entries in ascending `(Addr, Denom)`
/// order and the `start_after` bound must correctly skip past the end of
/// the previous page.
#[tokio::test]
async fn personal_quotas_pagination() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let user_a = accounts.user1.address();
    let user_b = accounts.user2.address();
    let owner = &mut accounts.owner;

    // Grant four quotas across two users × two denoms.
    for (user, denom) in [
        (user_a, usdc::DENOM.clone()),
        (user_a, dango::DENOM.clone()),
        (user_b, usdc::DENOM.clone()),
        (user_b, dango::DENOM.clone()),
    ] {
        suite
            .execute(
                owner,
                contracts.gateway,
                &gateway::ExecuteMsg::SetPersonalQuota {
                    user,
                    denom,
                    quota: Op::Insert(SetPersonalQuotaRequest {
                        amount: Uint128::new(1_000_000),
                        available_for: None,
                    }),
                },
                Coins::default(),
            )
            .await
            .should_succeed();
    }

    // First page, limit 2.
    let page1 = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotasRequest {
                start_after: None,
                limit: Some(2),
            },
        )
        .should_succeed();
    assert_eq!(page1.len(), 2);

    // Second page picks up after the last entry of page 1.
    let last = page1.last().expect("page 1 non-empty");
    let page2 = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotasRequest {
                start_after: Some((last.user, last.denom.clone())),
                limit: Some(2),
            },
        )
        .should_succeed();
    assert_eq!(page2.len(), 2);

    // No entry overlap between pages.
    let page1_keys: std::collections::BTreeSet<_> =
        page1.iter().map(|e| (e.user, e.denom.clone())).collect();
    let page2_keys: std::collections::BTreeSet<_> =
        page2.iter().map(|e| (e.user, e.denom.clone())).collect();
    assert!(page1_keys.is_disjoint(&page2_keys));

    // Combined, the two pages cover exactly all four grants.
    let combined: std::collections::BTreeSet<_> = page1_keys.union(&page2_keys).cloned().collect();
    let expected: std::collections::BTreeSet<_> = [
        (user_a, usdc::DENOM.clone()),
        (user_a, dango::DENOM.clone()),
        (user_b, usdc::DENOM.clone()),
        (user_b, dango::DENOM.clone()),
    ]
    .into_iter()
    .collect();
    assert_eq!(combined, expected);

    // Each page is internally sorted ascending.
    assert!(
        page1
            .windows(2)
            .all(|w| (w[0].user, &w[0].denom) <= (w[1].user, &w[1].denom))
    );
    assert!(
        page2
            .windows(2)
            .all(|w| (w[0].user, &w[0].denom) <= (w[1].user, &w[1].denom))
    );

    // And the boundary between pages is also ascending.
    let last_p1 = page1.last().unwrap();
    let first_p2 = page2.first().unwrap();
    assert!((last_p1.user, &last_p1.denom) < (first_p2.user, &first_p2.denom));

    // Querying beyond the end yields an empty page.
    let last_p2 = page2.last().unwrap();
    let page3 = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotasRequest {
                start_after: Some((last_p2.user, last_p2.denom.clone())),
                limit: Some(2),
            },
        )
        .should_succeed();
    assert!(page3.is_empty());
}

/// The `is_none_or(|t| block.timestamp < t)` predicate is strict. Cover
/// both sides of the boundary: at exactly `block.timestamp == expire_at`
/// the quota is already expired; 1ns before that it is still active.
#[tokio::test]
async fn personal_quota_expire_at_boundary() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver_addr = accounts.user2.address();
    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // 100M supply; leave USDC un-rate-limited so that the transfer's quota
    // path depends only on the personal allowance.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    // ---- Active: 1ns before expiry ----
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(10_000_000),
                    available_for: Some(Duration::from_hours(1)),
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    // Advance to 1ns before the expiry. The predicate `now < expire_at` is
    // still true, so the personal quota is active.
    advance_by(
        &mut suite,
        Duration::from_hours(1) - Duration::from_nanos(1),
    )
    .await;

    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_succeed();

    // The active path consumed 1 token from the personal quota.
    let pq = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("entry still present");
    assert_eq!(pq.amount, Uint128::new(9_999_999));

    // ---- Expired: at exactly the boundary ----
    // Re-grant a fresh 10M with another 1h lifetime. `expire_at` is now
    // `current_block_time + 1h`; advancing by exactly 1h puts us right on
    // the boundary.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(10_000_000),
                    available_for: Some(Duration::from_hours(1)),
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    advance_by(&mut suite, Duration::from_hours(1)).await;

    // The transfer should succeed (the denom is un-rate-limited), but the
    // personal quota must NOT be consumed — the predicate treats
    // `now == expire_at` as expired.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_succeed();

    let pq = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("expired entry is left in storage untouched");
    assert_eq!(pq.amount, Uint128::new(10_000_000));
}

/// After a personal quota has expired (but before any consumption has
/// scrubbed it), re-granting must replace the stale entry cleanly —
/// fresh amount, fresh expire_at, fresh granted_at. No carry-over of the
/// old expired record.
#[tokio::test]
async fn personal_quota_regrant_after_expiry() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver_addr = accounts.user2.address();
    let owner_addr = accounts.owner.address();
    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // Reserve + supply seed.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    // Grant 10M with a 1h lifetime.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(10_000_000),
                    available_for: Some(Duration::from_hours(1)),
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    let pq_before = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("entry present");
    let granted_at_before = pq_before.granted_at;

    // Advance 2h so the entry is expired but has not been scrubbed by any
    // transfer attempt.
    advance_by(&mut suite, Duration::from_hours(2)).await;

    // Re-grant a fresh 20M with a new 1h lifetime. Under no carry-over, the
    // old expired record is replaced wholesale.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(20_000_000),
                    available_for: Some(Duration::from_hours(1)),
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    let pq_after = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("entry present");

    assert_eq!(pq_after.amount, Uint128::new(20_000_000));
    assert!(pq_after.expire_at.is_some());
    // expire_at was recomputed from the new block time, so it is strictly
    // later than the original expiry.
    assert!(pq_after.expire_at > pq_before.expire_at);
    assert_eq!(pq_after.granted_by, owner_addr);
    // granted_at was reset to the new block time, strictly later than the
    // original grant.
    assert!(pq_after.granted_at > granted_at_before);

    // The new allowance is actually usable — consume 1M and check the stored
    // amount drops to 19M (it's the new quota that's being consumed, not a
    // phantom merger with the old).
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                1_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    let pq_consumed = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("entry present");
    assert_eq!(pq_consumed.amount, Uint128::new(19_000_000));
}

/// The cron only touches OUTBOUND_QUOTAS — it must never scrub
/// PERSONAL_QUOTAS, even if the entry is already expired. The expired
/// record should survive unchanged until the admin explicitly overwrites
/// or deletes it, or the user triggers consumption.
#[tokio::test]
async fn personal_quota_cron_tick_does_not_scrub_expired_entry() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver_addr = accounts.user2.address();
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    // Reserve + supply seed. (No mutable ref to `receiver` needed — the
    // test never transfers; it just grants and advances.)
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            &accounts.user2,
            100_000_000,
        )
        .await
        .should_succeed();

    // Global rate limit so cron_execute has something to reseed. This
    // isolates the cron's effect on OUTBOUND_QUOTAS from its effect on
    // PERSONAL_QUOTAS (which must be nil).
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Grant a 1h personal allowance.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(10_000_000),
                    available_for: Some(Duration::from_hours(1)),
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    let pq_before_cron = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("entry present");

    // Advance a full day. The personal quota expired 23h ago at this point.
    // The cron has fired at least once during this advance (24h tick).
    advance_to_next_day(&mut suite).await;

    let pq_after_cron = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: receiver_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("expired entry is preserved across cron");

    // Every field of the expired record is untouched by cron.
    assert_eq!(pq_after_cron.amount, pq_before_cron.amount);
    assert_eq!(pq_after_cron.expire_at, pq_before_cron.expire_at);
    assert_eq!(pq_after_cron.granted_by, pq_before_cron.granted_by);
    assert_eq!(pq_after_cron.granted_at, pq_before_cron.granted_at);
}

/// Drains spread across the trailing window all count against the cap, but
/// each one falls out 24h after it was made.
#[tokio::test]
async fn rolling_window_releases_gradually() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // 200M USDC supply.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            200_000_000,
        )
        .await
        .should_succeed();

    // 5% rate limit. Cap = 10M.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(5)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Drain the full 10M cap immediately.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                10_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // One minute before the 24h boundary, the drain is still in the window.
    advance_by(
        &mut suite,
        Duration::from_hours(23) + Duration::from_minutes(59),
    )
    .await;

    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc::DENOM.clone(), 1 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota!");

    // Cross 24h since the drain (and the cron tick at 1d). The original
    // entry has rolled out; the cron has reseeded the cap to 9.5M (190M ×
    // 5%). A fresh full-cap drain succeeds.
    advance_by(&mut suite, Duration::from_minutes(2)).await;

    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                9_500_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();
}

/// The cap is snapshotted by cron once per refresh period — supply changes
/// between cron ticks (deposits, etc.) do not enlarge the headroom.
#[tokio::test]
async fn cap_is_snapshotted_at_cron_tick() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let usdc_denom = usdc::DENOM.clone();

    // Receive 100M USDC. supply = 100M.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    // 10% rate limit. Snapshot seeded at supply = 100M, so cap = 10M.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc_denom.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    let initial = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRateLimitStatusRequest {
                denom: usdc_denom.clone(),
            },
        )
        .should_succeed()
        .expect("rate-limited");
    assert_eq!(initial.cap, Uint128::new(10_000_000));
    assert_eq!(initial.used_in_last_24h, Uint128::ZERO);

    // Receive another 100M. supply = 200M, but cap stays at 10M until cron.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    let mid = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRateLimitStatusRequest {
                denom: usdc_denom.clone(),
            },
        )
        .should_succeed()
        .expect("rate-limited");
    assert_eq!(mid.cap, Uint128::new(10_000_000));
    assert_eq!(mid.used_in_last_24h, Uint128::ZERO);

    // Cron tick reseeds the cap to 200M × 10% = 20M.
    advance_to_next_day(&mut suite).await;

    let after_cron = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRateLimitStatusRequest { denom: usdc_denom },
        )
        .should_succeed()
        .expect("rate-limited");
    assert_eq!(after_cron.cap, Uint128::new(20_000_000));
}

/// A withdraw fully covered by personal quota does not consume the trailing
/// rolling window — the global cap stays available for other withdraws.
#[tokio::test]
async fn personal_quota_does_not_consume_rolling_window() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver_addr = accounts.user2.address();
    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();
    let usdc_denom = usdc::DENOM.clone();

    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    // 1% rate limit. Cap = 1M.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc_denom.clone() => RateLimit::new_unchecked(Udec128::new_percent(1)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Grant a 50M personal quota — large enough to fully cover the test
    // withdraw without spilling into the global cap.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: receiver_addr,
                denom: usdc_denom.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(50_000_000),
                    available_for: None,
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    // Withdraw 40M, fully within the personal allowance.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(
                usdc_denom.clone(),
                40_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    // The trailing-window sum is still zero.
    let q = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRateLimitStatusRequest { denom: usdc_denom },
        )
        .should_succeed()
        .expect("rate-limited");
    assert_eq!(q.used_in_last_24h, Uint128::ZERO);
    assert_eq!(q.cap, Uint128::new(1_000_000));
}

/// Removing a denom from the rate-limit map clears its trailing-window
/// state. Re-adding it later starts with a fresh rolling sum.
#[tokio::test]
async fn denom_removal_clears_withdraw_volumes() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();
    let usdc_denom = usdc::DENOM.clone();

    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc_denom.clone() => RateLimit::new_unchecked(Udec128::new_percent(20)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Drain 5M — rolling sum is now 5M.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc_denom.clone(), 5_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_succeed();

    let after_drain = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRateLimitStatusRequest {
                denom: usdc_denom.clone(),
            },
        )
        .should_succeed()
        .expect("rate-limited");
    assert_eq!(after_drain.used_in_last_24h, Uint128::new(5_000_000));

    // Drop USDC from the rate-limit map. The cap entry and rolling-window
    // history should both go away.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {}),
            Coins::default(),
        )
        .await
        .should_succeed();

    let unlimited = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRateLimitStatusRequest {
                denom: usdc_denom.clone(),
            },
        )
        .should_succeed();
    assert!(unlimited.is_none());

    // Re-add USDC at a fresh rate. supply has dropped to 95M, cap = 19M.
    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc_denom.clone() => RateLimit::new_unchecked(Udec128::new_percent(20)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    let reseeded = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRateLimitStatusRequest { denom: usdc_denom },
        )
        .should_succeed()
        .expect("rate-limited");
    assert_eq!(reseeded.cap, Uint128::new(19_000_000));
    assert_eq!(reseeded.used_in_last_24h, Uint128::ZERO);
}

/// Exercise the `RateLimitStatus` and paginated `RateLimitStatuses` queries.
#[tokio::test]
async fn query_rate_limit_status() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;
    let owner = &mut accounts.owner;

    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();
    let usdc_denom = usdc::DENOM.clone();

    // Un-rate-limited denom returns None.
    let none = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRateLimitStatusRequest {
                denom: usdc_denom.clone(),
            },
        )
        .should_succeed();
    assert!(none.is_none());

    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    suite
        .execute(
            owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc_denom.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Drain 3M to leave a non-trivial rolling sum.
    suite
        .transfer_remote(
            receiver,
            owner,
            contracts.gateway,
            Remote::Warp {
                domain: mock_arbitrum::DOMAIN,
                contract: mock_arbitrum::USDC_WARP,
            },
            mock_arbitrum_recipient,
            Coin::new(usdc_denom.clone(), 3_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_succeed();

    // Single-denom query.
    let single = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRateLimitStatusRequest {
                denom: usdc_denom.clone(),
            },
        )
        .should_succeed()
        .expect("rate-limited");
    assert_eq!(single.supply_snapshot, Uint128::new(100_000_000));
    assert_eq!(single.cap, Uint128::new(10_000_000));
    assert_eq!(single.used_in_last_24h, Uint128::new(3_000_000));

    // Paginated enumeration returns the same data.
    let page = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRateLimitStatusesRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed();
    assert_eq!(page.len(), 1);
    let status = page.get(&usdc_denom).expect("usdc rate-limited");
    assert_eq!(status.supply_snapshot, Uint128::new(100_000_000));
    assert_eq!(status.cap, Uint128::new(10_000_000));
    assert_eq!(status.used_in_last_24h, Uint128::new(3_000_000));

    // After 24h + cron, the rolling sum drops back to zero.
    advance_to_next_day(&mut suite).await;

    let aged = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRateLimitStatusRequest { denom: usdc_denom },
        )
        .should_succeed()
        .expect("rate-limited");
    assert_eq!(aged.used_in_last_24h, Uint128::ZERO);
}

#[tokio::test]
async fn remove_routes() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let receiver = &mut accounts.user2;
    let relayer = &mut accounts.user1;

    let remote_recipient: Addr32 = Addr::mock(201).into();

    let arb_usdc = Remote::Warp {
        domain: mock_arbitrum::DOMAIN,
        contract: mock_arbitrum::USDC_WARP,
    };
    let eth_usdc = Remote::Warp {
        domain: mock_ethereum::DOMAIN,
        contract: mock_ethereum::USDC_WARP,
    };
    let eth_eth = Remote::Warp {
        domain: mock_ethereum::DOMAIN,
        contract: mock_ethereum::ETH_WARP,
    };

    // Attempt to remove a route as a non-owner. Should fail with the access
    // control error.
    suite
        .execute(
            &mut accounts.user3,
            contracts.gateway,
            &gateway::ExecuteMsg::RemoveRoutes(btree_set! { (contracts.warp, arb_usdc) }),
            Coins::default(),
        )
        .await
        .should_fail_with_error("only the owner can remove routes");

    // Attempt to remove a route that doesn't exist. Should fail at the route
    // lookup.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::RemoveRoutes(btree_set! {
                (contracts.warp, Remote::Warp {
                    domain: 999,
                    contract: Addr::mock(99).into(),
                }),
            }),
            Coins::default(),
        )
        .await
        .should_fail_with_error("data not found");

    // Remove a route that exists but was never funded, so it has no reserve
    // entry at all. Should succeed.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::RemoveRoutes(btree_set! { (contracts.warp, eth_eth) }),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRouteRequest {
                bridge: contracts.warp,
                remote: eth_eth,
            },
        )
        .should_succeed_and_equal(None);

    // Fund the arbitrum USDC route with 100M, plus the ethereum USDC route
    // with just enough to cover the arbitrum withdrawal fee, so that the
    // arbitrum reserve can later be drained to exactly zero.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            100_000_000,
        )
        .await
        .should_succeed();

    suite
        .receive_warp_transfer(
            relayer,
            mock_ethereum::DOMAIN,
            mock_ethereum::USDC_WARP,
            receiver,
            ARBITRUM_USDC_WITHDRAWAL_FEE,
        )
        .await
        .should_succeed();

    // Attempt to remove the arbitrum route while its reserve is non-zero.
    // Should fail with the reserve check error.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::RemoveRoutes(btree_set! { (contracts.warp, arb_usdc) }),
            Coins::default(),
        )
        .await
        .should_fail_with_error("can't remove route with non-zero reserve!");

    // Drain the arbitrum reserve to exactly zero: send the full 100M back,
    // with the fee covered by the ethereum top-up. The reserve entry remains
    // in storage afterwards, with a value of zero.
    suite
        .transfer_remote(
            receiver,
            &mut accounts.owner,
            contracts.gateway,
            arb_usdc,
            remote_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                100_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryReserveRequest {
                bridge: contracts.warp,
                remote: arb_usdc,
            },
        )
        .should_succeed_and_equal(Uint128::ZERO);

    // With the reserve drained to zero, the removal should now succeed.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::RemoveRoutes(btree_set! { (contracts.warp, arb_usdc) }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // The route and its reverse mapping should be gone.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRouteRequest {
                bridge: contracts.warp,
                remote: arb_usdc,
            },
        )
        .should_succeed_and_equal(None);

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryReverseRouteRequest {
                denom: usdc::DENOM.clone(),
                remote: arb_usdc,
            },
        )
        .should_succeed_and_equal(None);

    // The zero-valued reserve entry should have been deleted as well.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryReserveRequest {
                bridge: contracts.warp,
                remote: arb_usdc,
            },
        )
        .should_fail_with_error("data not found");

    // The sibling ethereum USDC route — same alloyed denom, different remote
    // — should be unaffected.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRouteRequest {
                bridge: contracts.warp,
                remote: eth_usdc,
            },
        )
        .should_succeed_and_equal(Some(usdc::DENOM.clone()));

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryReverseRouteRequest {
                denom: usdc::DENOM.clone(),
                remote: eth_usdc,
            },
        )
        .should_succeed_and_equal(Some(contracts.warp));

    // Inbound transfers through the removed route should fail.
    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            1_000_000,
        )
        .await
        .should_fail_with_error("data not found");

    // Outbound transfers through the removed route should fail as well. Top
    // the receiver up through the still-alive ethereum route first, so it has
    // tokens to attempt the transfer with.
    suite
        .receive_warp_transfer(
            relayer,
            mock_ethereum::DOMAIN,
            mock_ethereum::USDC_WARP,
            receiver,
            50_000,
        )
        .await
        .should_succeed();

    // The request itself fails fast at the route lookup — no withdrawal
    // request is stored for a route that doesn't exist.
    suite
        .execute(
            receiver,
            contracts.gateway,
            &gateway::ExecuteMsg::TransferRemote {
                remote: arb_usdc,
                recipient: remote_recipient,
            },
            Coin::new(usdc::DENOM.clone(), 50_000).unwrap(),
        )
        .await
        .should_fail_with_error("data not found");

    // Re-add the removed route, and verify it's functional again: an inbound
    // transfer recreates the reserve entry from scratch.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRoutes(btree_set! {
                (Origin::Remote(usdc::SUBDENOM.clone()), contracts.warp, arb_usdc),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite
        .receive_warp_transfer(
            relayer,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            receiver,
            70_000,
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryReserveRequest {
                bridge: contracts.warp,
                remote: arb_usdc,
            },
        )
        .should_succeed_and_equal(Uint128::new(70_000));
}

#[tokio::test]
async fn remove_native_route() {
    let (mut suite, mut accounts, _, contracts, mut valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    let remote_domain = 123;
    let remote_warp = Addr::mock(123);
    let dango_remote = Remote::Warp {
        domain: remote_domain,
        contract: remote_warp.into(),
    };

    // Register a route with a native denom in the gateway.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRoutes(btree_set! {
                (Origin::Local(dango::DENOM.clone()), contracts.warp, dango_remote),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Register the validator set for the remote domain.
    {
        let validator_set = MockValidatorSet::new_preset(remote_domain, false);

        suite
            .execute(
                &mut accounts.owner,
                contracts.hyperlane.ism,
                &isms::multisig::ExecuteMsg::SetValidators {
                    domain: remote_domain,
                    threshold: 2,
                    validators: validator_set.validator_addresses(),
                },
                Coins::default(),
            )
            .await
            .should_succeed();

        valset.insert(remote_domain, validator_set);
    }

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    // Send 100 dango to the remote domain. The tokens stay in the gateway
    // contract; no reserve entry is written, as reserves are only tracked
    // for remote denoms.
    suite
        .transfer_remote(
            &mut accounts.user1,
            &mut accounts.owner,
            contracts.gateway,
            dango_remote,
            Addr::mock(124).into(),
            coins! { dango::DENOM.clone() => 100 },
        )
        .await
        .should_succeed();

    // Even though 100 dango are currently bridged out, the route can be
    // removed: local-origin routes never track a reserve, so the reserve
    // check passes vacuously. The tokens stay in the gateway contract and
    // remain recoverable by re-adding the route, as demonstrated below.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::RemoveRoutes(btree_set! { (contracts.warp, dango_remote) }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // The route and its reverse mapping should be gone.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryRouteRequest {
                bridge: contracts.warp,
                remote: dango_remote,
            },
        )
        .should_succeed_and_equal(None);

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryReverseRouteRequest {
                denom: dango::DENOM.clone(),
                remote: dango_remote,
            },
        )
        .should_succeed_and_equal(None);

    // Receiving the tokens back now fails at the route lookup. (Contrast
    // with `native_denom`, where receiving on a route that exists but whose
    // gateway holds no balance fails with a balance subtraction error.)
    suite
        .receive_warp_transfer(
            &mut accounts.user3,
            remote_domain,
            remote_warp.into(),
            &accounts.user2,
            100,
        )
        .await
        .should_fail_with_error("data not found");

    // Re-add the route. The bridged-out tokens can then be received back.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRoutes(btree_set! {
                (Origin::Local(dango::DENOM.clone()), contracts.warp, dango_remote),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite.balances().record(&accounts.user2);

    suite
        .receive_warp_transfer(
            &mut accounts.user3,
            remote_domain,
            remote_warp.into(),
            &accounts.user2,
            100,
        )
        .await
        .should_succeed();

    suite.balances().should_change(
        &accounts.user2,
        btree_map! {
            dango::DENOM.clone() => BalanceChange::Increased(100),
        },
    );
}

/// Only the chain owner can set or update the withdrawal guardian.
#[tokio::test]
async fn set_guardian_only_owner() {
    let (suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let guardian_addr = accounts.user3.address();

    // A non-owner can't set the guardian.
    suite
        .execute(
            &mut accounts.user1,
            contracts.gateway,
            &gateway::ExecuteMsg::SetGuardian(guardian_addr),
            Coins::default(),
        )
        .await
        .should_fail_with_error("only the owner can set the withdrawal guardian");

    // The guardian is still unset.
    suite
        .query_wasm_smart(contracts.gateway, gateway::QueryGuardianRequest {})
        .should_succeed_and_equal(None);

    // The owner can set it.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetGuardian(guardian_addr),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(contracts.gateway, gateway::QueryGuardianRequest {})
        .should_succeed_and_equal(Some(guardian_addr));

    // Updating an already-set guardian follows the same rule: a non-owner
    // (not even the current guardian) can't overwrite it; the owner can.
    suite
        .execute(
            &mut accounts.user3,
            contracts.gateway,
            &gateway::ExecuteMsg::SetGuardian(accounts.user1.address()),
            Coins::default(),
        )
        .await
        .should_fail_with_error("only the owner can set the withdrawal guardian");

    // The failed attempt left the guardian unchanged.
    suite
        .query_wasm_smart(contracts.gateway, gateway::QueryGuardianRequest {})
        .should_succeed_and_equal(Some(guardian_addr));

    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetGuardian(accounts.user1.address()),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(contracts.gateway, gateway::QueryGuardianRequest {})
        .should_succeed_and_equal(Some(accounts.user1.address()));
}

/// A guardian configured in the genesis state is saved at instantiation:
/// the guardian query returns it without any `SetGuardian` call.
#[tokio::test]
async fn guardian_set_at_genesis() {
    let guardian_addr = Addr::mock(255);

    let (suite, _, _, contracts, _) = setup_test_naive_with_custom_genesis(
        TestOption {
            bridge_ops: |_| vec![],
            ..TestOption::default()
        },
        GenesisOption {
            gateway: GatewayOption {
                guardian: Some(guardian_addr),
                ..Preset::preset_test()
            },
            ..Preset::preset_test()
        },
    );

    suite
        .query_wasm_smart(contracts.gateway, gateway::QueryGuardianRequest {})
        .should_succeed_and_equal(Some(guardian_addr));
}

/// Only the chain owner and the guardian can respond to a withdrawal
/// request.
#[tokio::test]
async fn respond_only_owner_or_guardian() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let guardian_addr = accounts.user3.address();

    let remote = Remote::Warp {
        domain: mock_arbitrum::DOMAIN,
        contract: mock_arbitrum::USDC_WARP,
    };
    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // Fund user2 so it can open withdrawal requests.
    suite
        .receive_warp_transfer(
            &mut accounts.user1,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            &accounts.user2,
            100_000_000,
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetGuardian(guardian_addr),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Open a request.
    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            remote,
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                1_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await;

    // A random account can't respond — regardless of the response kind.
    for response in [
        WithdrawalResponse::Approve,
        WithdrawalResponse::Reject,
        WithdrawalResponse::Freeze,
        WithdrawalResponse::Confiscate,
    ] {
        suite
            .respond_to_withdrawal(&mut accounts.user1, contracts.gateway, id, response)
            .await
            .should_fail_with_error("you don't have the right, O you don't have the right");
    }

    // Neither can the requester themself.
    suite
        .respond_to_withdrawal(
            &mut accounts.user2,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_fail_with_error("you don't have the right, O you don't have the right");

    // The guardian can respond.
    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed();

    // The owner can respond as well — open a second request for it.
    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            remote,
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                1_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await;

    suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed();
}

/// Once a request is frozen, only the owner can respond to it — the
/// guardian who froze it is locked out.
#[tokio::test]
async fn respond_frozen_only_owner() {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    let guardian_addr = accounts.user3.address();

    let remote = Remote::Warp {
        domain: mock_arbitrum::DOMAIN,
        contract: mock_arbitrum::USDC_WARP,
    };
    let mock_arbitrum_recipient: Addr32 = Addr::mock(201).into();

    // Fund user2 so it can open withdrawal requests.
    suite
        .receive_warp_transfer(
            &mut accounts.user1,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            &accounts.user2,
            100_000_000,
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetGuardian(guardian_addr),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Open a request and let the guardian freeze it.
    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            remote,
            mock_arbitrum_recipient,
            Coin::new(
                usdc::DENOM.clone(),
                1_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await;

    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Freeze,
        )
        .await
        .should_succeed();

    // The guardian who froze it can no longer respond in any way.
    for response in [
        WithdrawalResponse::Approve,
        WithdrawalResponse::Reject,
        WithdrawalResponse::Freeze,
        WithdrawalResponse::Confiscate,
    ] {
        suite
            .respond_to_withdrawal(&mut accounts.user3, contracts.gateway, id, response)
            .await
            .should_fail_with_error("only the owner can respond to a frozen withdrawal request");
    }

    // A random account still fails the generic auth check.
    suite
        .respond_to_withdrawal(
            &mut accounts.user1,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_fail_with_error("you don't have the right, O you don't have the right");

    // The owner can resolve the frozen request.
    suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed();
}

/// Amount held by the gateway per request in the coin accounting tests:
/// 5M to bridge plus the withdrawal fee.
const WITHDRAW: u128 = 5_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE;

/// The route used by the coin accounting tests.
const WITHDRAW_REMOTE: Remote = Remote::Warp {
    domain: mock_arbitrum::DOMAIN,
    contract: mock_arbitrum::USDC_WARP,
};

/// The remote recipient used by the coin accounting tests.
fn withdraw_recipient() -> Addr32 {
    Addr::mock(201).into()
}

/// Common setup for the coin accounting tests: fund user2 with 100M USDC
/// through the arbitrum route, and set user3 as the withdrawal guardian.
async fn setup_withdrawal_test() -> (DefaultHyperlaneTestSuite, TestAccounts, Contracts) {
    let (mut suite, mut accounts, _, contracts, valset) = setup_test(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    suite.block_time = Duration::ZERO;

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    suite
        .receive_warp_transfer(
            &mut accounts.user1,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            &accounts.user2,
            100_000_000,
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetGuardian(accounts.user3.address()),
            Coins::default(),
        )
        .await
        .should_succeed();

    (suite, accounts, contracts)
}

/// Approving a withdrawal request drains the withheld coins from the
/// gateway — burned plus fee to the owner — with nothing flowing back to
/// the user.
#[tokio::test]
async fn withdrawal_approval_coin_accounting() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();
    let owner_addr = accounts.owner.address();

    // ---- Placing a request moves the funds from the sender to the gateway ----
    suite.balances().record(&user_addr);
    suite.balances().record(&contracts.gateway);
    suite.balances().record(&owner_addr);

    // Execute the request directly (rather than through the helper) to get
    // at the emitted `withdrawal_requested` event.
    let events = suite
        .execute(
            &mut accounts.user2,
            contracts.gateway,
            &gateway::ExecuteMsg::TransferRemote {
                remote: WITHDRAW_REMOTE,
                recipient: withdraw_recipient(),
            },
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await
        .should_succeed()
        .events;

    let requested = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| e.contract == contracts.gateway && e.ty == "withdrawal_requested")
        .take()
        .one()
        .event
        .data
        .deserialize_json::<gateway::WithdrawalRequested>()
        .unwrap();

    assert_eq!(
        requested,
        gateway::WithdrawalRequested {
            id: 0,
            user: user_addr,
            remote: WITHDRAW_REMOTE,
            recipient: withdraw_recipient(),
            denom: usdc::DENOM.clone(),
            amount: Uint128::new(WITHDRAW),
        }
    );

    let id = requested.id;

    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Decreased(WITHDRAW),
        },
    );
    suite.balances().should_change(
        &contracts.gateway,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Increased(WITHDRAW),
        },
    );

    // The gateway holds exactly the withdrawn amount.
    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(WITHDRAW.into());

    // ---- Approval drains the gateway; nothing flows back to the user ----
    suite.balances().refresh_all();

    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed();

    // The bridged amount is burned and the fee goes to the owner; the user
    // gets nothing back.
    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Unchanged,
        },
    );
    suite.balances().should_change(
        &contracts.gateway,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Decreased(WITHDRAW),
        },
    );
    suite.balances().should_change(
        &owner_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Increased(ARBITRUM_USDC_WITHDRAWAL_FEE),
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    // The approved request is deleted from storage.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed_and_equal(None);
}

/// Rejecting a withdrawal request refunds the full withheld amount to the
/// user.
#[tokio::test]
async fn withdrawal_rejection_coin_accounting() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();
    let owner_addr = accounts.owner.address();

    suite.balances().record(&user_addr);
    suite.balances().record(&contracts.gateway);
    suite.balances().record(&owner_addr);

    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    let events = suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Reject,
        )
        .await
        .should_succeed()
        .events;

    let rejected = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| e.contract == contracts.gateway && e.ty == "withdrawal_rejected")
        .take()
        .one()
        .event
        .data
        .deserialize_json::<gateway::WithdrawalRejected>()
        .unwrap();

    assert_eq!(
        rejected,
        gateway::WithdrawalRejected {
            id,
            user: user_addr,
            denom: usdc::DENOM.clone(),
            amount: Uint128::new(WITHDRAW),
            rejected_by: accounts.user3.address(),
        }
    );

    // Across request + rejection the user is made whole — no fee is
    // charged on a rejected withdrawal — and the gateway keeps nothing.
    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Unchanged,
        },
    );
    suite.balances().should_change(
        &contracts.gateway,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Unchanged,
        },
    );
    suite.balances().should_change(
        &owner_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Unchanged,
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    // The rejected request is deleted from storage.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed_and_equal(None);
}

/// Freezing a withdrawal request leaves the withheld coins inside the
/// gateway.
#[tokio::test]
async fn withdrawal_freeze_coin_accounting() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();

    suite.balances().record(&user_addr);
    suite.balances().record(&contracts.gateway);

    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    let events = suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Freeze,
        )
        .await
        .should_succeed()
        .events;

    let frozen = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| e.contract == contracts.gateway && e.ty == "withdrawal_frozen")
        .take()
        .one()
        .event
        .data
        .deserialize_json::<gateway::WithdrawalFrozen>()
        .unwrap();

    assert_eq!(
        frozen,
        gateway::WithdrawalFrozen {
            id,
            user: user_addr,
            denom: usdc::DENOM.clone(),
            amount: Uint128::new(WITHDRAW),
            frozen_by: accounts.user3.address(),
        }
    );

    // The funds left the user on request, and the freeze keeps them in the
    // gateway.
    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Decreased(WITHDRAW),
        },
    );
    suite.balances().should_change(
        &contracts.gateway,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Increased(WITHDRAW),
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(WITHDRAW.into());

    // Unlike the terminal responses, freezing keeps the request in storage
    // — now in the frozen state — awaiting the owner's decision.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed_and(|req| {
            req.as_ref()
                .is_some_and(|req| req.status == WithdrawalStatus::Frozen)
        });
}

/// Confiscating a frozen withdrawal request sends the withheld coins to
/// the owner.
#[tokio::test]
async fn withdrawal_confiscation_coin_accounting() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();
    let owner_addr = accounts.owner.address();

    // Setup: open a request and freeze it, so it can be confiscated.
    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Freeze,
        )
        .await
        .should_succeed();

    // ---- Confiscation sends the withheld coins to the owner ----
    suite.balances().record(&user_addr);
    suite.balances().record(&contracts.gateway);
    suite.balances().record(&owner_addr);

    let events = suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            id,
            WithdrawalResponse::Confiscate,
        )
        .await
        .should_succeed()
        .events;

    let confiscated = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| {
            e.contract == contracts.gateway && e.ty == "withdrawal_confiscated"
        })
        .take()
        .one()
        .event
        .data
        .deserialize_json::<gateway::WithdrawalConfiscated>()
        .unwrap();

    assert_eq!(
        confiscated,
        gateway::WithdrawalConfiscated {
            id,
            user: user_addr,
            denom: usdc::DENOM.clone(),
            amount: Uint128::new(WITHDRAW),
        }
    );

    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Unchanged,
        },
    );
    suite.balances().should_change(
        &contracts.gateway,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Decreased(WITHDRAW),
        },
    );
    suite.balances().should_change(
        &owner_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Increased(WITHDRAW),
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    // The confiscated request is deleted from storage.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed_and_equal(None);
}

/// Placing a withdrawal request stores it correctly: every field reflects
/// the request, it starts in the pending state, and it shows up in the
/// pending queue.
#[tokio::test]
async fn withdrawal_request_is_stored() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    // The first request gets ID zero.
    assert_eq!(id, 0);

    // The stored record matches the request in full. `created_at` is the
    // block timestamp, which doesn't move here since the setup pins the
    // block time to zero.
    let request = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed()
        .expect("the request should be stored");

    assert_eq!(
        request,
        gateway::WithdrawalRequest {
            user: accounts.user2.address(),
            remote: WITHDRAW_REMOTE,
            recipient: withdraw_recipient(),
            coin: Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
            status: WithdrawalStatus::Pending,
            created_at: suite.block.timestamp,
        }
    );

    // It is listed in the pending queue.
    let page = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestsRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed();

    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, id);
    assert_eq!(page[0].request, request);
}

/// A withdrawal request can only be confiscated after it has been frozen:
/// even the owner can't confiscate a pending request directly.
#[tokio::test]
async fn confiscation_requires_prior_freeze() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            id,
            WithdrawalResponse::Confiscate,
        )
        .await
        .should_fail_with_error("only a frozen withdrawal request can be confiscated");

    // The failed confiscation left the request untouched: still pending,
    // with the funds still in the gateway.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed_and(|req| {
            req.as_ref()
                .is_some_and(|req| req.status == WithdrawalStatus::Pending)
        });

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(WITHDRAW.into());
}

/// The guardian can never confiscate a withdrawal request — pending
/// requests included. (The frozen case is covered by
/// `respond_frozen_only_owner`, where the guardian is locked out of frozen
/// requests entirely.)
#[tokio::test]
async fn guardian_can_never_confiscate() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Confiscate,
        )
        .await
        .should_fail_with_error("only the owner can confiscate a withdrawal request");

    // The failed confiscation left the request untouched: still pending,
    // with the funds still in the gateway.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed_and(|req| {
            req.as_ref()
                .is_some_and(|req| req.status == WithdrawalStatus::Pending)
        });

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(WITHDRAW.into());
}

/// The owner can reject a frozen withdrawal request, refunding the full
/// withheld amount to the user.
#[tokio::test]
async fn frozen_request_rejection_refunds_user() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();

    suite.balances().record(&user_addr);

    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Freeze,
        )
        .await
        .should_succeed();

    suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            id,
            WithdrawalResponse::Reject,
        )
        .await
        .should_succeed();

    // Across request + freeze + rejection the user is made whole, the
    // gateway keeps nothing, and the request is deleted.
    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Unchanged,
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed_and_equal(None);
}

/// Withdrawal request IDs increment sequentially, and resolving a request
/// doesn't free its ID for reuse — the counter only moves forward.
#[tokio::test]
async fn withdrawal_request_ids_increment() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    for expected_id in 0..2u64 {
        let id = suite
            .request_transfer_remote(
                &mut accounts.user2,
                contracts.gateway,
                WITHDRAW_REMOTE,
                withdraw_recipient(),
                Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
            )
            .await;

        assert_eq!(id, expected_id);
    }

    // Resolve the first request; the next request still gets a fresh ID.
    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            0,
            WithdrawalResponse::Reject,
        )
        .await
        .should_succeed();

    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    assert_eq!(id, 2);
}

/// The pending queue paginates: `limit` caps the page size, `start_after`
/// resumes exclusively after the given ID, and paging past the end yields
/// an empty page.
#[tokio::test]
async fn withdrawal_requests_pagination() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    // Open three requests; IDs are sequential from zero.
    for expected_id in 0..3u64 {
        let id = suite
            .request_transfer_remote(
                &mut accounts.user2,
                contracts.gateway,
                WITHDRAW_REMOTE,
                withdraw_recipient(),
                Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
            )
            .await;

        assert_eq!(id, expected_id);
    }

    // First page, limit 2: the two lowest IDs, in ascending order.
    let page1 = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestsRequest {
                start_after: None,
                limit: Some(2),
            },
        )
        .should_succeed();

    assert_eq!(
        page1.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![0, 1]
    );

    // Second page resumes after the last ID of page 1 — `start_after` is
    // exclusive, so ID 1 doesn't repeat.
    let page2 = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestsRequest {
                start_after: Some(page1.last().unwrap().id),
                limit: Some(2),
            },
        )
        .should_succeed();

    assert_eq!(
        page2.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![2]
    );

    // Querying beyond the end yields an empty page.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestsRequest {
                start_after: Some(2),
                limit: Some(2),
            },
        )
        .should_succeed_and(Vec::is_empty);
}

/// Freezing moves a request from the pending queue to the frozen queue:
/// it disappears from `WithdrawalRequests` and shows up in
/// `FrozenWithdrawalRequests`, while other pending requests stay put.
#[tokio::test]
async fn frozen_withdrawal_requests_enumeration() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    // Open two requests, so freezing one demonstrates the other is
    // unaffected.
    let mut ids = Vec::new();
    for _ in 0..2 {
        let id = suite
            .request_transfer_remote(
                &mut accounts.user2,
                contracts.gateway,
                WITHDRAW_REMOTE,
                withdraw_recipient(),
                Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
            )
            .await;

        ids.push(id);
    }

    // Both requests sit in the pending queue; the frozen queue is empty.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestsRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and(|page| page.iter().map(|item| item.id).collect::<Vec<_>>() == ids);

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryFrozenWithdrawalRequestsRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and(Vec::is_empty);

    // Freeze the first request.
    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            ids[0],
            WithdrawalResponse::Freeze,
        )
        .await
        .should_succeed();

    // The frozen request left the pending queue; the other remains.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestsRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and(|page| {
            page.iter().map(|item| item.id).collect::<Vec<_>>() == vec![ids[1]]
        });

    // And it now shows up in the frozen queue, in the frozen state, with
    // the rest of the record intact.
    let frozen = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryFrozenWithdrawalRequestsRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed();

    assert_eq!(frozen.len(), 1);
    assert_eq!(frozen[0].id, ids[0]);
    assert_eq!(frozen[0].request.status, WithdrawalStatus::Frozen);
    assert_eq!(frozen[0].request.user, accounts.user2.address());
    assert_eq!(
        frozen[0].request.coin,
        Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap()
    );
}

/// A request exceeding the rate-limit cap is rejected outright by the
/// fail-fast validation: no request is stored and no funds leave the user.
#[tokio::test]
async fn over_cap_request_fails_fast() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();

    // 10% rate limit against the 100M supply → cap 10M, seeded immediately.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite.balances().record(&user_addr);

    // 11M is over the 10M cap; the request itself fails.
    suite
        .execute(
            &mut accounts.user2,
            contracts.gateway,
            &gateway::ExecuteMsg::TransferRemote {
                remote: WITHDRAW_REMOTE,
                recipient: withdraw_recipient(),
            },
            Coin::new(
                usdc::DENOM.clone(),
                11_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota! denom: bridge/usdc, requested: 11000000, residue after personal quota: 11000000");

    // Nothing was stored and nothing was escrowed.
    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Unchanged,
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestsRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and(|page| page.is_empty());
}

/// The fail-fast validation is read-only: pending requests consume no
/// quota, so two requests that individually fit the cap both pass it. The
/// first approval consumes the quota; the second approval then finds no
/// headroom left and refunds the user instead of bridging.
#[tokio::test]
async fn racing_approvals_refund_second_request() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();

    // Escrowed per racing request: 8M to bridge plus the fee — two of them
    // individually fit the 10M cap, together they exceed it.
    const AMOUNT: u128 = 8_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE;

    // 10% rate limit against the 100M supply → cap 10M.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite.balances().record(&user_addr);

    let first = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), AMOUNT).unwrap(),
        )
        .await;

    let second = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), AMOUNT).unwrap(),
        )
        .await;

    // The first approval bridges, consuming 8M of the 10M cap.
    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            first,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed();

    // The second approval succeeds as a transaction, but the withdrawal no
    // longer fits the cap, so the user is refunded instead.
    let events = suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            second,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed()
        .events;

    let failed = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| {
            e.contract == contracts.gateway && e.ty == "withdrawal_approval_failed"
        })
        .take()
        .one()
        .event
        .data
        .deserialize_json::<gateway::WithdrawalApprovalFailed>()
        .unwrap();

    assert_eq!(failed.id, second);
    assert_eq!(failed.user, user_addr);
    assert_eq!(failed.denom, usdc::DENOM.clone());
    assert_eq!(failed.amount, Uint128::new(AMOUNT));
    assert!(failed.reason.contains("insufficient outbound quota"));

    // The user paid for the first withdrawal only; the second escrow came
    // back. The gateway keeps nothing, and both requests are settled.
    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Decreased(AMOUNT),
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    // Both requests are deleted from storage — the refunded one is just as
    // settled as the bridged one — and the pending queue is empty.
    for id in [first, second] {
        suite
            .query_wasm_smart(
                contracts.gateway,
                gateway::QueryWithdrawalRequestRequest { id },
            )
            .should_succeed_and_equal(None);
    }

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestsRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and(|page| page.is_empty());
}

/// A request through a route whose reserve can't cover the bridged amount
/// is rejected outright by the fail-fast validation: no request is stored
/// and no funds leave the user.
#[tokio::test]
async fn insufficient_reserve_request_fails_fast() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();

    /// Mirrors the ethereum USDC withdrawal fee configured in the test
    /// genesis at `dango/testing/src/genesis.rs`; keep the two in sync.
    const ETHEREUM_USDC_WITHDRAWAL_FEE: u128 = 1_000_000;

    suite.balances().record(&user_addr);

    // The user holds USDC bridged in from arbitrum; the ethereum route was
    // never funded, so its reserve is zero and the request fails.
    suite
        .execute(
            &mut accounts.user2,
            contracts.gateway,
            &gateway::ExecuteMsg::TransferRemote {
                remote: Remote::Warp {
                    domain: mock_ethereum::DOMAIN,
                    contract: mock_ethereum::USDC_WARP,
                },
                recipient: withdraw_recipient(),
            },
            Coin::new(
                usdc::DENOM.clone(),
                5_000_000 + ETHEREUM_USDC_WITHDRAWAL_FEE,
            )
            .unwrap(),
        )
        .await
        .should_fail_with_error("insufficient reserve!");

    // Nothing was stored and nothing was escrowed.
    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Unchanged,
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestsRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and(|page| page.is_empty());
}

/// A request whose amount doesn't strictly exceed the withdrawal fee is
/// rejected outright by the fail-fast validation — after deducting the
/// fee there would be nothing left to bridge. No request is stored and
/// no funds leave the user.
#[tokio::test]
async fn withdrawal_not_exceeding_fee_fails_fast() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();

    suite.balances().record(&user_addr);

    // Exactly the fee: rejected, as nothing would be left to bridge.
    suite
        .execute(
            &mut accounts.user2,
            contracts.gateway,
            &gateway::ExecuteMsg::TransferRemote {
                remote: WITHDRAW_REMOTE,
                recipient: withdraw_recipient(),
            },
            Coin::new(usdc::DENOM.clone(), ARBITRUM_USDC_WITHDRAWAL_FEE).unwrap(),
        )
        .await
        .should_fail_with_error(format!(
            "withdrawal amount not sufficient to cover fee: {} <= {}",
            ARBITRUM_USDC_WITHDRAWAL_FEE, ARBITRUM_USDC_WITHDRAWAL_FEE
        ));

    // Less than the fee: rejected as well.
    suite
        .execute(
            &mut accounts.user2,
            contracts.gateway,
            &gateway::ExecuteMsg::TransferRemote {
                remote: WITHDRAW_REMOTE,
                recipient: withdraw_recipient(),
            },
            Coin::new(usdc::DENOM.clone(), ARBITRUM_USDC_WITHDRAWAL_FEE - 1).unwrap(),
        )
        .await
        .should_fail_with_error(format!(
            "withdrawal amount not sufficient to cover fee: {} <= {}",
            ARBITRUM_USDC_WITHDRAWAL_FEE - 1,
            ARBITRUM_USDC_WITHDRAWAL_FEE
        ));

    // Nothing was stored and nothing was escrowed.
    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Unchanged,
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestsRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and(|page| page.is_empty());
}

/// Freezing is not idempotent: a `Freeze` response to an already-frozen
/// request fails — even from the owner, who is otherwise allowed to
/// respond to frozen requests.
#[tokio::test]
async fn refreezing_frozen_request_fails() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Freeze,
        )
        .await
        .should_succeed();

    suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            id,
            WithdrawalResponse::Freeze,
        )
        .await
        .should_fail_with_error("withdrawal request is already frozen");

    // The failed refreeze left the request in the frozen state, with the
    // funds still escrowed.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed_and(|req| {
            req.as_ref()
                .is_some_and(|req| req.status == WithdrawalStatus::Frozen)
        });

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(WITHDRAW.into());
}

/// Responding to a request ID that doesn't exist fails — whether the ID
/// was never assigned, or belonged to a request that has already reached
/// a terminal response and was deleted.
#[tokio::test]
async fn respond_to_unknown_or_settled_request_fails() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    // No request with ID 999 was ever created.
    suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            999,
            WithdrawalResponse::Approve,
        )
        .await
        .should_fail_with_error("withdrawal request not found: 999");

    // Open a request and settle it with a rejection. Responding to the
    // settled ID fails the same way as a never-assigned one.
    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Reject,
        )
        .await
        .should_succeed();

    suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_fail_with_error(format!("withdrawal request not found: {id}"));
}

/// Pending requests reserve nothing, so two requests can each pass the
/// fail-fast reserve check while their combined bridged amounts exceed
/// the route's reserve. The first approval drains the reserve; the second
/// approval then refunds the user instead of bridging.
#[tokio::test]
async fn reserve_drained_while_pending_refunds_on_approval() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();

    /// Mirrors the ethereum USDC withdrawal fee configured in the test
    /// genesis at `dango/testing/src/genesis.rs`; keep the two in sync.
    const ETHEREUM_USDC_WITHDRAWAL_FEE: u128 = 1_000_000;

    /// Escrowed per request: 6M to bridge plus the fee. Each fits the 10M
    /// ethereum reserve on its own; together they exceed it.
    const AMOUNT: u128 = 6_000_000 + ETHEREUM_USDC_WITHDRAWAL_FEE;

    const ETHEREUM_USDC_REMOTE: Remote = Remote::Warp {
        domain: mock_ethereum::DOMAIN,
        contract: mock_ethereum::USDC_WARP,
    };

    // Fund the ethereum route's reserve with 10M — smaller than the user's
    // balance, so both escrows are affordable while the combined bridged
    // amounts overshoot the reserve.
    suite
        .receive_warp_transfer(
            &mut accounts.user1,
            mock_ethereum::DOMAIN,
            mock_ethereum::USDC_WARP,
            &accounts.user2,
            10_000_000,
        )
        .await
        .should_succeed();

    suite.balances().record(&user_addr);

    let first = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            ETHEREUM_USDC_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), AMOUNT).unwrap(),
        )
        .await;

    let second = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            ETHEREUM_USDC_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), AMOUNT).unwrap(),
        )
        .await;

    // The first approval bridges 6M, dropping the reserve to 4M.
    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            first,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed();

    // The second approval succeeds as a transaction, but the reserve no
    // longer covers the bridged amount, so the user is refunded instead.
    let events = suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            second,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed()
        .events;

    let failed = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| {
            e.contract == contracts.gateway && e.ty == "withdrawal_approval_failed"
        })
        .take()
        .one()
        .event
        .data
        .deserialize_json::<gateway::WithdrawalApprovalFailed>()
        .unwrap();

    assert_eq!(failed.id, second);
    assert_eq!(failed.user, user_addr);
    assert_eq!(failed.amount, Uint128::new(AMOUNT));
    assert!(failed.reason.contains("insufficient reserve"));

    // The user paid for the first withdrawal only; the second escrow came
    // back, and both requests are settled.
    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Decreased(AMOUNT),
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    for id in [first, second] {
        suite
            .query_wasm_smart(
                contracts.gateway,
                gateway::QueryWithdrawalRequestRequest { id },
            )
            .should_succeed_and_equal(None);
    }
}

/// A fee raised while a request is pending is enforced at approval: if
/// the escrowed amount no longer strictly exceeds the fee, the approval
/// refunds the user instead of bridging.
#[tokio::test]
async fn fee_raised_while_pending_refunds_on_approval() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();

    suite.balances().record(&user_addr);

    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    // Raise the fee to exactly the escrowed amount, so nothing would be
    // left to bridge.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetWithdrawalFees(vec![WithdrawalFee {
                denom: usdc::DENOM.clone(),
                remote: WITHDRAW_REMOTE,
                fee: Op::Insert(Uint128::new(WITHDRAW)),
            }]),
            Coins::default(),
        )
        .await
        .should_succeed();

    let events = suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed()
        .events;

    let failed = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| {
            e.contract == contracts.gateway && e.ty == "withdrawal_approval_failed"
        })
        .take()
        .one()
        .event
        .data
        .deserialize_json::<gateway::WithdrawalApprovalFailed>()
        .unwrap();

    assert_eq!(failed.id, id);
    assert_eq!(failed.user, user_addr);
    assert_eq!(failed.amount, Uint128::new(WITHDRAW));
    assert!(failed.reason.contains("not sufficient to cover fee"));

    // The escrow came back in full; the request is settled.
    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Unchanged,
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed_and_equal(None);
}

/// A route removed while a request is pending fails the approval-time
/// validation: the user is refunded instead of bridging. Uses a
/// local-origin route, which tracks no reserve and can therefore be
/// removed with a request outstanding.
#[tokio::test]
async fn route_removed_while_pending_refunds_on_approval() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();

    let dango_remote = Remote::Warp {
        domain: 123,
        contract: Addr::mock(123).into(),
    };

    // Register a local-origin route. No validator set is needed: the
    // request is refunded at approval, so no hyperlane message is ever
    // relayed.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRoutes(btree_set! {
                (
                    Origin::Local(dango::DENOM.clone()),
                    contracts.warp,
                    dango_remote,
                ),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite.balances().record(&user_addr);

    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            dango_remote,
            withdraw_recipient(),
            coins! { dango::DENOM.clone() => 1_000 },
        )
        .await;

    // Remove the route while the request is pending. Local-origin routes
    // never track a reserve, so removal is allowed.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::RemoveRoutes(btree_set! { (contracts.warp, dango_remote) }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // The approval finds no route and refunds the escrow.
    let events = suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed()
        .events;

    let failed = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| {
            e.contract == contracts.gateway && e.ty == "withdrawal_approval_failed"
        })
        .take()
        .one()
        .event
        .data
        .deserialize_json::<gateway::WithdrawalApprovalFailed>()
        .unwrap();

    assert_eq!(failed.id, id);
    assert_eq!(failed.user, user_addr);
    assert_eq!(failed.denom, dango::DENOM.clone());
    assert!(failed.reason.contains("data not found"));

    suite.balances().should_change(
        &user_addr,
        btree_map! {
            dango::DENOM.clone() => BalanceChange::Unchanged,
        },
    );

    suite
        .query_balance(&contracts.gateway, dango::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed_and_equal(None);
}

/// A frozen request is re-validated when the owner approves it. If the
/// withdrawal no longer fits the rate limit, the owner's approval refunds
/// the user — settling the request out of the frozen queue.
#[tokio::test]
async fn frozen_request_approval_failure_refunds() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();

    // Escrowed per request: 8M to bridge plus the fee — two of them
    // individually fit the 10M cap, together they exceed it.
    const AMOUNT: u128 = 8_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE;

    // 10% rate limit against the 100M supply → cap 10M, seeded immediately.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(10)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite.balances().record(&user_addr);

    // Open a request and freeze it.
    let frozen_id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), AMOUNT).unwrap(),
        )
        .await;

    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            frozen_id,
            WithdrawalResponse::Freeze,
        )
        .await
        .should_succeed();

    // A second request is approved while the first sits frozen, consuming
    // 8M of the 10M window.
    let second_id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), AMOUNT).unwrap(),
        )
        .await;

    suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            second_id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed();

    // The owner approves the frozen request; it no longer fits the cap,
    // so the user is refunded and the frozen queue is emptied.
    let events = suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            frozen_id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed()
        .events;

    let failed = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| {
            e.contract == contracts.gateway && e.ty == "withdrawal_approval_failed"
        })
        .take()
        .one()
        .event
        .data
        .deserialize_json::<gateway::WithdrawalApprovalFailed>()
        .unwrap();

    assert_eq!(failed.id, frozen_id);
    assert_eq!(failed.user, user_addr);
    assert_eq!(failed.amount, Uint128::new(AMOUNT));
    assert!(failed.reason.contains("insufficient outbound quota"));

    // The user paid for the approved withdrawal only.
    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Decreased(AMOUNT),
        },
    );

    suite
        .query_balance(&contracts.gateway, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    // The refunded request left the frozen queue and is deleted.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryFrozenWithdrawalRequestsRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and(Vec::is_empty);

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id: frozen_id },
        )
        .should_succeed_and_equal(None);
}

/// A personal quota is consumed at approval, not at request time: a quota
/// that covers the withdrawal when the request is created but expires
/// while it is pending no longer counts at approval, and the withdrawal
/// falls back to the global window.
#[tokio::test]
async fn personal_quota_expired_between_request_and_approval() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let user_addr = accounts.user2.address();

    // Escrow 5M plus the fee — well over the 1M cap, but fully covered by
    // the personal quota at request time.
    const AMOUNT: u128 = 5_000_000 + ARBITRUM_USDC_WITHDRAWAL_FEE;

    // 1% rate limit against the 100M supply → cap 1M, seeded immediately.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => RateLimit::new_unchecked(Udec128::new_percent(1)),
            }),
            Coins::default(),
        )
        .await
        .should_succeed();

    // Grant a 10M personal quota that expires in one hour.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: user_addr,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(SetPersonalQuotaRequest {
                    amount: Uint128::new(10_000_000),
                    available_for: Some(Duration::from_hours(1)),
                }),
            },
            Coins::default(),
        )
        .await
        .should_succeed();

    suite.balances().record(&user_addr);

    // The quota covers the 5M in full, so the fail-fast validation passes
    // despite the 1M cap.
    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), AMOUNT).unwrap(),
        )
        .await;

    // The quota expires while the request is pending.
    advance_by(&mut suite, Duration::from_hours(1)).await;

    // Approval re-validates with the quota expired: the full 5M counts
    // against the 1M cap, so the user is refunded instead.
    let events = suite
        .respond_to_withdrawal(
            &mut accounts.user3,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed()
        .events;

    let failed = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| {
            e.contract == contracts.gateway && e.ty == "withdrawal_approval_failed"
        })
        .take()
        .one()
        .event
        .data
        .deserialize_json::<gateway::WithdrawalApprovalFailed>()
        .unwrap();

    assert_eq!(failed.id, id);
    assert_eq!(failed.user, user_addr);
    assert!(failed.reason.contains("insufficient outbound quota"));

    suite.balances().should_change(
        &user_addr,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Unchanged,
        },
    );

    // The expired entry is left in storage untouched — the failed
    // consumption never scrubbed it.
    let pq = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: user_addr,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed()
        .expect("expired entry is left in storage");
    assert_eq!(pq.amount, Uint128::new(10_000_000));

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed_and_equal(None);
}

/// Rotating the guardian takes effect immediately, including for requests
/// opened before the rotation: the old guardian loses its response rights,
/// the new one gains them.
#[tokio::test]
async fn guardian_rotation() {
    let (mut suite, mut accounts, contracts) = setup_withdrawal_test().await;

    let new_guardian_addr = accounts.user1.address();

    // Open a request while user3 is still the guardian.
    let id = suite
        .request_transfer_remote(
            &mut accounts.user2,
            contracts.gateway,
            WITHDRAW_REMOTE,
            withdraw_recipient(),
            Coin::new(usdc::DENOM.clone(), WITHDRAW).unwrap(),
        )
        .await;

    // Rotate the guardian to user1.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetGuardian(new_guardian_addr),
            Coins::default(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(contracts.gateway, gateway::QueryGuardianRequest {})
        .should_succeed_and_equal(Some(new_guardian_addr));

    // The old guardian can no longer respond — not even to a request that
    // was opened on its watch.
    for response in [
        WithdrawalResponse::Approve,
        WithdrawalResponse::Reject,
        WithdrawalResponse::Freeze,
    ] {
        suite
            .respond_to_withdrawal(&mut accounts.user3, contracts.gateway, id, response)
            .await
            .should_fail_with_error("you don't have the right, O you don't have the right");
    }

    // The new guardian can, including this pre-rotation request.
    suite
        .respond_to_withdrawal(
            &mut accounts.user1,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed();
}

async fn advance_to_next_day(suite: &mut TestSuite) {
    suite.block_time = Duration::from_days(1);
    suite.make_empty_block().await;
    suite.block_time = Duration::ZERO;
}

async fn advance_by(suite: &mut TestSuite, d: Duration) {
    suite.block_time = d;
    suite.make_empty_block().await;
    suite.block_time = Duration::ZERO;
}
