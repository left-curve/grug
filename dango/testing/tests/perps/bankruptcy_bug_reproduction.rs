//! # Bankruptcy price bug — reproduction of the 2026-06-03 testnet incident
//!
//! ## The bug
//!
//! `compute_bankruptcy_price` divides the account's **whole** equity by the
//! **scheduled close amount**. The close schedule sizes the close to cure the
//! maintenance-margin deficit, so for a solvent account (`0 < equity < MM`)
//! the close is only a fraction of the position. Dividing whole-account
//! equity by that fraction amplifies the offset, throwing the bankruptcy
//! price far from the oracle — and the ADL leg settles at that price,
//! confiscating the account's entire equity through a single partial fill
//! and handing it to the ADL counter-party.
//!
//! ## The testnet incident (block 32500262)
//!
//! During a sharp market-wide drop, several solvent accounts with large SOL
//! positions fell slightly below maintenance margin. Their deficit-curing
//! closes were small fractions of their positions, the book was thin, and
//! the remainders were ADL'd at bankruptcy prices ranging from $59.86 down
//! to **−$0.596944** against a $76.32 oracle. The same defect fired on
//! mainnet on 2026-06-05 (ETH at $1,558.63 against a $1,687.41 oracle),
//! merely on smaller positions.
//!
//! ## This test
//!
//! The minimal reproduction of that signature: a solvent long, marginally
//! below maintenance margin, an empty book, and a partial scheduled close
//! that goes entirely to ADL.
//!
//! - Alice opens LONG 10 ETH @ $2,000 with exactly $2,000 margin (10x, the
//!   initial-margin boundary). Bob holds the opposite SHORT 10 ETH.
//! - Oracle drops to $1,875:
//!   - equity = 2,000 + 10 × (1,875 − 2,000) = $750 (solvent)
//!   - MM = 10 × 1,875 × 5% = $937.50 → liquidatable
//!   - deficit = 937.50 − 750 = $187.50
//!   - close = 187.50 / (1,875 × 5%) = exactly 2 ETH (partial)
//! - The book is empty, so the full 2 ETH goes to ADL against Bob.
//!
//! Trading and liquidation fees are zeroed so the figures isolate the
//! bankruptcy-price arithmetic.
//!
//! ## The buggy behavior (pre-fix)
//!
//! ```plain
//! bp = 1,875 − 750/2 = $1,500   (−20% from the oracle)
//! ```
//!
//! Alice's 2-ETH fill at $1,500 realized −$1,000, wiping her equity to
//! exactly zero even though she was solvent and 8 ETH of her position
//! remained. Bob pocketed the difference: 2 × (2,000 − 1,500) = +$1,000.
//!
//! ## Correct behavior, asserted below
//!
//! The bankruptcy price divides by the **full position size** — by
//! definition, it is the price at which closing the whole position zeroes
//! the account's equity:
//!
//! ```plain
//! bp = 1,875 − 750/10 = $1,800   (within the 5% mmr band)
//! ```
//!
//! Alice concedes equity/size = $75 per ADL'd unit (2 × 75 = $150), keeping
//! equity of $600 = 750 × (1 − 2/10).

use {
    crate::{default_pair_param, default_param, register_oracle_prices},
    dango_math::Uint128,
    dango_order_book::{Dimensionless, OrderKind, Quantity, TimeInForce, UsdPrice, UsdValue},
    dango_primitives::{
        Addressable, CheckedContractEvent, Coins, JsonDeExt, QuerierExt, ResultExt, SearchEvent,
        btree_map,
    },
    dango_testing::{TestOption, pair_id, setup_test_naive},
    dango_types::{
        constants::usdc,
        perps::{self, Deleveraged, Liquidated, Param, RateSchedule, UserState},
    },
};

