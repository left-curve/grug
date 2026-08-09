//! # Liquidation & ADL — spec example transcriptions
//!
//! End-to-end tests asserting the exact figures of the worked examples in
//! `book/perps/4-liquidation-and-adl.md` ("Examples" section). Examples 1–8
//! follow the spec verbatim (Alice long, Bob short); examples 9–16 mirror
//! them with the sides flipped (Alice short, Bob long).
//!
//! Cast, as in the spec: **Alice** (`user1`) is liquidated; **Bob** (`user2`)
//! holds the exact opposite position(s) and is the ADL counter-party;
//! **Carol** (`user3`) supplies order-book liquidity where the example calls
//! for it.
//!
//! Parameters, as in the spec: 5% maintenance margin ratio, 10% initial
//! margin ratio, 0.1% liquidation fee, zero trading fees (the spec's
//! examples start from already-open positions with given margins, so opening
//! fees would shift every figure), zero liquidation buffer.
//!
//! | #      | Alice                  | Margin        | Oracle moves to           | Book (Carol)  | Outcome                                                                                 |
//! | ------ | ---------------------- | ------------- | ------------------------- | ------------- | --------------------------------------------------------------------------------------- |
//! | 1 (9)  | long (short) 10 ETH    | 2,180 (2,220) | 1,800 (2,200)             | 8 @ oracle    | book fills 8; no ADL; no bad debt                                                       |
//! | 2 (10) | long (short) 10 ETH    | 2,180 (2,220) | 1,800 (2,200)             | 5 @ oracle    | book 5 + ADL 3 @ bp 1,782 (2,222)                                                       |
//! | 3 (11) | long (short) 10 ETH    | 2,180 (2,220) | 1,800 (2,200)             | —             | ADL 8 @ bp 1,782 (2,222)                                                                |
//! | 4 (12) | long (short) 10 ETH    | 2,800         | 1,700 (2,300)             | 10 @ oracle   | book fills 10 below bp; bad debt 200                                                    |
//! | 5 (13) | long (short) 10 ETH    | 2,800         | 1,700 (2,300)             | 4 @ oracle    | book 4 + ADL 6 @ bp 1,720 (2,280); bad debt 80                                          |
//! | 6 (14) | long (short) 10 ETH    | 2,800         | 1,700 (2,300)             | —             | ADL 10 @ bp; margin zeroed exactly; no bad debt                                         |
//! | 7 (15) | 10 ETH + 1 BTC         | 7,065 (7,435) | 1,900 / 47k (2,100 / 53k) | —             | ADL 0.1 BTC @ bp 43,935 (56,435); ETH untouched                                         |
//! | 8 (16) | 10 ETH + 1 BTC         | 7,065 (7,435) | 1,800 / 44k (2,200 / 56k) | —             | BTC fully ADL'd @ bp 44,935 (55,435), equity → 0, then ETH ADL'd at oracle; no bad debt |

use {
    crate::{default_pair_param, default_param},
    dango_math::Uint128,
    dango_order_book::{Dimensionless, OrderKind, Quantity, TimeInForce, UsdPrice, UsdValue},
    dango_primitives::{
        Addr, Addressable, CheckedContractEvent, Coins, Denom, JsonDeExt, QuerierExt, ResultExt,
        SearchEvent, TxEvents, btree_map,
    },
    dango_testing::{
        OracleTestEntry, TestAccount, TestAccounts, TestOption, TestSuiteNaive, pair_id,
        setup_test_naive,
    },
    dango_types::{
        constants::usdc,
        perps::{self, BadDebtCovered, Deleveraged, Liquidated, Param, RateSchedule, UserState},
    },
};

const ENTRY_ETH: i128 = 2_000;
const ENTRY_BTC: i128 = 50_000;
const BOB_MARGIN_SINGLE: i128 = 10_000;
const BOB_MARGIN_DOUBLE: i128 = 12_000;
const CAROL_MARGIN: i128 = 5_000;

fn btc_pair_id() -> Denom {
    "perp/btcusd".parse().unwrap()
}

