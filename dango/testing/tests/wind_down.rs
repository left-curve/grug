//! The contract-side half of the wind-down.
//!
//! The chain upgrade turns token transfers off and hands the chain owner a way
//! to push each remaining balance back across the bridge. These are the
//! behaviors that flip with it: everything that could strand funds stops, and
//! everything that gets funds out keeps working.

use {
    dango_genesis::{AccountOption, GenesisOption},
    dango_hyperlane_types::Addr32,
    dango_math::{IsZero, NumberConst, Uint128},
    dango_primitives::{
        Addr, Addressable, CheckedContractEvent, Coins, Inner, JsonDeExt, Message, Op, QuerierExt,
        ResultExt, SearchEvent, TxEvents, addr, btree_map, coins,
    },
    dango_testing::{
        BalanceChange, HyperlaneTestSuite, Preset, TestOption, mock_arbitrum, mock_ethereum,
        setup_test_naive, setup_test_naive_with_custom_genesis,
    },
    dango_types::{
        account, bank,
        constants::usdc,
        gateway::{self, Remote, WithdrawalResponse},
    },
};

/// The error every gated transfer must be rejected with.
const DISABLED: &str = "token transfers are disabled";

/// USDC withdrawal fee on the Ethereum Warp route, mirroring the test genesis
/// at `dango/testing/src/genesis.rs`; keep the two in sync.
const ETHEREUM_USDC_WITHDRAWAL_FEE: u128 = 1_000_000;

/// An address at which no contract was ever instantiated.
const DEAD: Addr = addr!("000000000000000000000000000000000000dead");

fn ethereum_usdc() -> Remote {
    Remote::Warp {
        domain: mock_ethereum::DOMAIN,
        contract: mock_ethereum::USDC_WARP,
    }
}

fn arbitrum_usdc() -> Remote {
    Remote::Warp {
        domain: mock_arbitrum::DOMAIN,
        contract: mock_arbitrum::USDC_WARP,
    }
}

/// A remote for which no route is configured.
fn unrouted() -> Remote {
    Remote::Bitcoin
}

fn recipient() -> Addr32 {
    Addr::mock(201).into()
}

/// The ID of the withdrawal request a transaction filed.
///
/// `ForceWithdrawal` reaches the gateway as a submessage, so the event lands in
/// the account's transaction. Reading it back beats hardcoding an ID, which
/// would silently depend on where the request counter starts.
fn withdrawal_id(events: &TxEvents, gateway: Addr) -> u64 {
    events
        .clone()
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| e.contract == gateway && e.ty == "withdrawal_requested")
        .take()
        .one()
        .event
        .data
        .deserialize_json::<gateway::WithdrawalRequested>()
        .unwrap()
        .id
}

// -------------------------- turning transfers off ----------------------------

#[tokio::test]
async fn set_transfers_enabled_is_owner_only() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    suite
        .query_wasm_smart(contracts.bank, bank::QueryTransfersEnabledRequest {})
        .should_succeed_and_equal(true);

    suite
        .execute(
            &mut accounts.user1,
            contracts.bank,
            &bank::ExecuteMsg::SetTransfersEnabled(false),
            Coins::new(),
        )
        .await
        .should_fail_with_error("you don't have the right");

    suite
        .query_wasm_smart(contracts.bank, bank::QueryTransfersEnabledRequest {})
        .should_succeed_and_equal(true);

    suite
        .execute(
            &mut accounts.owner,
            contracts.bank,
            &bank::ExecuteMsg::SetTransfersEnabled(false),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(contracts.bank, bank::QueryTransfersEnabledRequest {})
        .should_succeed_and_equal(false);
}

// ------------------------------ what's blocked -------------------------------

/// Peer-to-peer transfers are how a user strands funds, either by sending to
/// an address that doesn't exist or by sending to a contract that can't spend
/// them. All of them stop.
#[tokio::test]
async fn disabling_blocks_plain_transfers() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    disable_transfers(&mut suite, &mut accounts.owner, contracts.bank).await;

    suite
        .transfer(
            &mut accounts.user1,
            accounts.user2.address(),
            coins! { usdc::DENOM.clone() => 100 },
        )
        .await
        .should_fail_with_error(DISABLED);

    suite
        .batch_transfer(
            &mut accounts.user1,
            btree_map! {
                accounts.user2.address() => coins! { usdc::DENOM.clone() => 100 },
                accounts.user3.address() => coins! { usdc::DENOM.clone() => 200 },
            },
        )
        .await
        .should_fail_with_error(DISABLED);

    suite
        .transfer(
            &mut accounts.user1,
            DEAD,
            coins! { usdc::DENOM.clone() => 100 },
        )
        .await
        .should_fail_with_error(DISABLED);

    // Nothing was stranded in the bank on the way.
    suite
        .query_wasm_smart(
            contracts.bank,
            bank::QueryOrphanedTransfersRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and(|orphans| orphans.is_empty());
}