#[tokio::test]
async fn solvent_partial_close_adl_price() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let pair = pair_id();

    // Zero trading and liquidation fees; default pair params (5% mmr, 10% imr).
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: Param {
                    taker_fee_rates: RateSchedule::default(),
                    liquidation_fee_rate: Dimensionless::ZERO,
                    ..default_param()
                },
                pair_params: btree_map! { pair.clone() => default_pair_param() },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // ------------------------------------------------------------------------
    // Bob (user2) deposits $10,000 and asks 10 ETH @ $2,000. Alice (user1)
    // deposits $2,000 and market-buys 10 ETH. With zero fees:
    //   Alice: long 10 @ $2,000, margin $2,000 (exactly the 10% IMR).
    //   Bob:   short 10 @ $2,000, margin $10,000.
    // ------------------------------------------------------------------------

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
                size: Quantity::new_int(-10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
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
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(2_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10),
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

    let alice: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    assert_eq!(alice.margin, UsdValue::new_int(2_000));
    assert_eq!(alice.positions[&pair].size, Quantity::new_int(10));

    // ------------------------------------------------------------------------
    // Oracle drops to $1,875. Alice is solvent (equity $750 > 0) but below
    // maintenance margin ($937.50). The scheduled close is exactly 2 ETH.
    // ------------------------------------------------------------------------

    register_oracle_prices(&mut suite, &mut accounts, 1_875).await;

    // ------------------------------------------------------------------------
    // Liquidate. The book is empty (Bob's opening ask was fully consumed),
    // so the entire 2-ETH close is ADL'd against Bob — at the bankruptcy
    // price.
    // ------------------------------------------------------------------------

    let events = suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Liquidate {
                user: accounts.user1.address(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    // ------------------------------------------------------------------------
    // The Liquidated event. The 2-ETH partial close is priced by dividing
    // Alice's whole $750 equity by her FULL 10-ETH position:
    //
    //   bp = 1,875 − 750/10 = $1,800
    //
    // (Pre-fix, the divisor was the 2-ETH close amount: bp = $1,500.)
    // ------------------------------------------------------------------------

    let liquidated_events = events
        .clone()
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "liquidated")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<Liquidated>().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(liquidated_events.len(), 1, "exactly one Liquidated event");
    let liquidated = &liquidated_events[0];
    assert_eq!(liquidated.user, accounts.user1.address());
    assert_eq!(liquidated.adl_size, Quantity::new_int(-2));
    assert_eq!(
        liquidated.adl_price,
        Some(UsdPrice::new_int(1_800)),
        "bankruptcy price must divide equity by the full position size"
    );
    assert_eq!(liquidated.adl_realized_pnl, UsdValue::new_int(-400));

    // ------------------------------------------------------------------------
    // The Deleveraged event (Bob). He buys back 2 ETH of his $2,000-entry
    // short at $1,800, realizing 2 × (2,000 − 1,800) = +$400: his
    // mark-to-market profit plus Alice's $75-per-unit equity concession.
    // ------------------------------------------------------------------------

    let deleveraged_events = events
        .clone()
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "deleveraged")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<Deleveraged>().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        deleveraged_events.len(),
        1,
        "single ADL counter-party (Bob)"
    );
    let deleveraged = &deleveraged_events[0];
    assert_eq!(deleveraged.user, accounts.user2.address());
    assert_eq!(deleveraged.closing_size, Quantity::new_int(2));
    assert_eq!(deleveraged.fill_price, UsdPrice::new_int(1_800));
    assert_eq!(deleveraged.realized_pnl, UsdValue::new_int(400));

    // ------------------------------------------------------------------------
    // No bad debt: a solvent account's equity stays positive through a
    // partial ADL at the bankruptcy price.
    // ------------------------------------------------------------------------

    let bad_debt_events = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "bad_debt_covered")
        .take()
        .all();
    assert!(bad_debt_events.is_empty(), "no bad debt in this scenario");

    let global_state: perps::State = suite
        .query_wasm_smart(contracts.perps, perps::QueryStateRequest {})
        .should_succeed();
    assert_eq!(global_state.insurance_fund, UsdValue::ZERO);

    // ------------------------------------------------------------------------
    // Alice's state: margin = 2,000 + 2 × (1,800 − 2,000) = $1,600; her
    // remaining 8 ETH carries −$1,000 unrealized at the oracle, so her
    // equity is $600 = 750 × (1 − 2/10) — she concedes only the closed
    // fraction's share of her equity.
    //
    // (Pre-fix: margin $1,000, equity exactly ZERO — fully confiscated
    // despite being solvent.)
    // ------------------------------------------------------------------------

    let alice: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    assert_eq!(alice.margin, UsdValue::new_int(1_600));
    assert_eq!(alice.positions[&pair].size, Quantity::new_int(8));
    assert_eq!(alice.positions[&pair].entry_price, UsdPrice::new_int(2_000));

    let alice_unrealized = alice.positions[&pair]
        .size
        .checked_mul(
            UsdPrice::new_int(1_875)
                .checked_sub(UsdPrice::new_int(2_000))
                .unwrap(),
        )
        .unwrap();
    let alice_equity = alice.margin.checked_add(alice_unrealized).unwrap();
    assert_eq!(
        alice_equity,
        UsdValue::new_int(600),
        "a solvent account keeps the unclosed fraction of its equity"
    );

    // ------------------------------------------------------------------------
    // Bob's state: margin = 10,000 + 400 = $10,400, short 8 left.
    //
    // (Pre-fix: margin $11,000 — Alice's entire equity.)
    // ------------------------------------------------------------------------

    let bob: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed()
        .unwrap();
    assert_eq!(bob.margin, UsdValue::new_int(10_400));
    assert_eq!(bob.positions[&pair].size, Quantity::new_int(-8));
}
