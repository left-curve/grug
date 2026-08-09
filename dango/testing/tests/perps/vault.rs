use {
    crate::{default_pair_param, default_param, register_oracle_prices},
    dango_app::CONTRACT_NAMESPACE,
    dango_math::{Dec128_6, NumberConst, Uint128},
    dango_order_book::{
        Dimensionless, OrderId, OrderKind, Quantity, QueryOrdersByUserResponseItem, UsdPrice,
        UsdValue,
    },
    dango_primitives::{
        Addressable, Coins, Duration, QuerierExt, ResultExt, Timestamp, btree_map, concat,
    },
    dango_pyth_types::MarketSession,
    dango_testing::{OracleTestEntry, TestOption, pair_id, setup_test_naive},
    dango_types::{
        constants::usdc,
        oracle::QueryPriceRequest,
        perps::{self, PairParam, Param},
    },
    std::{collections::BTreeMap, str::FromStr},
};

/// Covers: add liquidity → vault trades → realized PnL reflected in share
/// price → correct withdrawal amounts.
///
/// | Step | Action                                           | Assert                                          |
/// | ---- | ------------------------------------------------ | ----------------------------------------------- |
/// | 1    | LP deposits $10k, adds $5k liquidity             | vault margin=$5,000; shares minted              |
/// | 2    | Configure: enable vault MM (weight=1, spread=5%) | —                                               |
/// | 3    | OnOracleUpdate → vault places bid+ask             | vault has orders on book                        |
/// | 4    | Taker deposits $10k, sells into vault's bid       | vault gets long position                        |
/// | 5    | Oracle → $2,200                                  | vault's long has unrealized profit              |
/// | 6    | OnOracleUpdate → vault refreshes orders           | —                                               |
/// | 7    | Taker closes (buys back via vault ask)            | vault realizes PnL; vault margin increases      |
/// | 8    | LP removes half shares                           | unlock reflects realized PnL in share price     |
/// | 9    | LP removes remaining shares                      | unlock reflects realized PnL                    |
/// | 10   | Advance time past cooldown                       | unlocks credited to LP margin                   |
/// | 11   | Verify total withdrawn ≈ $5k + vault profit      | —                                               |
#[tokio::test]
async fn vault_lp_lifecycle() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    // Register oracle prices: ETH = $2,000, USDC = $1.
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // -------------------------------------------------------------------------
    // Step 1: LP (user1) deposits $10,000 and adds $5,000 liquidity.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Vault(perps::VaultMsg::AddLiquidity {
                amount: UsdValue::new_int(5_000),
                min_shares_to_mint: None,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let lp_state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert!(
        lp_state.vault_shares > Uint128::ZERO,
        "LP should have shares"
    );
    assert_eq!(lp_state.margin, UsdValue::new_int(5_000), "LP margin = $5k");

    let total_shares = lp_state.vault_shares;

    let vault_state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: contracts.perps,
            },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(
        vault_state.margin,
        UsdValue::new_int(5_000),
        "vault margin = $5,000"
    );

    // -------------------------------------------------------------------------
    // Step 2: Configure: enable vault market-making.
    // vault_total_weight = 1, vault_liquidity_weight = 1, vault_half_spread = 5%,
    // vault_max_quote_size = 2 ETH (small to keep math tractable).
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: Param {
                    vault_total_weight: Dimensionless::new_int(1),
                    ..default_param()
                },
                pair_params: btree_map! {
                    pair.clone() => PairParam {
                        vault_liquidity_weight: Dimensionless::new_int(1),
                        vault_half_spread: Dimensionless::new_permille(50), // 5%
                        vault_max_quote_size: Quantity::new_int(2),
                        ..default_pair_param()
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 3: Call OnOracleUpdate so the vault places bid+ask.
    // -------------------------------------------------------------------------

    suite.make_empty_block().await;

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshIndexPrices {}),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshVaultOrders {}),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Vault should have orders on the book.
    let vault_orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: contracts.perps,
            },
        )
        .should_succeed();

    let vault_bids: Vec<_> = vault_orders
        .values()
        .filter(|o| o.size.is_positive())
        .collect();
    let vault_asks: Vec<_> = vault_orders
        .values()
        .filter(|o| o.size.is_negative())
        .collect();

    assert!(
        !vault_bids.is_empty(),
        "vault should have a bid on the book"
    );
    assert!(
        !vault_asks.is_empty(),
        "vault should have an ask on the book"
    );

    // Vault bid = $2,000 * (1 - 5%) = $1,900, ask = $2,000 * (1 + 5%) = $2,100.
    let bid_price = vault_bids[0].limit_price;
    assert_eq!(bid_price, UsdPrice::new_int(1_900));

    let vault_bid_size = vault_bids[0].size;

    // -------------------------------------------------------------------------
    // Step 4: Taker (user2) deposits $10k, market sells into vault's bid.
    // -------------------------------------------------------------------------
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: vault_bid_size.checked_neg().unwrap(), // sell
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

    // Vault should now have a long position.
    let vault_state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: contracts.perps,
            },
        )
        .should_succeed()
        .unwrap();
    let vault_pos = vault_state
        .positions
        .get(&pair)
        .expect("vault should have a position");

    assert!(vault_pos.size.is_positive(), "vault should be long");

    let vault_long_size = vault_pos.size;
    let vault_margin_after_buy = vault_state.margin;

    // -------------------------------------------------------------------------
    // Step 5: Oracle → $2,200. Vault's long has unrealized profit.
    // -------------------------------------------------------------------------

    register_oracle_prices(&mut suite, &mut accounts, 2_200).await;

    // -------------------------------------------------------------------------
    // Step 6: OnOracleUpdate at $2,200 → vault refreshes orders.
    // -------------------------------------------------------------------------

    suite.make_empty_block().await;

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshIndexPrices {}),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshVaultOrders {}),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 7: Taker buys back from vault's ask → vault realizes PnL.
    // Vault ask ≈ $2,200 * 1.05 = $2,310.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: vault_long_size, // buy same amount (closes taker's short)
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

    // Vault should have realized PnL → margin increased above pre-trade level.
    let vault_state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: contracts.perps,
            },
        )
        .should_succeed()
        .unwrap();

    assert!(
        vault_state.margin > vault_margin_after_buy,
        "vault margin should increase after realized PnL"
    );

    // -------------------------------------------------------------------------
    // Step 8: LP removes half shares. Share price includes realized profit.
    // -------------------------------------------------------------------------

    let half_shares = total_shares / Uint128::new(2);

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Vault(perps::VaultMsg::RemoveLiquidity {
                shares_to_burn: half_shares,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let lp_state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let unlock1_amount = lp_state.unlocks.back().unwrap().amount_to_release;

    // The unlock should be > $2,500 (half of original $5k) because the vault
    // realized profits from the long position.
    assert!(
        unlock1_amount > UsdValue::new_int(2_500),
        "first unlock ({unlock1_amount}) should exceed $2,500 due to realized PnL"
    );

    // -------------------------------------------------------------------------
    // Step 9: LP removes remaining shares.
    // -------------------------------------------------------------------------

    let remaining_shares = lp_state.vault_shares;

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Vault(perps::VaultMsg::RemoveLiquidity {
                shares_to_burn: remaining_shares,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let lp_state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(
        lp_state.unlocks.len(),
        2,
        "LP should have 2 pending unlocks"
    );

    let unlock2_amount = lp_state.unlocks.back().unwrap().amount_to_release;

    // -------------------------------------------------------------------------
    // Step 10: Advance time past cooldown (1 day) so cron processes unlocks.
    // -------------------------------------------------------------------------

    suite.increase_time(Duration::from_days(2)).await;

    // -------------------------------------------------------------------------
    // Step 11: Verify total withdrawn reflects original $5k + vault profits.
    // -------------------------------------------------------------------------

    let lp_state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    // Unlocks should have been processed (queue empty).
    assert!(
        lp_state.unlocks.is_empty(),
        "all unlocks should be processed"
    );

    // Total credited = unlock1 + unlock2.
    let total_unlocked = unlock1_amount.checked_add(unlock2_amount).unwrap();

    assert!(
        total_unlocked > UsdValue::new_int(5_000),
        "total withdrawn ({total_unlocked}) should exceed original $5,000 deposit"
    );

    // The LP's margin should now include the original $5k trading margin
    // plus the unlocked amounts.
    assert!(
        lp_state.margin > UsdValue::new_int(10_000),
        "LP margin ({}) should exceed original $10k (unlocks credited)",
        lp_state.margin
    );
}

/// Verify that an oracle price update succeeds even when the perps contract
/// is in a broken state, and that vault orders remain unchanged.
///
/// Under the old architecture the oracle's `FeedPrices` called perps
/// `VaultMsg::Refresh` via a submessage with `reply_on_error`, so a perps
/// failure had to be explicitly caught to avoid reverting the oracle update.
/// Under the new architecture oracle feeding and perps refreshing are
/// separate transactions injected by the proposal preparer, so a perps
/// failure cannot affect the oracle by construction. The property this test
/// asserts is therefore guaranteed structurally, making the test technically
/// irrelevant, but we keep it since there is no cost.
#[tokio::test]
async fn oracle_triggers_on_oracle_update() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let pair = pair_id();

    // -------------------------------------------------------------------------
    // Setup: Register Pyth price sources and seed USDC + a dummy ETH price.
    // The real ETH price is fed via signed Pyth Lazer messages later.
    // -------------------------------------------------------------------------

    suite
        .seed_oracle_prices(
            &mut accounts.owner,
            btree_map! {
                usdc::DENOM.clone() => OracleTestEntry {
                    pyth_id: 1,
                    humanized_price: UsdPrice::new_int(1),
                },
                pair.clone() => OracleTestEntry {
                    pyth_id: 2,
                    humanized_price: UsdPrice::new_int(1),
                },
            },
        )
        .await;

    // -------------------------------------------------------------------------
    // Setup: Deposit USDC and add vault liquidity (follows vault_lp_lifecycle).
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Vault(perps::VaultMsg::AddLiquidity {
                amount: UsdValue::new_int(5_000),
                min_shares_to_mint: None,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Setup: Configure vault market-making weights.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: Param {
                    vault_total_weight: Dimensionless::new_int(1),
                    ..default_param()
                },
                pair_params: btree_map! {
                    pair.clone() => PairParam {
                        vault_liquidity_weight: Dimensionless::new_int(1),
                        vault_half_spread: Dimensionless::new_permille(50), // 5%
                        vault_max_quote_size: Quantity::new_int(2),
                        ..default_pair_param()
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Vault should have no orders before any price is fed.
    let vault_orders_0: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: contracts.perps,
            },
        )
        .should_succeed();

    assert!(
        vault_orders_0.is_empty(),
        "vault should have no orders before feeding prices"
    );

    // -------------------------------------------------------------------------
    // Step 1: Feed price #1. This triggers OnOracleUpdate via the submessage
    // and the vault should place bid+ask orders.
    // -------------------------------------------------------------------------

    let eth_price = UsdPrice::new(Dec128_6::from_str("2038.056").unwrap());

    suite
        .feed_oracle_prices(
            &mut accounts.owner,
            &[(2, eth_price, MarketSession::Regular)],
            Some(Timestamp::MAX - Duration::from_seconds(1)),
        )
        .await;

    // Oracle price should be set for the perps pair.
    let price1 = suite
        .query_wasm_smart(
            contracts.oracle,
            QueryPriceRequest {
                denom: pair.clone(),
            },
        )
        .unwrap();

    assert!(
        price1.humanized_price > UsdPrice::ZERO,
        "oracle price should be set after feeding"
    );

    // Vault should have orders on the book (placed by OnOracleUpdate).
    let vault_orders_1: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: contracts.perps,
            },
        )
        .should_succeed();

    let vo1_bids: Vec<_> = vault_orders_1
        .values()
        .filter(|o| o.size.is_positive())
        .collect();
    let vo1_asks: Vec<_> = vault_orders_1
        .values()
        .filter(|o| o.size.is_negative())
        .collect();

    assert_eq!(
        vo1_bids.len(),
        1,
        "vault should have exactly 1 bid after OnOracleUpdate"
    );
    assert_eq!(
        vo1_asks.len(),
        1,
        "vault should have exactly 1 ask after OnOracleUpdate"
    );
    assert_eq!(
        vo1_bids[0].pair_id, pair,
        "bid should be for the perps pair"
    );
    assert_eq!(
        vo1_asks[0].pair_id, pair,
        "ask should be for the perps pair"
    );

    // bid = floor(oracle * 0.95) = floor(2038.056 * 0.95) = $1,936
    // ask = ceil(oracle * 1.05) = ceil(2038.056 * 1.05) = $2,140
    //
    // Note that we use $1 tick size in the testing setup. It's not a sensible
    // tick size for production, but it simplifies assertions like this.
    let vo1_bid_price = vo1_bids[0].limit_price;
    let vo1_ask_price = vo1_asks[0].limit_price;
    assert_eq!(vo1_bid_price, UsdPrice::new_int(1_936));
    assert_eq!(vo1_ask_price, UsdPrice::new_int(2_140));

    // |size| = min(half_margin / (oracle * IMR), vault_max_quote_size)
    //        = min(2500 / (2038.056 * 0.1), 2) = min(12.27, 2) = 2
    assert_eq!(vo1_bids[0].size, Quantity::new_int(2));
    assert_eq!(vo1_asks[0].size, Quantity::new_int(-2));

    // -------------------------------------------------------------------------
    // Step 2: Corrupt perps PARAM storage so RefreshVaultOrders will fail on
    // the next invocation (deserialization error).
    // -------------------------------------------------------------------------

    suite.app.db.with_state_storage_mut(|storage| {
        let ns = concat(CONTRACT_NAMESPACE, contracts.perps.as_ref());
        let full_key = concat(&ns, b"param");
        storage.write(&full_key, b"garbled");
    });

    // -------------------------------------------------------------------------
    // Step 3: Feed price #2 without perps refresh. The oracle update succeeds
    // independently. Then verify the perps refresh fails (corrupted storage)
    // but the oracle price is still updated.
    // -------------------------------------------------------------------------

    suite
        .do_oracle_actions(
            &mut accounts.owner,
            None,
            Some((&[(2, eth_price, MarketSession::Regular)], Timestamp::MAX)),
            false,
            false,
        )
        .await;

    // Oracle price should have been updated (new timestamp or value).
    let price2 = suite
        .query_wasm_smart(
            contracts.oracle,
            QueryPriceRequest {
                denom: pair.clone(),
            },
        )
        .unwrap();

    assert!(
        price2.timestamp >= price1.timestamp,
        "oracle price should be updated despite perps failure"
    );

    // Vault orders should be unchanged — we did not call refresh, and even if
    // we tried, it would fail on the corrupted storage. Compare order IDs to
    // prove these are the exact same orders, not new ones at the same price.
    let vault_orders_2: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: contracts.perps,
            },
        )
        .should_succeed();

    assert!(
        vault_orders_1
            .keys()
            .zip(vault_orders_2.keys())
            .all(|(a, b)| a == b),
        "order IDs should be unchanged after skipped refresh"
    );
}