/// The spec's parameters: zero trading fees, 0.1% liquidation fee.
fn spec_param() -> Param {
    Param {
        taker_fee_rates: RateSchedule::default(),
        liquidation_fee_rate: Dimensionless::new_permille(1), // 0.1%
        ..default_param()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Long,
    Short,
}

impl Side {
    /// Sign of Alice's position; Bob's is the opposite.
    fn sign(self) -> i128 {
        match self {
            Side::Long => 1,
            Side::Short => -1,
        }
    }
}

/// The ADL leg expected from a liquidation, if any.
struct Adl {
    /// Absolute ADL'd amount, in raw (1e-6) position units.
    size_raw: i128,
    /// The bankruptcy price (whole dollars).
    price: i128,
}

// ---------------------------- shared helpers ---------------------------------

async fn deposit(
    suite: &mut TestSuiteNaive,
    account: &mut TestAccount,
    perps: Addr,
    dollars: i128,
) {
    suite
        .execute(
            account,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(
                usdc::DENOM.clone(),
                Uint128::new(dollars as u128 * 1_000_000),
            )
            .unwrap(),
        )
        .await
        .should_succeed();
}

async fn submit_limit(
    suite: &mut TestSuiteNaive,
    account: &mut TestAccount,
    perps: Addr,
    pair: &Denom,
    size: i128,
    price: i128,
) {
    suite
        .execute(
            account,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(size),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(price),
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

async fn submit_market(
    suite: &mut TestSuiteNaive,
    account: &mut TestAccount,
    perps: Addr,
    pair: &Denom,
    size: i128,
) {
    suite
        .execute(
            account,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(size),
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

/// Seed oracle prices for USDC, ETH, and (optionally) BTC.
async fn seed_prices(
    suite: &mut TestSuiteNaive,
    accounts: &mut TestAccounts,
    eth_price: i128,
    btc_price: Option<i128>,
) {
    let mut entries = btree_map! {
        usdc::DENOM.clone() => OracleTestEntry {
            pyth_id: 1,
            humanized_price: UsdPrice::new_int(1),
        },
        pair_id() => OracleTestEntry {
            pyth_id: 2,
            humanized_price: UsdPrice::new_int(eth_price),
        },
    };

    if let Some(btc_price) = btc_price {
        entries.insert(
            btc_pair_id(),
            OracleTestEntry {
                pyth_id: 3,
                humanized_price: UsdPrice::new_int(btc_price),
            },
        );
    }

    suite.seed_oracle_prices(&mut accounts.owner, entries).await;
}

/// Configure the spec params for the ETH pair and, when `with_btc`, also the
/// BTC pair. Both pairs must be configured in a single call: the contract
/// replaces its pair list wholesale with the message's keys. The BTC pair is
/// new (not in the test genesis), so its oracle price must be seeded before
/// this call — the contract initializes the pair's state from the oracle.
async fn configure(
    suite: &mut TestSuiteNaive,
    accounts: &mut TestAccounts,
    perps: Addr,
    with_btc: bool,
) {
    let mut pair_params = btree_map! { pair_id() => default_pair_param() };

    if with_btc {
        pair_params.insert(btc_pair_id(), default_pair_param());
    }

    suite
        .execute(
            &mut accounts.owner,
            perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: spec_param(),
                pair_params,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();
}

async fn liquidate(
    suite: &mut TestSuiteNaive,
    accounts: &mut TestAccounts,
    perps: Addr,
) -> TxEvents {
    suite
        .execute(
            &mut accounts.owner,
            perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Liquidate {
                user: accounts.user1.address(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events
}

fn search_liquidated(events: &TxEvents) -> Vec<Liquidated> {
    events
        .clone()
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "liquidated")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<Liquidated>().unwrap())
        .collect()
}

fn search_deleveraged(events: &TxEvents) -> Vec<Deleveraged> {
    events
        .clone()
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "deleveraged")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<Deleveraged>().unwrap())
        .collect()
}

fn search_bad_debt(events: &TxEvents) -> Vec<BadDebtCovered> {
    events
        .clone()
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "bad_debt_covered")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<BadDebtCovered>().unwrap())
        .collect()
}

async fn query_user(suite: &mut TestSuiteNaive, perps: Addr, user: Addr) -> Option<UserState> {
    suite
        .query_wasm_smart(perps, perps::QueryUserStateRequest { user })
        .should_succeed()
}

async fn assert_insurance_fund(suite: &mut TestSuiteNaive, perps: Addr, expected_raw: i128) {
    let state: perps::State = suite
        .query_wasm_smart(perps, perps::QueryStateRequest {})
        .should_succeed();

    assert_eq!(
        state.insurance_fund,
        UsdValue::new_raw(expected_raw),
        "insurance fund mismatch"
    );
}

// --------------------- single-position runner (1-6, 9-14) --------------------

struct SingleCase {
    side: Side,
    /// Alice's deposit. Bob's is `BOB_MARGIN_SINGLE`.
    alice_margin: i128,
    /// ETH oracle price after the move (entry is `ENTRY_ETH`).
    oracle: i128,
    /// Carol's resting order size at the post-move oracle price, on the side
    /// the liquidation close will hit. Zero means an empty book.
    carol_size: i128,
    /// The expected ADL leg; `None` when the book absorbs the whole close.
    adl: Option<Adl>,
    /// Alice's post-liquidation margin, raw (1e-6 dollars).
    alice_margin_raw: i128,
    /// Alice's remaining position size (signed, whole units); 0 = wiped.
    alice_pos: i128,
    /// Bob's post-liquidation margin, raw.
    bob_margin_raw: i128,
    /// Bob's remaining position size (signed, whole units); 0 = fully ADL'd.
    bob_pos: i128,
    /// Insurance fund after liquidation, raw (fees in, bad debt out).
    insurance_fund_raw: i128,
    /// Expected bad debt in whole dollars, if any.
    bad_debt: Option<i128>,
}

async fn run_single(case: SingleCase) {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let pair = pair_id();
    let sign = case.side.sign();

    seed_prices(&mut suite, &mut accounts, ENTRY_ETH, None).await;
    configure(&mut suite, &mut accounts, contracts.perps, false).await;

    // Open the positions: Bob makes at the entry price, Alice takes.
    deposit(
        &mut suite,
        &mut accounts.user2,
        contracts.perps,
        BOB_MARGIN_SINGLE,
    )
    .await;
    submit_limit(
        &mut suite,
        &mut accounts.user2,
        contracts.perps,
        &pair,
        -10 * sign,
        ENTRY_ETH,
    )
    .await;

    deposit(
        &mut suite,
        &mut accounts.user1,
        contracts.perps,
        case.alice_margin,
    )
    .await;
    submit_market(
        &mut suite,
        &mut accounts.user1,
        contracts.perps,
        &pair,
        10 * sign,
    )
    .await;

    // Sanity: zero-fee opens leave margins untouched.
    let alice = query_user(&mut suite, contracts.perps, accounts.user1.address())
        .await
        .unwrap();
    assert_eq!(alice.margin, UsdValue::new_int(case.alice_margin));
    assert_eq!(alice.positions[&pair].size, Quantity::new_int(10 * sign));

    // Move the oracle; Alice becomes liquidatable.
    seed_prices(&mut suite, &mut accounts, case.oracle, None).await;

    // Carol's book liquidity, resting at the post-move oracle price on the
    // side the close will hit (bids for a long's close, asks for a short's).
    if case.carol_size > 0 {
        deposit(
            &mut suite,
            &mut accounts.user3,
            contracts.perps,
            CAROL_MARGIN,
        )
        .await;
        submit_limit(
            &mut suite,
            &mut accounts.user3,
            contracts.perps,
            &pair,
            case.carol_size * sign,
            case.oracle,
        )
        .await;
    }

    let events = liquidate(&mut suite, &mut accounts, contracts.perps).await;

    // ---- Liquidated event (exactly one; single pair) ----
    let liquidated = search_liquidated(&events);
    assert_eq!(liquidated.len(), 1, "exactly one Liquidated event");
    assert_eq!(liquidated[0].user, accounts.user1.address());

    // The event reports Alice's position size after this pair's liquidation —
    // the same value the state query asserts below, whether the close was
    // partial (book and/or ADL) or full.
    assert_eq!(
        liquidated[0].remaining_position_size,
        Some(Quantity::new_int(case.alice_pos))
    );

    match &case.adl {
        Some(adl) => {
            // Alice's ADL closes against her position: negative for a long.
            assert_eq!(
                liquidated[0].adl_size,
                Quantity::new_raw(-adl.size_raw * sign)
            );
            assert_eq!(liquidated[0].adl_price, Some(UsdPrice::new_int(adl.price)));

            // Alice's realized PnL on the ADL leg, per unit: fill price vs
            // entry for a long, entry vs fill price for a short.
            let alice_adl_pnl = adl.size_raw * (adl.price - ENTRY_ETH) * sign;
            assert_eq!(
                liquidated[0].adl_realized_pnl,
                UsdValue::new_raw(alice_adl_pnl)
            );

            // ---- Deleveraged event (Bob, the only counter-party) ----
            let deleveraged = search_deleveraged(&events);
            assert_eq!(deleveraged.len(), 1, "single ADL counter-party (Bob)");
            assert_eq!(deleveraged[0].user, accounts.user2.address());
            assert_eq!(
                deleveraged[0].closing_size,
                Quantity::new_raw(adl.size_raw * sign)
            );
            assert_eq!(deleveraged[0].fill_price, UsdPrice::new_int(adl.price));
            // Zero-sum: Bob's gain is Alice's loss on the ADL leg.
            assert_eq!(
                deleveraged[0].realized_pnl,
                UsdValue::new_raw(-alice_adl_pnl)
            );

            // The event reports Bob's position size after the ADL fill —
            // non-zero when his position was only partially consumed, and
            // the same value the state query asserts below.
            assert_eq!(
                deleveraged[0].remaining_position_size,
                Some(Quantity::new_int(case.bob_pos))
            );
        },
        None => {
            assert_eq!(liquidated[0].adl_size, Quantity::ZERO);
            assert_eq!(liquidated[0].adl_price, None);
            assert!(
                search_deleveraged(&events).is_empty(),
                "no ADL ⇒ no Deleveraged event"
            );
        },
    }

    // ---- Bad debt ----
    let bad_debt = search_bad_debt(&events);
    match case.bad_debt {
        Some(amount) => {
            assert_eq!(bad_debt.len(), 1, "expected a BadDebtCovered event");
            assert_eq!(bad_debt[0].liquidated_user, accounts.user1.address());
            assert_eq!(bad_debt[0].amount, UsdValue::new_int(amount));
            assert_eq!(
                bad_debt[0].insurance_fund_remaining,
                UsdValue::new_raw(case.insurance_fund_raw)
            );
        },
        None => assert!(bad_debt.is_empty(), "no bad debt expected"),
    }

    // ---- Alice ----
    let alice = query_user(&mut suite, contracts.perps, accounts.user1.address()).await;
    if case.alice_pos == 0 {
        // Fully closed; the state entry may survive with zero margin or be
        // pruned entirely.
        if let Some(alice) = alice {
            assert!(alice.positions.is_empty(), "Alice should hold no position");
            assert_eq!(alice.margin, UsdValue::new_raw(case.alice_margin_raw));
        }
        assert_eq!(
            case.alice_margin_raw, 0,
            "wiped Alice must have zero margin"
        );
    } else {
        let alice = alice.expect("Alice retains a position");
        assert_eq!(alice.margin, UsdValue::new_raw(case.alice_margin_raw));
        assert_eq!(
            alice.positions[&pair].size,
            Quantity::new_int(case.alice_pos)
        );
        assert_eq!(
            alice.positions[&pair].entry_price,
            UsdPrice::new_int(ENTRY_ETH)
        );
    }

    // ---- Bob ----
    let bob = query_user(&mut suite, contracts.perps, accounts.user2.address())
        .await
        .unwrap();
    assert_eq!(bob.margin, UsdValue::new_raw(case.bob_margin_raw));
    if case.bob_pos == 0 {
        assert!(bob.positions.is_empty(), "Bob fully ADL'd");
    } else {
        assert_eq!(bob.positions[&pair].size, Quantity::new_int(case.bob_pos));
        assert_eq!(
            bob.positions[&pair].entry_price,
            UsdPrice::new_int(ENTRY_ETH)
        );
    }

    // ---- Carol ----
    if case.carol_size > 0 {
        let carol = query_user(&mut suite, contracts.perps, accounts.user3.address())
            .await
            .unwrap();
        assert_eq!(
            carol.positions[&pair].size,
            Quantity::new_int(case.carol_size * sign),
            "Carol's resting order absorbs the book leg in full"
        );
        assert_eq!(
            carol.positions[&pair].entry_price,
            UsdPrice::new_int(case.oracle)
        );
    }

    // ---- Insurance fund ----
    assert_insurance_fund(&mut suite, contracts.perps, case.insurance_fund_raw).await;
}

// ---------------------- two-position runner (7-8, 15-16) ---------------------

struct DoubleCase {
    side: Side,
    /// Alice's deposit. Bob's is `BOB_MARGIN_DOUBLE`.
    alice_margin: i128,
    /// Post-move oracle prices (entries are `ENTRY_ETH` / `ENTRY_BTC`).
    oracle_eth: i128,
    oracle_btc: i128,
    /// The BTC ADL leg (always present: BTC has the larger MM contribution,
    /// so it is scheduled first; the book is empty in these examples).
    btc_adl: Adl,
    /// The ETH ADL leg; `None` when the BTC close cures the whole deficit
    /// and ETH is never scheduled.
    eth_adl: Option<Adl>,
    /// Alice's post-liquidation margin, raw.
    alice_margin_raw: i128,
    /// Bob's post-liquidation margin, raw.
    bob_margin_raw: i128,
    /// Insurance fund after liquidation, raw.
    insurance_fund_raw: i128,
}

async fn run_double(case: DoubleCase) {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let eth = pair_id();
    let btc = btc_pair_id();
    let sign = case.side.sign();

    // The BTC pair is not in the test genesis: seed its oracle price first,
    // then register it via Configure.
    seed_prices(&mut suite, &mut accounts, ENTRY_ETH, Some(ENTRY_BTC)).await;
    configure(&mut suite, &mut accounts, contracts.perps, true).await;

    // Open both pairs: Bob makes at entry, Alice takes.
    deposit(
        &mut suite,
        &mut accounts.user2,
        contracts.perps,
        BOB_MARGIN_DOUBLE,
    )
    .await;
    submit_limit(
        &mut suite,
        &mut accounts.user2,
        contracts.perps,
        &eth,
        -10 * sign,
        ENTRY_ETH,
    )
    .await;
    submit_limit(
        &mut suite,
        &mut accounts.user2,
        contracts.perps,
        &btc,
        -sign,
        ENTRY_BTC,
    )
    .await;

    deposit(
        &mut suite,
        &mut accounts.user1,
        contracts.perps,
        case.alice_margin,
    )
    .await;
    submit_market(
        &mut suite,
        &mut accounts.user1,
        contracts.perps,
        &eth,
        10 * sign,
    )
    .await;
    submit_market(&mut suite, &mut accounts.user1, contracts.perps, &btc, sign).await;

    let alice = query_user(&mut suite, contracts.perps, accounts.user1.address())
        .await
        .unwrap();
    assert_eq!(alice.margin, UsdValue::new_int(case.alice_margin));
    assert_eq!(alice.positions[&eth].size, Quantity::new_int(10 * sign));
    assert_eq!(alice.positions[&btc].size, Quantity::new_int(sign));

    // Move both oracles; Alice becomes liquidatable. The book is empty.
    seed_prices(
        &mut suite,
        &mut accounts,
        case.oracle_eth,
        Some(case.oracle_btc),
    )
    .await;

    let events = liquidate(&mut suite, &mut accounts, contracts.perps).await;

    // ---- Liquidated events, in schedule order: BTC (larger MM) first ----
    let liquidated = search_liquidated(&events);
    let expected_count = 1 + case.eth_adl.is_some() as usize;
    assert_eq!(
        liquidated.len(),
        expected_count,
        "one Liquidated event per scheduled pair"
    );

    assert_eq!(liquidated[0].pair_id, btc);
    assert_eq!(liquidated[0].user, accounts.user1.address());
    assert_eq!(
        liquidated[0].adl_size,
        Quantity::new_raw(-case.btc_adl.size_raw * sign)
    );
    assert_eq!(
        liquidated[0].adl_price,
        Some(UsdPrice::new_int(case.btc_adl.price))
    );
    let alice_btc_pnl = case.btc_adl.size_raw * (case.btc_adl.price - ENTRY_BTC) * sign;
    assert_eq!(
        liquidated[0].adl_realized_pnl,
        UsdValue::new_raw(alice_btc_pnl)
    );

    // Each per-pair Liquidated event reports its own pair's resulting size:
    // Alice's 1-BTC position less the BTC ADL leg.
    assert_eq!(
        liquidated[0].remaining_position_size,
        Some(Quantity::new_raw(
            (1_000_000 - case.btc_adl.size_raw) * sign
        ))
    );

    if let Some(eth_adl) = &case.eth_adl {
        assert_eq!(liquidated[1].pair_id, eth);
        assert_eq!(
            liquidated[1].adl_size,
            Quantity::new_raw(-eth_adl.size_raw * sign)
        );
        assert_eq!(
            liquidated[1].adl_price,
            Some(UsdPrice::new_int(eth_adl.price))
        );
        // Alice's 10-ETH position less the ETH ADL leg.
        assert_eq!(
            liquidated[1].remaining_position_size,
            Some(Quantity::new_raw((10_000_000 - eth_adl.size_raw) * sign))
        );
    }

    // ---- Deleveraged events (all against Bob) ----
    let deleveraged = search_deleveraged(&events);
    assert_eq!(deleveraged.len(), expected_count);

    assert_eq!(deleveraged[0].user, accounts.user2.address());
    assert_eq!(deleveraged[0].pair_id, btc);
    assert_eq!(
        deleveraged[0].closing_size,
        Quantity::new_raw(case.btc_adl.size_raw * sign)
    );
    assert_eq!(
        deleveraged[0].fill_price,
        UsdPrice::new_int(case.btc_adl.price)
    );
    assert_eq!(
        deleveraged[0].realized_pnl,
        UsdValue::new_raw(-alice_btc_pnl)
    );

    // Bob's post-ADL BTC size mirrors Alice's remainder on the short side.
    assert_eq!(
        deleveraged[0].remaining_position_size,
        Some(Quantity::new_raw(
            -(1_000_000 - case.btc_adl.size_raw) * sign
        ))
    );

    if let Some(eth_adl) = &case.eth_adl {
        let alice_eth_pnl = eth_adl.size_raw * (eth_adl.price - ENTRY_ETH) * sign;
        assert_eq!(deleveraged[1].user, accounts.user2.address());
        assert_eq!(deleveraged[1].pair_id, eth);
        assert_eq!(deleveraged[1].fill_price, UsdPrice::new_int(eth_adl.price));
        assert_eq!(
            deleveraged[1].realized_pnl,
            UsdValue::new_raw(-alice_eth_pnl)
        );
        // Bob's post-ADL ETH size mirrors Alice's remainder on the short side.
        assert_eq!(
            deleveraged[1].remaining_position_size,
            Some(Quantity::new_raw(-(10_000_000 - eth_adl.size_raw) * sign))
        );
    }

    // ---- No bad debt in any two-position example ----
    assert!(search_bad_debt(&events).is_empty(), "no bad debt expected");

    // ---- Alice ----
    let alice = query_user(&mut suite, contracts.perps, accounts.user1.address()).await;
    if case.eth_adl.is_some() {
        // Both positions fully closed.
        if let Some(alice) = alice {
            assert!(alice.positions.is_empty());
            assert_eq!(alice.margin, UsdValue::new_raw(case.alice_margin_raw));
        }
        assert_eq!(
            case.alice_margin_raw, 0,
            "wiped Alice must have zero margin"
        );
    } else {
        // ETH untouched; BTC partially closed.
        let alice = alice.expect("Alice retains positions");
        assert_eq!(alice.margin, UsdValue::new_raw(case.alice_margin_raw));
        assert_eq!(alice.positions[&eth].size, Quantity::new_int(10 * sign));
        assert_eq!(
            alice.positions[&btc].size,
            Quantity::new_raw((1_000_000 - case.btc_adl.size_raw) * sign)
        );
    }

    // ---- Bob (mirror) ----
    let bob = query_user(&mut suite, contracts.perps, accounts.user2.address())
        .await
        .unwrap();
    assert_eq!(bob.margin, UsdValue::new_raw(case.bob_margin_raw));
    if case.eth_adl.is_some() {
        assert!(bob.positions.is_empty(), "Bob fully ADL'd on both pairs");
    } else {
        assert_eq!(bob.positions[&eth].size, Quantity::new_int(-10 * sign));
        assert_eq!(
            bob.positions[&btc].size,
            Quantity::new_raw(-(1_000_000 - case.btc_adl.size_raw) * sign)
        );
    }

    // ---- Insurance fund ----
    assert_insurance_fund(&mut suite, contracts.perps, case.insurance_fund_raw).await;
}

// ------------------------- examples 1-6: Alice long --------------------------

/// Example 1 — solvent; the book absorbs the whole close.
///
/// Equity 180, MM 900, deficit 720 → close 8 of 10 ETH; bp 1,782. Carol's
/// 8-ETH bid at the 1,800 oracle fills everything: no ADL. Margin
/// 2,180 − 1,600 − 14.40 (fee) = 565.60; Alice keeps 2 ETH.
#[tokio::test]
async fn example_1_solvent_full_book() {
    run_single(SingleCase {
        side: Side::Long,
        alice_margin: 2_180,
        oracle: 1_800,
        carol_size: 8,
        adl: None,
        alice_margin_raw: 565_600_000,
        alice_pos: 2,
        bob_margin_raw: 10_000_000_000,
        bob_pos: -10,
        insurance_fund_raw: 14_400_000,
        bad_debt: None,
    })
    .await;
}

/// Example 2 — solvent; book absorbs 5, the remaining 3 ADL'd at bp 1,782.
///
/// Margin 2,180 − 1,000 − 654 − 14.40 = 511.60. Bob gains 654 (600
/// mark-to-market + Alice's 54 concession), keeps short 7.
#[tokio::test]
async fn example_2_solvent_partial_book() {
    run_single(SingleCase {
        side: Side::Long,
        alice_margin: 2_180,
        oracle: 1_800,
        carol_size: 5,
        adl: Some(Adl {
            size_raw: 3_000_000,
            price: 1_782,
        }),
        alice_margin_raw: 511_600_000,
        alice_pos: 2,
        bob_margin_raw: 10_654_000_000,
        bob_pos: -7,
        insurance_fund_raw: 14_400_000,
        bad_debt: None,
    })
    .await;
}

/// Example 3 — solvent; empty book, the whole 8-ETH close ADL'd at bp 1,782.
///
/// Margin 2,180 − 1,744 − 14.40 = 421.60. Bob gains 1,744, keeps short 2.
#[tokio::test]
async fn example_3_solvent_empty_book() {
    run_single(SingleCase {
        side: Side::Long,
        alice_margin: 2_180,
        oracle: 1_800,
        carol_size: 0,
        adl: Some(Adl {
            size_raw: 8_000_000,
            price: 1_782,
        }),
        alice_margin_raw: 421_600_000,
        alice_pos: 2,
        bob_margin_raw: 11_744_000_000,
        bob_pos: -2,
        insurance_fund_raw: 14_400_000,
        bad_debt: None,
    })
    .await;
}

/// Example 4 — insolvent (equity −200); full close fills on the book at the
/// 1,700 oracle, below the 1,720 bp. Fee 0; bad debt 200 to the insurance
/// fund; Alice wiped; Bob untouched.
#[tokio::test]
async fn example_4_insolvent_full_book() {
    run_single(SingleCase {
        side: Side::Long,
        alice_margin: 2_800,
        oracle: 1_700,
        carol_size: 10,
        adl: None,
        alice_margin_raw: 0,
        alice_pos: 0,
        bob_margin_raw: 10_000_000_000,
        bob_pos: -10,
        insurance_fund_raw: -200_000_000,
        bad_debt: Some(200),
    })
    .await;
}

/// Example 5 — insolvent; book absorbs 4 at 1,700, the remaining 6 ADL'd at
/// bp 1,720. Bad debt 80 = the book-filled 4 × (bp − fill). Bob absorbs 120
/// by buying 6 at $20 above oracle, keeps short 4.
#[tokio::test]
async fn example_5_insolvent_partial_book() {
    run_single(SingleCase {
        side: Side::Long,
        alice_margin: 2_800,
        oracle: 1_700,
        carol_size: 4,
        adl: Some(Adl {
            size_raw: 6_000_000,
            price: 1_720,
        }),
        alice_margin_raw: 0,
        alice_pos: 0,
        bob_margin_raw: 11_680_000_000,
        bob_pos: -4,
        insurance_fund_raw: -80_000_000,
        bad_debt: Some(80),
    })
    .await;
}

/// Example 6 — insolvent; empty book, all 10 ETH ADL'd at bp 1,720. Alice's
/// margin lands at exactly zero by construction — no bad debt despite her
/// insolvency; Bob absorbs the 200 shortfall.
#[tokio::test]
async fn example_6_insolvent_empty_book() {
    run_single(SingleCase {
        side: Side::Long,
        alice_margin: 2_800,
        oracle: 1_700,
        carol_size: 0,
        adl: Some(Adl {
            size_raw: 10_000_000,
            price: 1_720,
        }),
        alice_margin_raw: 0,
        alice_pos: 0,
        bob_margin_raw: 12_800_000_000,
        bob_pos: 0,
        insurance_fund_raw: 0,
        bad_debt: None,
    })
    .await;
}

// ---------------------- examples 7-8: two positions, long --------------------

/// Example 7 — two longs (10 ETH + 1 BTC), solvent. Equity 3,065, MM 3,300,
/// deficit 235. BTC (MM contribution 2,350 > ETH's 950) is scheduled first;
/// closing 0.1 BTC cures the whole deficit, ETH is never touched. Empty book
/// → ADL 0.1 BTC at bp = 47,000 − 3,065/1 = 43,935 (whole-account equity
/// over the BTC position's full size). Margin 7,065 − 606.50 − 4.70 (fee)
/// = 6,453.80.
#[tokio::test]
async fn example_7_two_positions_solvent() {
    run_double(DoubleCase {
        side: Side::Long,
        alice_margin: 7_065,
        oracle_eth: 1_900,
        oracle_btc: 47_000,
        btc_adl: Adl {
            size_raw: 100_000,
            price: 43_935,
        },
        eth_adl: None,
        alice_margin_raw: 6_453_800_000,
        bob_margin_raw: 12_606_500_000,
        insurance_fund_raw: 4_700_000,
    })
    .await;
}

/// Example 8 — two longs, insolvent (equity −935). Both positions fully
/// closed, BTC first: 1 BTC ADL'd at bp = 44,000 + 935 = 44,935, which
/// brings the account's equity to exactly zero; the ETH bp is then
/// recomputed from zero equity and equals the 1,800 oracle. Fee 0, no bad
/// debt; Bob nets Alice's whole 7,065 margin across the two fills.
#[tokio::test]
async fn example_8_two_positions_insolvent() {
    run_double(DoubleCase {
        side: Side::Long,
        alice_margin: 7_065,
        oracle_eth: 1_800,
        oracle_btc: 44_000,
        btc_adl: Adl {
            size_raw: 1_000_000,
            price: 44_935,
        },
        eth_adl: Some(Adl {
            size_raw: 10_000_000,
            price: 1_800,
        }),
        alice_margin_raw: 0,
        bob_margin_raw: 19_065_000_000,
        insurance_fund_raw: 0,
    })
    .await;
}

// ------------------ examples 9-16: short mirrors of 1-8 ----------------------

/// Example 9 — mirror of example 1: Alice short 10 ETH, oracle rises to
/// 2,200. Equity 220, MM 1,100, deficit 880 → close 8; bp 2,222. Carol's
/// 8-ETH ask at oracle fills everything. Margin 2,220 − 1,600 − 17.60 =
/// 602.40.
#[tokio::test]
async fn example_9_solvent_full_book_short() {
    run_single(SingleCase {
        side: Side::Short,
        alice_margin: 2_220,
        oracle: 2_200,
        carol_size: 8,
        adl: None,
        alice_margin_raw: 602_400_000,
        alice_pos: -2,
        bob_margin_raw: 10_000_000_000,
        bob_pos: 10,
        insurance_fund_raw: 17_600_000,
        bad_debt: None,
    })
    .await;
}

/// Example 10 — mirror of example 2: book absorbs 5, 3 ADL'd at bp 2,222.
#[tokio::test]
async fn example_10_solvent_partial_book_short() {
    run_single(SingleCase {
        side: Side::Short,
        alice_margin: 2_220,
        oracle: 2_200,
        carol_size: 5,
        adl: Some(Adl {
            size_raw: 3_000_000,
            price: 2_222,
        }),
        alice_margin_raw: 536_400_000,
        alice_pos: -2,
        bob_margin_raw: 10_666_000_000,
        bob_pos: 7,
        insurance_fund_raw: 17_600_000,
        bad_debt: None,
    })
    .await;
}

/// Example 11 — mirror of example 3: empty book, 8 ADL'd at bp 2,222.
#[tokio::test]
async fn example_11_solvent_empty_book_short() {
    run_single(SingleCase {
        side: Side::Short,
        alice_margin: 2_220,
        oracle: 2_200,
        carol_size: 0,
        adl: Some(Adl {
            size_raw: 8_000_000,
            price: 2_222,
        }),
        alice_margin_raw: 426_400_000,
        alice_pos: -2,
        bob_margin_raw: 11_776_000_000,
        bob_pos: 2,
        insurance_fund_raw: 17_600_000,
        bad_debt: None,
    })
    .await;
}

/// Example 12 — mirror of example 4: insolvent short (equity −200), oracle
/// 2,300, bp 2,280. Carol's 10-ETH ask at oracle fills everything above the
/// bp; bad debt 200.
#[tokio::test]
async fn example_12_insolvent_full_book_short() {
    run_single(SingleCase {
        side: Side::Short,
        alice_margin: 2_800,
        oracle: 2_300,
        carol_size: 10,
        adl: None,
        alice_margin_raw: 0,
        alice_pos: 0,
        bob_margin_raw: 10_000_000_000,
        bob_pos: 10,
        insurance_fund_raw: -200_000_000,
        bad_debt: Some(200),
    })
    .await;
}

/// Example 13 — mirror of example 5: book absorbs 4 at 2,300, 6 ADL'd at bp
/// 2,280; bad debt 80.
#[tokio::test]
async fn example_13_insolvent_partial_book_short() {
    run_single(SingleCase {
        side: Side::Short,
        alice_margin: 2_800,
        oracle: 2_300,
        carol_size: 4,
        adl: Some(Adl {
            size_raw: 6_000_000,
            price: 2_280,
        }),
        alice_margin_raw: 0,
        alice_pos: 0,
        bob_margin_raw: 11_680_000_000,
        bob_pos: 4,
        insurance_fund_raw: -80_000_000,
        bad_debt: Some(80),
    })
    .await;
}

/// Example 14 — mirror of example 6: empty book, all 10 ADL'd at bp 2,280;
/// margin zeroed exactly, no bad debt.
#[tokio::test]
async fn example_14_insolvent_empty_book_short() {
    run_single(SingleCase {
        side: Side::Short,
        alice_margin: 2_800,
        oracle: 2_300,
        carol_size: 0,
        adl: Some(Adl {
            size_raw: 10_000_000,
            price: 2_280,
        }),
        alice_margin_raw: 0,
        alice_pos: 0,
        bob_margin_raw: 12_800_000_000,
        bob_pos: 0,
        insurance_fund_raw: 0,
        bad_debt: None,
    })
    .await;
}

/// Example 15 — mirror of example 7: two shorts, solvent. Equity 3,435, MM
/// 3,700, deficit 265 → close 0.1 BTC at bp = 53,000 + 3,435 = 56,435.
/// Margin 7,435 − 643.50 − 5.30 (fee) = 6,786.20.
#[tokio::test]
async fn example_15_two_positions_solvent_short() {
    run_double(DoubleCase {
        side: Side::Short,
        alice_margin: 7_435,
        oracle_eth: 2_100,
        oracle_btc: 53_000,
        btc_adl: Adl {
            size_raw: 100_000,
            price: 56_435,
        },
        eth_adl: None,
        alice_margin_raw: 6_786_200_000,
        bob_margin_raw: 12_643_500_000,
        insurance_fund_raw: 5_300_000,
    })
    .await;
}

/// Example 16 — mirror of example 8: two shorts, insolvent (equity −565).
/// BTC fully ADL'd at bp = 56,000 − 565 = 55,435 (zeroing equity), then ETH
/// ADL'd at the 2,200 oracle. No bad debt; Bob nets Alice's whole 7,435
/// margin.
#[tokio::test]
async fn example_16_two_positions_insolvent_short() {
    run_double(DoubleCase {
        side: Side::Short,
        alice_margin: 7_435,
        oracle_eth: 2_200,
        oracle_btc: 56_000,
        btc_adl: Adl {
            size_raw: 1_000_000,
            price: 55_435,
        },
        eth_adl: Some(Adl {
            size_raw: 10_000_000,
            price: 2_200,
        }),
        alice_margin_raw: 0,
        bob_margin_raw: 19_435_000_000,
        insurance_fund_raw: 0,
    })
    .await;
}

// --------------- edge case: negative margin, positive equity -----------------

/// Reads the perps insurance fund.
async fn insurance_fund(suite: &mut TestSuiteNaive, perps: Addr) -> UsdValue {
    let state: perps::State = suite
        .query_wasm_smart(perps, perps::QueryStateRequest {})
        .should_succeed();
    state.insurance_fund
}

/// A cross-margined account can reach liquidation with **negative margin but
/// positive equity**. Here Alice withdraws against her open long's unrealised
/// profit (the case-(a) path), driving her margin negative while she stays
/// solvent; a later price drop makes her liquidatable. The partial close must
/// NOT be mistaken for bad debt — the insurance fund must grow by the
/// liquidation fee rather than be drained — and Alice's margin must stay
/// negative (not floored to zero).
///
/// Mirrors the "negative margin, positive equity" edge case in
/// `book/perps/4-liquidation-and-adl.md`.
#[tokio::test]
async fn negative_margin_positive_equity_no_bad_debt() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let pair = pair_id();

    seed_prices(&mut suite, &mut accounts, ENTRY_ETH, None).await;
    configure(&mut suite, &mut accounts, contracts.perps, false).await;

    // Bob makes at the entry price; Alice takes a long 10 ETH @ $2,000.
    deposit(
        &mut suite,
        &mut accounts.user2,
        contracts.perps,
        BOB_MARGIN_SINGLE,
    )
    .await;
    submit_limit(
        &mut suite,
        &mut accounts.user2,
        contracts.perps,
        &pair,
        -10,
        ENTRY_ETH,
    )
    .await;

    deposit(&mut suite, &mut accounts.user1, contracts.perps, 2_500).await;
    submit_market(&mut suite, &mut accounts.user1, contracts.perps, &pair, 10).await;

    // ETH rises to $2,400: Alice's long shows +$4,000 unrealised. Equity
    // $6,500, used IM $2,400, available $4,100. She withdraws $3,950 — more
    // than her $2,500 deposit — driving margin to −$1,450 while staying solvent.
    // This is the case-(a) withdraw-against-unrealised-profit path.
    seed_prices(&mut suite, &mut accounts, 2_400, None).await;

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Withdraw {
                amount: UsdValue::new_int(3_950),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let alice = query_user(&mut suite, contracts.perps, accounts.user1.address())
        .await
        .unwrap();
    assert!(
        alice.margin.is_negative(),
        "withdrawal against unrealised profit should drive margin negative"
    );

    // ETH falls to $2,200: equity = −1,450 + 10×200 = 550; MM = 1,100 ⇒
    // liquidatable, but solvent.
    seed_prices(&mut suite, &mut accounts, 2_200, None).await;

    // Carol rests a deep bid at the oracle so the whole close fills on the book
    // (no ADL), regardless of small funding drift in the exact close size.
    deposit(
        &mut suite,
        &mut accounts.user3,
        contracts.perps,
        CAROL_MARGIN,
    )
    .await;
    submit_limit(
        &mut suite,
        &mut accounts.user3,
        contracts.perps,
        &pair,
        10,
        2_200,
    )
    .await;

    let insurance_before = insurance_fund(&mut suite, contracts.perps).await;

    let events = liquidate(&mut suite, &mut accounts, contracts.perps).await;

    // The account is solvent (equity > 0) throughout ⇒ no bad debt.
    assert!(
        search_bad_debt(&events).is_empty(),
        "solvent account must not produce bad debt"
    );
    // The book absorbs the whole close ⇒ no ADL.
    assert!(
        search_deleveraged(&events).is_empty(),
        "close fills on the book; no ADL expected"
    );

    // Alice keeps a (long) position and her margin stays negative — the bug
    // would have floored it to zero.
    let alice = query_user(&mut suite, contracts.perps, accounts.user1.address())
        .await
        .unwrap();
    assert!(
        alice.margin.is_negative(),
        "margin must remain negative for a solvent account"
    );
    let pos = alice
        .positions
        .get(&pair)
        .expect("Alice retains a partial position");
    assert!(pos.size.is_positive(), "still long after the partial close");

    // The insurance fund GREW by the liquidation fee; the bug would instead
    // have drained it by the phantom bad debt.
    let insurance_after = insurance_fund(&mut suite, contracts.perps).await;
    assert!(
        insurance_after > insurance_before,
        "insurance fund should grow by the fee, not be drained: \
         {insurance_before} -> {insurance_after}"
    );
}
