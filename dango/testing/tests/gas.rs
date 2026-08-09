use {
    dango_math::{MultiplyFraction, Udec128, Uint128},
    dango_primitives::{
        Addressable, Coins, Inner, Json, Message, QuerierExt, ResultExt, btree_map,
    },
    dango_testing::{BalanceChange, setup_test_naive},
    dango_types::constants::usdc,
};

const OLD_FEE_RATE: Udec128 = Udec128::new_percent(1); // 0.01 uusdc per gas unit
const NEW_FEE_RATE: Udec128 = Udec128::new_percent(2); // 0.02 uusdc per gas unit

const GAS: u64 = 50_000;

/// A change to the chain's `gas_fee_rate` must not affect the transaction that
/// makes the change, but must take effect for subsequent transactions.
///
/// We observe this through a non-owner account (`user1`): the gas fee is
/// credited to the chain owner, so a fee charged on an owner-sent transaction
/// would be paid to the owner itself (a no-op) and thus unobservable.
#[tokio::test]
async fn gas_fee_rate_update_works() {
    let (mut suite, mut accounts, ..) = setup_test_naive(Default::default());

    let user2 = accounts.user2.address();

    // The gas fee rate starts at zero. Raise it to `OLD_FEE_RATE`.
    let mut new_cfg = suite.query_config().unwrap();
    new_cfg.gas_fee_rate = OLD_FEE_RATE;
    suite
        .configure_with_gas::<Json>(&mut accounts.owner, GAS, Some(new_cfg), None)
        .await
        .should_succeed();

    // A subsequent transaction by `user1` is charged at `OLD_FEE_RATE`, on the
    // requested gas limit (there is no refund of unused gas).
    suite.balances().record(&accounts.user1);
    suite
        .send_message_with_gas(
            &mut accounts.user1,
            GAS,
            Message::transfer(user2, Coins::new()).unwrap(),
        )
        .await
        .should_succeed();

    let fee_old = Uint128::new(GAS as u128)
        .checked_mul_dec_ceil(OLD_FEE_RATE)
        .unwrap();
    suite.balances().should_change(
        &accounts.user1,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Decreased(fee_old.into_inner()),
        },
    );

    // The owner raises the rate to `NEW_FEE_RATE`.
    let mut new_cfg = suite.query_config().unwrap();
    new_cfg.gas_fee_rate = NEW_FEE_RATE;
    suite
        .configure_with_gas::<Json>(&mut accounts.owner, GAS, Some(new_cfg), None)
        .await
        .should_succeed();

    // The next transaction by `user1` is charged at the new rate.
    suite.balances().record(&accounts.user1);
    suite
        .send_message_with_gas(
            &mut accounts.user1,
            GAS,
            Message::transfer(user2, Coins::new()).unwrap(),
        )
        .await
        .should_succeed();

    let fee_new = Uint128::new(GAS as u128)
        .checked_mul_dec_ceil(NEW_FEE_RATE)
        .unwrap();
    suite.balances().should_change(
        &accounts.user1,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Decreased(fee_new.into_inner()),
        },
    );
}