/// Funds attached to an `Execute` message are a transfer too, so they are
/// blocked everywhere except on the way to the gateway.
#[tokio::test]
async fn disabling_blocks_funds_attached_to_other_contracts() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    disable_transfers(&mut suite, &mut accounts.owner, contracts.bank).await;

    suite
        .send_message(
            &mut accounts.user1,
            Message::execute(
                contracts.account_factory,
                &dango_types::account_factory::ExecuteMsg::UpdateUsername(
                    "someone".parse().unwrap(),
                ),
                coins! { usdc::DENOM.clone() => 100 },
            )
            .unwrap(),
        )
        .await
        .should_fail_with_error(DISABLED);
}

// ------------------------------ what still works -----------------------------

/// The whole point of the exception: a user must still be able to get their
/// funds off the chain.
#[tokio::test]
async fn disabling_still_allows_a_full_withdrawal() {
    let (suite, mut accounts, _, contracts, valset) = setup_test_naive(TestOption::default());
    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    disable_transfers(&mut suite, &mut accounts.owner, contracts.bank).await;

    let supply_before = suite.query_supply(usdc::DENOM.clone()).unwrap();

    suite.balances().record_many([
        &accounts.user1.address(),
        &accounts.owner.address(),
        &contracts.gateway,
    ]);

    let withdrawn = 50_000_000;

    let (mut user1, mut owner) = (accounts.user1.clone(), accounts.owner.clone());

    suite
        .transfer_remote(
            &mut user1,
            &mut owner,
            contracts.gateway,
            ethereum_usdc(),
            recipient(),
            coins! { usdc::DENOM.clone() => withdrawn },
        )
        .await
        .should_succeed();

    // The user paid the full amount; the fee went to the owner; the rest was
    // burned on its way across the bridge.
    suite.balances().should_change(
        &accounts.user1,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Decreased(withdrawn),
        },
    );

    suite.balances().should_change(
        &accounts.owner,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Increased(ETHEREUM_USDC_WITHDRAWAL_FEE),
        },
    );

    // The gateway holds nothing afterwards: escrow in, burn and fee out.
    suite
        .balances()
        .should_change(&contracts.gateway, btree_map! {});

    assert_eq!(
        suite.query_supply(usdc::DENOM.clone()).unwrap(),
        supply_before - Uint128::new(withdrawn - ETHEREUM_USDC_WITHDRAWAL_FEE)
    );
}

/// A rejected withdrawal refunds the user out of the gateway's escrow, which
/// is another transfer that must survive.
#[tokio::test]
async fn disabling_still_allows_a_withdrawal_refund() {
    let (suite, mut accounts, _, contracts, valset) = setup_test_naive(TestOption::default());
    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    disable_transfers(&mut suite, &mut accounts.owner, contracts.bank).await;

    suite.balances().record(&accounts.user1.address());

    let id = suite
        .request_transfer_remote(
            &mut accounts.user1,
            contracts.gateway,
            ethereum_usdc(),
            recipient(),
            coins! { usdc::DENOM.clone() => 50_000_000 },
        )
        .await;

    suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            id,
            WithdrawalResponse::Reject,
        )
        .await
        .should_succeed();

    suite
        .balances()
        .should_change(&accounts.user1, btree_map! {});
}

/// Deposits keep landing, so a user who bridges in during the wind-down is not
/// left with a message the chain refuses to deliver.
#[tokio::test]
async fn disabling_still_allows_inbound_deposits() {
    let (suite, mut accounts, _, contracts, valset) = setup_test_naive(TestOption::default());
    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    disable_transfers(&mut suite, &mut accounts.owner, contracts.bank).await;

    suite.balances().record(&accounts.user2.address());

    let (mut user1, user2) = (accounts.user1.clone(), accounts.user2.clone());

    suite
        .receive_warp_transfer(
            &mut user1,
            mock_ethereum::DOMAIN,
            mock_ethereum::USDC_WARP,
            &user2,
            123_456,
        )
        .await
        .should_succeed();

    suite.balances().should_change(
        &accounts.user2,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Increased(123_456),
        },
    );
}

