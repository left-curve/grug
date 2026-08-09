use {
    crate::{TestAccounts, TestSuiteNaiveWithIndexer},
    dango_genesis::Contracts,
    dango_math::Uint128,
    dango_order_book::{Dimensionless, OrderKind, Quantity, TimeInForce, UsdPrice},
    dango_primitives::{Coins, Denom, ResultExt, btree_map},
    dango_pyth_types::PythId,
    dango_types::{constants::usdc, perps},
};

pub fn pair_id() -> Denom {
    "perp/ethusd".parse().unwrap()
}

/// Specification for a single oracle price entry used in tests.
///
/// `pyth_id` is the synthetic Pyth Lazer feed ID under which the price is
/// stored. Tests can use any `u32`; the value only has to be unique across
/// entries within the same test environment.
pub struct OracleTestEntry {
    pub pyth_id: PythId,
    pub humanized_price: UsdPrice,
}

/// Common setup: register oracle prices + deposit margin for user1 and user2.
pub async fn setup_perps_env(
    suite: &mut TestSuiteNaiveWithIndexer,
    accounts: &mut TestAccounts,
    contracts: &Contracts,
    eth_price: u128,
    margin_per_user: u128,
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

    for account in [&mut accounts.user1, &mut accounts.user2] {
        let amount = Uint128::new(margin_per_user * 1_000_000);
        suite
            .execute(
                account,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
                Coins::one(usdc::DENOM.clone(), amount).unwrap(),
            )
            .await
            .should_succeed();
    }
}

/// Place a limit ask (user2) then a market buy (user1) to produce an
/// `OrderFilled` at the given price and size.
pub async fn create_perps_fill(
    suite: &mut TestSuiteNaiveWithIndexer,
    accounts: &mut TestAccounts,
    contracts: &Contracts,
    pair_id: &Denom,
    price: u128,
    size: u128,
) {
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id.clone(),
                size: Quantity::new_int(-(size as i128)),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(price as i128),
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

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id.clone(),
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
