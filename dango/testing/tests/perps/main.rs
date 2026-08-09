use {
    dango_order_book::{Dimensionless, Quantity, UsdPrice},
    dango_primitives::{Duration, btree_map},
    dango_testing::{OracleTestEntry, TestAccounts, TestSuiteNaive, pair_id},
    dango_types::{
        constants::usdc,
        perps::{PairParam, Param, RateSchedule},
    },
};

mod adl_bug_reproduction;
mod bankruptcy_bug_reproduction;
mod batch_update_orders;
mod client_order_id;
mod conditional_orders;
mod index_price;
mod liquidation;
mod liquidation_spec;
mod price_band;
mod reduce_only;
mod referral;
mod trading;
mod vault;
mod vault_snapshots;
mod vault_withdrawal_health;
mod wind_down;

/// Return the genesis-default global params (mirrors `PerpsOption::preset_test()`).
pub fn default_param() -> Param {
    Param {
        taker_fee_rates: RateSchedule {
            base: Dimensionless::new_permille(1), // 0.1%
            ..Default::default()
        },
        protocol_fee_rate: Dimensionless::ZERO,
        liquidation_fee_rate: Dimensionless::new_permille(10), // 1%
        vault_cooldown_period: Duration::from_days(1),
        max_unlocks: 10,
        max_open_orders: 100,
        funding_period: Duration::from_hours(1),
        max_action_batch_size: 5,
        ..Default::default()
    }
}

/// Return the genesis-default pair params (mirrors `PerpsOption::preset_test()`).
pub fn default_pair_param() -> PairParam {
    PairParam {
        initial_margin_ratio: Dimensionless::new_permille(100), // 10%
        maintenance_margin_ratio: Dimensionless::new_permille(50), // 5%
        tick_size: UsdPrice::new_int(1),
        max_abs_oi: Quantity::new_int(1_000_000),
        ..PairParam::new_mock()
    }
}

/// Register fixed oracle prices for the perps pair and settlement currency.
pub async fn register_oracle_prices(
    suite: &mut TestSuiteNaive,
    accounts: &mut TestAccounts,
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