/// A deposit addressed to an account that was never created would strand the
/// tokens in the bank all over again, so the delivery is refused instead.
#[tokio::test]
async fn disabling_blocks_a_deposit_to_a_missing_account() {
    let (suite, mut accounts, _, contracts, valset) = setup_test_naive(TestOption::default());
    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    disable_transfers(&mut suite, &mut accounts.owner, contracts.bank).await;

    let mut user1 = accounts.user1.clone();

    suite
        .receive_warp_transfer(
            &mut user1,
            mock_ethereum::DOMAIN,
            mock_ethereum::USDC_WARP,
            &DEAD,
            123_456,
        )
        .await
        .should_fail_with_error("can't send to the non-existent address");

    suite
        .query_wasm_smart(
            contracts.bank,
            bank::QueryOrphanedTransfersRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and(|orphans| orphans.is_empty());
}

// ------------------------------ force withdrawal -----------------------------

#[tokio::test]
async fn force_withdrawal_is_chain_owner_only() {
    let (mut suite, mut accounts, _, _contracts, _) = setup_test_naive(TestOption::default());

    let target = accounts.user2.address();

    // A different user can't do it...
    suite
        .execute(
            &mut accounts.user1,
            target,
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: ethereum_usdc(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error("you don't have the right");

    // ...and neither can the account's own user.
    suite
        .execute(
            &mut accounts.user2,
            target,
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: ethereum_usdc(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error("you don't have the right");
}

/// The account's whole balance of the denom is escrowed, and the request
/// records the user it came from, not the owner who triggered it.
#[tokio::test]
async fn force_withdrawal_sends_the_entire_balance() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let target = accounts.user2.address();
    let balance = suite
        .query_balance(&accounts.user2, usdc::DENOM.clone())
        .unwrap();

    assert!(balance.is_non_zero());

    suite.balances().record_many([&target, &contracts.gateway]);

    let events = suite
        .execute(
            &mut accounts.owner,
            target,
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: ethereum_usdc(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    let id = withdrawal_id(&events, contracts.gateway);

    suite.balances().should_change(
        &accounts.user2,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Decreased(balance.into_inner()),
        },
    );

    suite.balances().should_change(
        &contracts.gateway,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Increased(balance.into_inner()),
        },
    );

    let request = suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(request.user, target);
    assert_eq!(request.coin.amount, balance);
    assert_eq!(request.remote, ethereum_usdc());
    assert_eq!(request.recipient, recipient());
}

/// Approving the request completes the bridge, which is what actually returns
/// the money.
#[tokio::test]
async fn force_withdrawal_then_approve_bridges() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let target = accounts.user2.address();
    let balance = suite
        .query_balance(&accounts.user2, usdc::DENOM.clone())
        .unwrap();
    let supply_before = suite.query_supply(usdc::DENOM.clone()).unwrap();

    suite
        .balances()
        .record_many([&accounts.owner.address(), &contracts.gateway]);

    let events = suite
        .execute(
            &mut accounts.owner,
            target,
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: ethereum_usdc(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    let id = withdrawal_id(&events, contracts.gateway);

    suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            id,
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed();

    // The bridged amount is burned; the fee stays with the owner.
    assert_eq!(
        suite.query_supply(usdc::DENOM.clone()).unwrap(),
        supply_before - balance + Uint128::new(ETHEREUM_USDC_WITHDRAWAL_FEE)
    );

    suite.balances().should_change(
        &accounts.owner,
        btree_map! {
            usdc::DENOM.clone() => BalanceChange::Increased(ETHEREUM_USDC_WITHDRAWAL_FEE),
        },
    );

    suite
        .balances()
        .should_change(&contracts.gateway, btree_map! {});
}

#[tokio::test]
async fn force_withdrawal_of_zero_balance_fails() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });

    let target = accounts.user2.address();

    suite
        .execute(
            &mut accounts.owner,
            target,
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: ethereum_usdc(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error("holds no bridge/usdc");

    // Nothing was filed.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest { id: 0 },
        )
        .should_succeed_and_equal(None);
}

/// The account holds USDC but no ETH, so asking for ETH must fail rather than
/// escrow nothing.
#[tokio::test]
async fn force_withdrawal_of_an_unheld_denom_fails() {
    let (mut suite, mut accounts, _, _, _) = setup_test_naive(TestOption::default());

    let target = accounts.user3.address();

    suite
        .execute(
            &mut accounts.owner,
            target,
            &account::ExecuteMsg::ForceWithdrawal {
                denom: dango_types::constants::eth::DENOM.clone(),
                remote: Remote::Warp {
                    domain: mock_ethereum::DOMAIN,
                    contract: mock_ethereum::ETH_WARP,
                },
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error("holds no bridge/eth");
}

/// A remote with no route fails fast inside the gateway, and the whole
/// transaction unwinds, so the escrow never happens.
#[tokio::test]
async fn force_withdrawal_with_an_unrouted_remote_fails() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let target = accounts.user2.address();

    suite.balances().record_many([&target, &contracts.gateway]);

    suite
        .execute(
            &mut accounts.owner,
            target,
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: unrouted(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_fail();

    suite
        .balances()
        .should_change(&accounts.user2, btree_map! {});
    suite
        .balances()
        .should_change(&contracts.gateway, btree_map! {});
}

/// A balance that doesn't cover the route's fee leaves nothing to bridge, so
/// the gateway rejects it and nothing is escrowed.
#[tokio::test]
async fn force_withdrawal_below_the_fee_fails() {
    let (suite, mut accounts, _, contracts, valset) = setup_test_naive(TestOption {
        bridge_ops: |_| vec![],
        ..TestOption::default()
    });
    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    // Fund the account with less than the Arbitrum route's 10,000-unit fee.
    let (mut user1, user2) = (accounts.user1.clone(), accounts.user2.clone());

    suite
        .receive_warp_transfer(
            &mut user1,
            mock_ethereum::DOMAIN,
            mock_ethereum::USDC_WARP,
            &user2,
            10_000,
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            user2.address(),
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: arbitrum_usdc(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error("withdrawal amount not sufficient to cover fee");
}

/// Coins attached to the message would be stranded in the account, so they are
/// refused outright.
#[tokio::test]
async fn force_withdrawal_rejects_attached_funds() {
    let (mut suite, mut accounts, _, _, _) = setup_test_naive(TestOption::default());

    let target = accounts.user2.address();

    suite
        .execute(
            &mut accounts.owner,
            target,
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: ethereum_usdc(),
                recipient: recipient(),
            },
            coins! { usdc::DENOM.clone() => 100 },
        )
        .await
        .should_fail_with_error("don't send funds when forcing a withdrawal");
}

/// The interaction that has to hold: the account-to-gateway leg is exactly the
/// one the transfer block leaves open.
#[tokio::test]
async fn force_withdrawal_works_while_transfers_are_disabled() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    disable_transfers(&mut suite, &mut accounts.owner, contracts.bank).await;

    let target = accounts.user2.address();
    let balance = suite
        .query_balance(&accounts.user2, usdc::DENOM.clone())
        .unwrap();

    let events = suite
        .execute(
            &mut accounts.owner,
            target,
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: ethereum_usdc(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            withdrawal_id(&events, contracts.gateway),
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed();

    suite
        .query_balance(&accounts.user2, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    assert!(balance.is_non_zero());
}

/// The owner holds a balance of its own, and sweeps it the same way.
#[tokio::test]
async fn force_withdrawal_works_on_the_owner_account() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let target = accounts.owner.address();

    let events = suite
        .execute(
            &mut accounts.owner,
            target,
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: ethereum_usdc(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    suite
        .query_balance(&accounts.owner, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);

    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryWithdrawalRequestRequest {
                id: withdrawal_id(&events, contracts.gateway),
            },
        )
        .should_succeed_and(|request| request.as_ref().unwrap().user == target);
}

/// Many of the accounts left holding dust never reached the minimum deposit,
/// so they are still `Inactive` and can't send a transaction of their own.
/// Sweeping them is exactly the case this method exists for.
#[tokio::test]
async fn force_withdrawal_works_on_an_inactive_account() {
    let (suite, mut accounts, codes, contracts, valset) = setup_test_naive_with_custom_genesis(
        TestOption::default(),
        GenesisOption {
            account: AccountOption {
                // Anything below this leaves a newly onboarded account
                // `Inactive`, which is the state we want to sweep from.
                minimum_deposit: coins! { usdc::DENOM.clone() => 10_000_000 },
                ..Preset::preset_test()
            },
            ..Preset::preset_test()
        },
    );

    let mut suite = HyperlaneTestSuite::new(suite, valset, &contracts);

    // Onboard a user without a deposit, then send it less than the minimum, so
    // it holds funds but never activates.
    let user = dango_testing::TestAccount::new_random().predict_address(
        contracts.account_factory,
        0,
        dango_primitives::HashExt::sha2_256(&codes.account.to_bytes()),
        true,
    );

    user.register_user(&mut suite, contracts.account_factory, Coins::new())
        .await;

    let mut relayer = accounts.user1.clone();

    suite
        .receive_warp_transfer(
            &mut relayer,
            mock_ethereum::DOMAIN,
            mock_ethereum::USDC_WARP,
            &user,
            5_000_000,
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(dango_types::auth::AccountStatus::Inactive);

    suite
        .execute(
            &mut accounts.owner,
            user.address(),
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: ethereum_usdc(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .query_balance(&user, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::ZERO);
}

/// A forced withdrawal is an ordinary withdrawal in the gateway's eyes. It
/// gets no allowance the user wouldn't have had.
#[tokio::test]
async fn force_withdrawal_respects_rate_limits() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    // A zero rate limit is a hard freeze.
    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetRateLimits(btree_map! {
                usdc::DENOM.clone() => dango_primitives::Bounded::new_unchecked(
                    dango_math::Udec128::ZERO,
                ),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            accounts.user2.address(),
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: ethereum_usdc(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error("insufficient outbound quota");
}

/// A personal quota is consumed before the global one. A forced withdrawal
/// goes through the same accounting, so a quota granted to the user is spent
/// on their behalf rather than ignored.
#[tokio::test]
async fn force_withdrawal_consumes_a_personal_quota() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let target = accounts.user2.address();
    let balance = suite
        .query_balance(&accounts.user2, usdc::DENOM.clone())
        .unwrap();

    // Enough to cover the whole withdrawal, and no more, so a correct
    // accounting empties it exactly.
    let quota = balance - Uint128::new(ETHEREUM_USDC_WITHDRAWAL_FEE);

    suite
        .execute(
            &mut accounts.owner,
            contracts.gateway,
            &gateway::ExecuteMsg::SetPersonalQuota {
                user: target,
                denom: usdc::DENOM.clone(),
                quota: Op::Insert(gateway::SetPersonalQuotaRequest {
                    amount: quota,
                    available_for: None,
                }),
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    let events = suite
        .execute(
            &mut accounts.owner,
            target,
            &account::ExecuteMsg::ForceWithdrawal {
                denom: usdc::DENOM.clone(),
                remote: ethereum_usdc(),
                recipient: recipient(),
            },
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    // The quota is untouched until the request is approved.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: target,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed_and(|entry| entry.as_ref().unwrap().amount == quota);

    suite
        .respond_to_withdrawal(
            &mut accounts.owner,
            contracts.gateway,
            withdrawal_id(&events, contracts.gateway),
            WithdrawalResponse::Approve,
        )
        .await
        .should_succeed();

    // Fully consumed, so the entry is deleted rather than left at zero.
    suite
        .query_wasm_smart(
            contracts.gateway,
            gateway::QueryPersonalQuotaRequest {
                user: target,
                denom: usdc::DENOM.clone(),
            },
        )
        .should_succeed_and_equal(None);
}

// --------------------------------- helpers -----------------------------------

async fn disable_transfers<DB, VM, PP, ID>(
    suite: &mut dango_testing::TestSuite<DB, VM, PP, ID>,
    owner: &mut dango_testing::TestAccount,
    bank: Addr,
) where
    DB: dango_app::Db,
    VM: dango_app::Vm + Clone + Send + Sync + 'static,
    PP: dango_app::ProposalPreparer,
    ID: dango_app::Indexer,
    dango_app::AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    suite
        .execute(
            owner,
            bank,
            &bank::ExecuteMsg::SetTransfersEnabled(false),
            Coins::new(),
        )
        .await
        .should_succeed();
}
