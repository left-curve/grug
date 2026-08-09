use {
    dango_genesis::{AccountOption, GenesisOption, GenesisUser, GrugOption},
    dango_math::{Udec128, Uint128},
    dango_primitives::{Addressable, Coins, Duration, HashExt, Message, ResultExt},
    dango_testing::{
        Preset, TestOption, owner, setup_test_naive_with_custom_genesis, user1, user2, user3,
        user4, user5, user6, user7, user8, user9,
    },
    dango_types::{account_factory::NewUserSalt, auth::Key, constants::dango},
    test_case::test_case,
};

/// The gas fee rate used by these tests: 0.25 units of `gas_token` per unit of
/// gas.
const FEE_RATE: Udec128 = Udec128::new_percent(25);

/// Build a custom genesis where the gas token is DANGO and the gas fee rate is
/// non-zero, with `user1` funded with `sender_balance` and all other genesis
/// users (including the owner) starting empty.
fn gas_genesis(sender_balance: u128) -> GenesisOption {
    let genesis_user = |key: dango_types::auth::Key, seed: u32, balance: u128| GenesisUser {
        salt: NewUserSalt {
            key,
            key_hash: match &key {
                Key::Secp256k1(pk) => pk.sha2_256(),
                _ => unreachable!("test genesis users use secp256k1 keys"),
            },
            seed,
        },
        dango_balance: Uint128::new(balance),
    };

    GenesisOption {
        grug: GrugOption {
            owner_index: 0,
            gas_token: dango::DENOM.clone(),
            gas_fee_rate: FEE_RATE,
            max_orphan_age: Duration::from_weeks(1),
        },
        account: AccountOption {
            genesis_users: vec![
                genesis_user(Key::Secp256k1(owner::PUBLIC_KEY.into()), 0, 0),
                genesis_user(Key::Secp256k1(user1::PUBLIC_KEY.into()), 1, sender_balance),
                genesis_user(Key::Secp256k1(user2::PUBLIC_KEY.into()), 2, 0),
                genesis_user(Key::Secp256k1(user3::PUBLIC_KEY.into()), 3, 0),
                genesis_user(Key::Secp256k1(user4::PUBLIC_KEY.into()), 4, 0),
                genesis_user(Key::Secp256k1(user5::PUBLIC_KEY.into()), 5, 0),
                genesis_user(Key::Secp256k1(user6::PUBLIC_KEY.into()), 6, 0),
                genesis_user(Key::Secp256k1(user7::PUBLIC_KEY.into()), 7, 0),
                genesis_user(Key::Secp256k1(user8::PUBLIC_KEY.into()), 8, 0),
                genesis_user(Key::Secp256k1(user9::PUBLIC_KEY.into()), 9, 0),
            ],
            ..Preset::preset_test()
        },
        ..Preset::preset_test()
    }
}

// A sender attempts a token transfer with various gas limits and transfer
// amounts. Depending on these, the transaction may fail either during fee
// withholding or while processing the message. We check the outcome and the
// account balances afterwards.
//
// Unlike the previous (taxman-based) behavior, there is no gas refund: the
// sender pays exactly `ceil(gas_limit * gas_fee_rate)`, regardless of how much
// gas is actually used. The fee is credited to the chain's owner.
//
// Case 1. Sender can afford the transfer but not the gas fee.
// The tx fails during fee withholding; no state change is committed.
#[test_case(
    10,
    1,
    100_000,
    0,
    10,
    0,
    Some("subtraction overflow: 10 - 25000");
    "error while withholding fee"
)]
// Case 2. Sender can afford the gas fee but not the transfer.
// The fee is withheld (and kept), but the transfer is reverted.
#[test_case(
    30_000,
    99_999,
    100_000,
    25_000, // = 100,000 * 0.25
    5_000,  // = 30,000 - (100,000 * 0.25)
    0,
    Some("subtraction overflow: 5000 - 99999");
    "error while processing messages"
)]
// Case 3. Sender can afford both the gas fee and the transfer.
#[test_case(
    30_000,
    123,
    100_000,
    25_000, // = 100,000 * 0.25
    4_877,  // = 30,000 - (100,000 * 0.25) - 123
    123,
    None;
    "successful tx"
)]
#[tokio::test]
async fn gas_fee_charging_works(
    sender_balance_before: u128,
    send_amount: u128,
    gas_limit: u64,
    owner_balance_after: u128,
    sender_balance_after: u128,
    receiver_balance_after: u128,
    maybe_err: Option<&str>,
) {
    let (mut suite, mut accounts, ..) = setup_test_naive_with_custom_genesis(
        TestOption {
            bridge_ops: |_| vec![],
            ..TestOption::default()
        },
        gas_genesis(sender_balance_before),
    );

    let to = accounts.user9.address();

    let outcome = suite
        .send_message_with_gas(
            &mut accounts.user1,
            gas_limit,
            Message::transfer(to, Coins::one(dango::DENOM.clone(), send_amount).unwrap()).unwrap(),
        )
        .await;

    match maybe_err {
        Some(err) => {
            outcome.should_fail_with_error(err);
        },
        None => {
            outcome.should_succeed();
        },
    }

    // The fee is credited to the chain owner.
    suite
        .query_balance(&accounts.owner, dango::DENOM.clone())
        .should_succeed_and_equal(Uint128::new(owner_balance_after));
    suite
        .query_balance(&accounts.user1, dango::DENOM.clone())
        .should_succeed_and_equal(Uint128::new(sender_balance_after));
    suite
        .query_balance(&accounts.user9, dango::DENOM.clone())
        .should_succeed_and_equal(Uint128::new(receiver_balance_after));
}