/// Demonstrates a bug where the vault over-commits margin when sizing
/// market-making orders.
///
/// **Root cause** (`vault/refresh.rs:67`): `refresh_orders` uses the raw
/// `vault_state.margin` (deposited collateral) to size orders. It should use
/// available margin (`equity - used_margin`) which accounts for unrealized PnL
/// and margin consumed by existing positions.
///
/// **Current (buggy) behavior**:
/// After the vault accumulates positions from fills and the oracle price moves
/// against those positions, `refresh_orders` still sizes new orders based on
/// the full deposited margin. This causes the vault to place orders far larger
/// than its remaining equity can support. When a taker fills those orders, the
/// vault's total exposure grows until `equity < maintenance_margin` — an
/// unhealthy state that, for a normal user, would trigger liquidation.
///
/// **Expected behavior after fix**:
/// `refresh_orders` should compute the vault's available margin
/// (`equity - used_margin`). When available margin is zero or negative (as it
/// is after the price drop in this test), the vault should place no new orders,
/// keeping its position stable and its equity above the maintenance margin.
///
/// Scenario:
///   1. Vault receives $5,000 margin, market-makes ETH at $2,000
///   2. Taker fills vault bid -> vault goes long 12.5 ETH @ $1,980
///   3. Price drops to $1,700 -> equity=$1,500, available=$0, still healthy
///   4. Vault refreshes using raw margin=$5,000 -> places bid for ~14.7 ETH
///      (correct: available=$0, should place NO orders)
///   5. Taker fills that bid -> vault long ~27 ETH total
///   6. equity(~$1,750) < maintenance_margin(~$2,312) -> UNHEALTHY
///
/// This test asserts the vault is healthy at step 6 and is expected to FAIL
/// under the current code. Once the bug is fixed, this test should pass.
#[tokio::test]
async fn vault_overcommits_margin_after_position_and_price_drop() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let pair = pair_id();

    // Register oracle: ETH = $2,000, USDC = $1.
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // -------------------------------------------------------------------------
    // Step 1: LP (user1) deposits $5,000 USDC and adds all of it as vault
    // liquidity. After this the vault's margin = $5,000.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(5_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Vault(perps::VaultMsg::AddLiquidity {
                amount: UsdValue::new_int(5_000),
                min_shares_to_mint: None,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let vault_state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: contracts.perps,
            },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(vault_state.margin, UsdValue::new_int(5_000));

    // -------------------------------------------------------------------------
    // Step 2: Configure vault market-making.
    //   - vault_half_spread = 1%  -> bid at oracle*(1-1%), ask at oracle*(1+1%)
    //   - vault_max_quote_size = 100 ETH (large, won't constrain)
    //   - IMR = 10%, MMR = 5%
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: Param {
                    vault_total_weight: Dimensionless::new_int(1),
                    ..default_param()
                },
                pair_params: btree_map! {
                    pair.clone() => PairParam {
                        vault_liquidity_weight: Dimensionless::new_int(1),
                        vault_half_spread: Dimensionless::new_permille(10), // 1%
                        vault_max_quote_size: Quantity::new_int(100),
                        ..default_pair_param()
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 3: Refresh vault orders (Round 1, ETH @ $2,000).
    //   half_margin = $2,500
    //   size = $2,500 / ($2,000 * 10%) = 12.5 ETH
    //   bid @ $2,000 * (1 - 1%) = $1,980
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshIndexPrices {}),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshVaultOrders {}),
            Coins::new(),
        )
        .await
        .should_succeed();

    let vault_orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: contracts.perps,
            },
        )
        .should_succeed();

    let vault_bid = vault_orders
        .values()
        .find(|o| o.size.is_positive())
        .expect("vault should have a bid");

    assert_eq!(vault_bid.limit_price, UsdPrice::new_int(1_980));

    let round1_bid_size = vault_bid.size;

    // -------------------------------------------------------------------------
    // Step 4: Taker (user2) deposits $10,000 and market sells into vault bid.
    //   Vault goes long 12.5 ETH @ $1,980.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: round1_bid_size.checked_neg().unwrap(),
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

    // Verify vault has a long position.
    let vault_state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: contracts.perps,
            },
        )
        .should_succeed()
        .unwrap();

    let vault_pos = vault_state
        .positions
        .get(&pair)
        .expect("vault should have a position");
    assert!(vault_pos.size.is_positive(), "vault should be long");

    // -------------------------------------------------------------------------
    // Step 5: Price drops to $1,700 (15% drop).
    //   Unrealized PnL = 12.5 * ($1,700 - $1,980) = -$3,500
    //   Equity = $5,000 - $3,500 = $1,500
    //   Used IM = 12.5 * $1,700 * 10% = $2,125
    //   Available = max(0, $1,500 - $2,125) = $0
    //   MM = 12.5 * $1,700 * 5% = $1,062.50
    //   Vault is STILL HEALTHY ($1,500 >= $1,062.50) but has $0 available.
    // -------------------------------------------------------------------------

    register_oracle_prices(&mut suite, &mut accounts, 1_700).await;

    // Sanity check: vault should be healthy at this point.
    let vault_ext: perps::UserStateExtended = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateExtendedRequest {
                user: contracts.perps,
                include_equity: true,
                include_available_margin: true,
                include_maintenance_margin: false,
                include_unrealized_pnl: false,
                include_unrealized_funding: false,
                include_liquidation_price: false,
                include_all: false,
            },
        )
        .should_succeed();

    let equity_before = vault_ext.equity.unwrap();
    let available_before = vault_ext.available_margin.unwrap();

    let vault_pos = vault_ext.positions.get(&pair).unwrap();
    let mm_before = vault_pos
        .size
        .checked_abs()
        .unwrap()
        .checked_mul(UsdPrice::new_int(1_700))
        .unwrap()
        .checked_mul(Dimensionless::new_permille(50))
        .unwrap();

    assert!(
        equity_before >= mm_before,
        "vault should be healthy before Round 2: equity ({equity_before}) >= MM ({mm_before})"
    );
    assert_eq!(
        available_before,
        UsdValue::ZERO,
        "available margin should be zero — no room for new orders"
    );

    // -------------------------------------------------------------------------
    // Step 6: Refresh vault orders (Round 2, ETH @ $1,700) — THE BUG.
    //   Bug: uses raw margin ~$5,000 -> size = $2,500 / ($1,700 * 10%) = 14.7 ETH
    //   Correct: available = $0 -> should place NO orders.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshIndexPrices {}),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshVaultOrders {}),
            Coins::new(),
        )
        .await
        .should_succeed();

    // With the fix, available margin is $0, so the vault places NO orders.
    // OLD (incorrect) behavior: the vault would place a ~14.7 ETH bid here
    // because it sized orders from raw margin ($5,000) instead of available
    // margin ($0).
    //
    // let vault_bid = vault_orders
    //     .values()
    //     .find(|o| o.size.is_positive())
    //     .expect("vault should have a bid");
    // let round2_bid_size = vault_bid.size;
    let vault_orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: contracts.perps,
            },
        )
        .should_succeed();

    assert!(
        vault_orders.is_empty(),
        "vault should place no orders when available margin is zero"
    );

    // -------------------------------------------------------------------------
    // Step 7: Taker attempts to sell into the vault's (now absent) bid.
    //   With the OLD incorrect logic the vault would have placed a ~14.7 ETH
    //   bid, this fill would succeed, and the vault's total long position
    //   would grow to ~27 ETH — pushing equity below maintenance margin
    //   (unhealthy). With the fix the book is empty, so this market sell
    //   finds no liquidity and is rejected.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: round1_bid_size.checked_neg().unwrap(),
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
        // OLD (incorrect) behavior: the vault had a bid, so this would succeed.
        // .should_succeed();
        .should_fail_with_error("no liquidity");

    // -------------------------------------------------------------------------
    // Step 8: Assert the vault is healthy.
    //   With the OLD incorrect logic the vault would be unhealthy here:
    //   equity (~$1,750) < maintenance_margin (~$2,312).
    //   With the fix the vault's position stays at ~12.5 ETH and equity
    //   remains above maintenance margin.
    // -------------------------------------------------------------------------

    let vault_ext: perps::UserStateExtended = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateExtendedRequest {
                user: contracts.perps,
                include_equity: true,
                include_available_margin: false,
                include_maintenance_margin: false,
                include_unrealized_pnl: false,
                include_unrealized_funding: false,
                include_liquidation_price: false,
                include_all: false,
            },
        )
        .should_succeed();

    let equity = vault_ext.equity.unwrap();

    let vault_pos = vault_ext
        .positions
        .get(&pair)
        .expect("vault should have a position");

    let maintenance_margin = vault_pos
        .size
        .checked_abs()
        .unwrap()
        .checked_mul(UsdPrice::new_int(1_700))
        .unwrap()
        .checked_mul(Dimensionless::new_permille(50)) // MMR = 5%
        .unwrap();

    assert!(
        equity >= maintenance_margin,
        "vault should be healthy: equity ({equity}) >= maintenance_margin ({maintenance_margin})"
    );
}
