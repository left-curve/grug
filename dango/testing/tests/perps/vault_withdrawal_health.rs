use {
    crate::{default_pair_param, default_param, register_oracle_prices},
    dango_math::{MultiplyRatio, NumberConst, Uint128},
    dango_order_book::{
        Dimensionless, OrderId, OrderKind, Quantity, QueryOrdersByUserResponseItem, UsdPrice,
        UsdValue,
    },
    dango_primitives::{Addressable, Coins, QuerierExt, ResultExt, btree_map},
    dango_testing::{TestOption, pair_id, setup_test_naive},
    dango_types::{
        constants::usdc,
        perps::{self, PairParam, Param},
    },
    std::collections::BTreeMap,
};

/// Regression test: vault withdrawal pushes vault into liquidatable state
/// due to maintenance margin requirement of large open positions.
///
/// The vault does not need a negative PnL for this to happen. The withdrawer
/// simply needs to withdraw such that remaining equity < maintenance margin.
/// This test demonstrates this when the vault's position is at break even
/// (oracle = entry price, PnL = 0).
///
/// The root cause is that the vault only asserts it has enough _raw_ margin to
/// honor the withdrawal. This ignores that portions of the raw margin is used
/// to maintain open positions and open orders. The correct approach is to assert
/// the vault has sufficient _available_ margin instead.
///
/// | Step | Action                                          | Key numbers                                                   | Assert                           |
/// | ---- | ----------------------------------------------- | ------------------------------------------------------------- | -------------------------------- |
/// | 1    | LP deposits $5k, adds $5k vault liquidity       | vault margin=$5,000; ~5B shares minted                        | shares > 0, vault margin = $5k   |
/// | 2    | Configure vault MM (5% spread, max 20 ETH)      | bid=$1,900, ask=$2,100                                        | —                                |
/// | 3    | Refresh vault orders                            | vault places bid + ask                                        | vault bid at $1,900              |
/// | 4    | Taker sells into vault bid                      | vault goes long; margin ~$5,000                               | vault has long position          |
/// | 5    | Oracle drops to $1,900 (breakeven)              | PnL=0; equity≈$5,000; MM≈$1,188                               | vault healthy (equity > MM)      |
/// | 6    | LP burns ~85% of shares                         | release≈$4,250; old margin check passes; equity→≈$750         | withdraw rejected (fix)          |
#[tokio::test]
async fn vault_withdrawal_at_breakeven_makes_vault_liquidatable() {
    // ---- Step 0: Setup ----

    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    let pair = pair_id();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // -------------------------------------------------------------------------
    // Step 1: LP (user1) deposits $5,000 USDC and adds $5,000 as vault liquidity.
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

    let lp_state: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let lp_shares = lp_state.vault_shares;
    assert!(lp_shares > Uint128::ZERO, "LP should have vault shares");

    // -------------------------------------------------------------------------
    // Step 2: Configure vault market-making: 5% half-spread, max 20 ETH per side.
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
                        vault_max_quote_size: Quantity::new_int(20),
                        ..default_pair_param()
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 3: Refresh vault orders. Expect bid at $1,900 (= $2,000 * 0.95).
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
    assert!(
        !vault_bids.is_empty(),
        "vault should have a bid on the book"
    );
    assert_eq!(
        vault_bids[0].limit_price,
        UsdPrice::new_int(1_900),
        "vault bid should be at $1,900"
    );

    let vault_bid_size = vault_bids[0].size;

    // -------------------------------------------------------------------------
    // Step 4: Taker (user2) deposits $10,000 and market sells into vault's bid.
    //
    // Vault goes long 20 ETH at $1,900.
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
                    max_slippage: Dimensionless::new_percent(10),
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
    let vault_state: perps::UserState = suite
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
        .expect("vault should have ETH position");
    assert!(vault_pos.size.is_positive(), "vault should be long ETH");

    // -------------------------------------------------------------------------
    // Step 5: Oracle moves to $1,900 (= entry price → breakeven).
    //
    // Unrealized PnL = 20 * ($1,900 - $1,900) = $0
    // Vault equity  ≈ $5,000 (margin, no PnL)
    // MM            = 20 * $1,900 * 5%  = $1,900
    // IMR           = 20 * $1,900 * 10% = $3,800
    // Vault is healthy: $5,000 > $1,900.
    // -------------------------------------------------------------------------

    register_oracle_prices(&mut suite, &mut accounts, 1_900).await;

    let vault_ext: perps::UserStateExtended = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateExtendedRequest {
                user: contracts.perps,
                include_equity: true,
                include_maintenance_margin: true,
                include_available_margin: false,
                include_unrealized_pnl: true,
                include_unrealized_funding: false,
                include_liquidation_price: false,
                include_all: false,
            },
        )
        .should_succeed();

    let equity_before = vault_ext.equity.unwrap();
    let mm_before = vault_ext.maintenance_margin.unwrap();
    assert!(
        equity_before > mm_before,
        "vault should be healthy BEFORE withdrawal: equity={equity_before}, MM={mm_before}"
    );
    // Confirm breakeven: equity ≈ margin (no unrealized PnL).
    let vault_state_bev: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: contracts.perps,
            },
        )
        .should_succeed()
        .unwrap();
    assert_eq!(
        equity_before, vault_state_bev.margin,
        "vault should be at breakeven (equity = margin)"
    );

    // -------------------------------------------------------------------------
    // Step 6: LP burns ~85% of shares.
    //
    // amount_to_release ≈ 85% of equity.
    // Old raw-margin check: margin >= amount → passes.
    //
    // Post-withdrawal equity drops well below MM → vault is liquidatable.
    // -------------------------------------------------------------------------

    let shares_to_burn = lp_shares
        .checked_multiply_ratio_floor(Uint128::new(85), Uint128::new(100))
        .unwrap();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Vault(perps::VaultMsg::RemoveLiquidity { shares_to_burn }),
            Coins::new(),
        )
        .await
        .should_fail_with_error("insufficient vault available margin to cover withdrawal");
}
