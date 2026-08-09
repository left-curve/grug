use {
    crate::{
        VIRTUAL_ASSETS, VIRTUAL_SHARES,
        core::{compute_available_margin, compute_user_equity},
        querier::NoCachePerpQuerier,
        state::{PARAM, STATE, USER_STATES},
        trade::ensure_trading_enabled,
    },
    anyhow::ensure,
    dango_math::{IsZero, MultiplyRatio, Number as _, Signed, Uint128},
    dango_order_book::UsdValue,
    dango_primitives::{MutableCtx, Response},
    dango_types::perps::{LiquidityAdded, Param, State, UserState},
};

/// Add liquidity to the counterparty vault by transferring margin to the vault.
///
/// Mutates: `STATE` (vault_share_supply), `USER_STATES` (margin, vault_shares).
///
/// Returns: `Response`.
pub fn add_liquidity(
    ctx: MutableCtx,
    amount: UsdValue,
    min_shares_to_mint: Option<Uint128>,
) -> anyhow::Result<Response> {
    // ---------------------------- 1. Preparation -----------------------------

    ensure!(ctx.funds.is_empty(), "no funds expected");

    let param = PARAM.load(ctx.storage)?;

    ensure_trading_enabled(&param)?;

    let mut state = STATE.load(ctx.storage)?;

    let mut user_state = USER_STATES
        .may_load(ctx.storage, ctx.sender)?
        .unwrap_or_default();

    let mut vault_user_state = USER_STATES
        .may_load(ctx.storage, ctx.contract)?
        .unwrap_or_default();

    let perp_querier = NoCachePerpQuerier::new_local(ctx.storage);

    // --------------------------- 2. Business logic ---------------------------

    let shares_minted = _add_liquidity(
        &perp_querier,
        &param,
        &mut state,
        &mut user_state,
        &mut vault_user_state,
        amount,
        min_shares_to_mint,
    )?;

    // ------------------------ 3. Apply state changes -------------------------

    STATE.save(ctx.storage, &state)?;
    USER_STATES.save(ctx.storage, ctx.sender, &user_state)?;
    USER_STATES.save(ctx.storage, ctx.contract, &vault_user_state)?;

    #[cfg(feature = "tracing")]
    {
        tracing::info!(
            user = %ctx.sender,
            %amount,
            %shares_minted,
            "Liquidity added"
        );
    }

    #[cfg(feature = "metrics")]
    {
        metrics::histogram!(crate::metrics::LABEL_VAULT_DEPOSIT_AMOUNT).record(amount.to_f64());
    }

    Ok(Response::new().add_event(LiquidityAdded {
        user: ctx.sender,
        amount,
        shares_minted,
    })?)
}

/// The actual logic for handling the add-liquidity operation.
///
/// Mutates:
/// - `state` (vault_share_supply)
/// - `user_state` (margin, vault_shares)
/// - `vault_user_state` (margin)
///
/// Returns: the number of shares minted.
fn _add_liquidity(
    perp_querier: &NoCachePerpQuerier,
    param: &Param,
    state: &mut State,
    user_state: &mut UserState,
    vault_user_state: &mut UserState,
    amount: UsdValue,
    min_shares_to_mint: Option<Uint128>,
) -> anyhow::Result<Uint128> {
    // ----------------------- Step 1. Validate deposit ------------------------

    // 1. Deposit amount must be non-zero.
    // 2. The user must have enough available margin.
    // 3. Vault deposit cap must not be exceeded.

    ensure!(
        amount.is_positive(),
        "amount of margin to add must be positive"
    );

    let available_margin = compute_available_margin(perp_querier, user_state)?;
    ensure!(
        available_margin >= amount,
        "insufficient available margin: {available_margin} (available) < {amount} (requested)"
    );

    if let Some(cap) = param.vault_deposit_cap {
        let post_deposit_margin = vault_user_state.margin.checked_add(amount)?;
        ensure!(
            post_deposit_margin <= cap,
            "vault deposit cap exceeded! current ({}) + deposit ({}) > cap ({})",
            vault_user_state.margin,
            amount,
            cap,
        );
    }

    // --------------------- Step 2. Compute vault equity ----------------------

    let vault_equity = compute_user_equity(perp_querier, vault_user_state)?;

    // Add virtual shares to the current vault share supply to arrive at the
    // effective supply.
    let effective_supply = state.vault_share_supply.checked_add(VIRTUAL_SHARES)?;

    // Add virtual asset to vault equity to arrive at the effective equity.
    let effective_equity = vault_equity.checked_add(VIRTUAL_ASSETS)?;

    ensure!(
        effective_equity.is_positive(),
        "vault is in catastrophic loss! deposit disabled. effective equity: {effective_equity}"
    );

    // -------------------------- Step 3. Mint shares --------------------------

    // Multiply first, then divide, to avoid precision loss from intermediate
    // rounding in Dec128_6 (6 decimal places). The scale factors cancel since
    // both amount and effective_equity are UsdValue (same Dec128_6 scale).
    let shares_to_mint = effective_supply.checked_multiply_ratio_floor(
        amount.into_inner().checked_into_unsigned()?.0,
        effective_equity.into_inner().checked_into_unsigned()?.0,
    )?;

    ensure!(
        shares_to_mint.is_non_zero(),
        "amount of vault shares to be minted is zero"
    );

    if let Some(min_shares_to_mint) = min_shares_to_mint {
        ensure!(
            shares_to_mint >= min_shares_to_mint,
            "too few shares minted: {shares_to_mint} (actual) < {min_shares_to_mint} (expected)"
        );
    }

    // Increse vault shares supply.
    state
        .vault_share_supply
        .checked_add_assign(shares_to_mint)?;

    // Mint new vault shares to the user.
    user_state.vault_shares.checked_add_assign(shares_to_mint)?;

    // Deduct margin from user.
    user_state.margin.checked_sub_assign(amount)?;

    // Add the margin to the vault.
    vault_user_state.margin.checked_add_assign(amount)?;

    Ok(shares_to_mint)
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_math::{NumberConst, Uint128},
        dango_order_book::{Dimensionless, FundingPerUnit, Quantity, UsdPrice},
        dango_primitives::{MockStorage, btree_map, hash_map},
        dango_types::{
            constants::eth,
            perps::{PairParam, PairState, Position, UserState},
        },
    };

    fn default_param() -> Param {
        Param::default()
    }

    fn state_with_supply(supply: u128) -> State {
        State {
            vault_share_supply: Uint128::new(supply),
            ..Default::default()
        }
    }

    // ---- Test 1: first deposit into an empty vault (no pairs) ----
    #[test]
    fn first_deposit_empty_vault() {
        let storage = MockStorage::new();
        let param = default_param();
        let mut state = state_with_supply(0);
        let mut user_state = UserState {
            margin: UsdValue::new_int(1),
            ..Default::default()
        };
        let mut vault_user_state = UserState::default();
        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_int(1),
            None,
        )
        .unwrap();

        assert_eq!(user_state.vault_shares, Uint128::new(1_000_000));
        assert_eq!(user_state.margin, UsdValue::ZERO);
    }

    // ---- Test 2: second deposit of same size into a non-empty vault ----
    #[test]
    fn second_deposit_same_size() {
        let storage = MockStorage::new();
        let param = default_param();
        let mut state = state_with_supply(1_000_000);
        let mut user_state = UserState {
            margin: UsdValue::new_int(1),
            ..Default::default()
        };
        let mut vault_user_state = UserState {
            margin: UsdValue::new_int(1),
            ..Default::default()
        };
        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_int(1),
            None,
        )
        .unwrap();

        assert_eq!(user_state.vault_shares, Uint128::new(1_000_000));
        assert_eq!(user_state.margin, UsdValue::ZERO);
    }

    // ---- Test 3: zero deposit ----
    #[test]
    fn zero_add_liquidity() {
        let storage = MockStorage::new();
        let param = default_param();
        let mut state = state_with_supply(0);
        let mut user_state = UserState::default();
        let mut vault_user_state = UserState::default();
        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        let err = _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::ZERO,
            None,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("amount of margin to add must be positive")
        );
    }

    // ---- Test 4: insufficient margin rejected ----
    #[test]
    fn insufficient_margin_rejected() {
        let storage = MockStorage::new();
        let param = default_param();
        let mut state = state_with_supply(0);
        let mut user_state = UserState {
            margin: UsdValue::new_permille(500),
            ..Default::default()
        };
        let mut vault_user_state = UserState::default();
        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        let err = _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_int(1),
            None,
        )
        .unwrap_err();

        assert!(err.to_string().contains("insufficient available margin"));
    }

    // ---- Test 6: min_shares passes ----
    #[test]
    fn min_shares_passes() {
        let storage = MockStorage::new();
        let param = default_param();
        let mut state = state_with_supply(0);
        let mut user_state = UserState {
            margin: UsdValue::new_int(1),
            ..Default::default()
        };
        let mut vault_user_state = UserState::default();
        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_int(1),
            Some(Uint128::new(1_000_000)),
        )
        .unwrap();

        assert_eq!(user_state.vault_shares, Uint128::new(1_000_000));
    }

    // ---- Test 7: min_shares fails ----
    #[test]
    fn min_shares_fails() {
        let storage = MockStorage::new();
        let param = default_param();
        let mut state = state_with_supply(0);
        let mut user_state = UserState {
            margin: UsdValue::new_int(1),
            ..Default::default()
        };
        let mut vault_user_state = UserState::default();
        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        let err = _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_int(1),
            Some(Uint128::new(1_000_001)),
        )
        .unwrap_err();

        assert!(err.to_string().contains("too few shares minted"));
    }

    // ---- Test 11: large deposit no overflow ----
    #[test]
    fn large_deposit_no_overflow() {
        let storage = MockStorage::new();
        let param = default_param();

        let one_billion = 1_000_000_000_u128;
        let one_billion_shares: u128 = one_billion * 1_000_000;

        let mut state = state_with_supply(one_billion_shares);
        let mut user_state = UserState {
            margin: UsdValue::new_int(one_billion as i128),
            ..Default::default()
        };
        let mut vault_user_state = UserState {
            margin: UsdValue::new_int(one_billion as i128),
            ..Default::default()
        };
        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_int(one_billion as i128),
            None,
        )
        .unwrap();

        // With existing margin equal to deposit, shares should be close to supply
        // (slightly less due to virtual shares/assets dilution).
        assert!(user_state.vault_shares > Uint128::new(0));
    }

    // ---- Test: small deposit should not lose precision ----
    #[test]
    fn small_deposit_precision() {
        let storage = MockStorage::new();
        let param = default_param();

        // Vault: $2 margin, 2M shares.
        // effective_supply = 2M + 1M = 3M
        // effective_equity = $2 + $1 = $3 (raw = 3_000_000)
        //
        // Deposit $0.000001 (raw = 1).
        // Correct: floor(3_000_000 * 1 / 3_000_000) = 1 share.
        // Buggy (divide-first): ratio = 1 * 10^6 / 3_000_000 = 0 → zero shares.
        let mut state = state_with_supply(2_000_000);
        let mut user_state = UserState {
            margin: UsdValue::new_raw(1),
            ..Default::default()
        };
        let mut vault_user_state = UserState {
            margin: UsdValue::new_int(2),
            ..Default::default()
        };
        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        let shares = _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_raw(1),
            None,
        )
        .unwrap();

        assert_eq!(shares, Uint128::new(1));
    }

    // ---- Test 13: exact division yields exact shares ----
    #[test]
    fn exact_division_shares() {
        let storage = MockStorage::new();
        let param = default_param();
        let mut state = state_with_supply(2_000_000);
        let mut user_state = UserState {
            margin: UsdValue::new_int(1),
            ..Default::default()
        };
        let mut vault_user_state = UserState {
            margin: UsdValue::new_int(2),
            ..Default::default()
        };
        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_int(1),
            None,
        )
        .unwrap();

        // effective_supply = 3M, effective_equity = $3
        // floor(3M * 1M / 3M) = 1_000_000 (exact, no rounding needed).
        assert_eq!(user_state.vault_shares, Uint128::new(1_000_000));
    }

    // ---- Test: deposit within cap succeeds ----
    #[test]
    fn deposit_within_cap_succeeds() {
        let storage = MockStorage::new();

        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        let param = Param {
            vault_deposit_cap: Some(UsdValue::new_int(10)),
            ..Default::default()
        };
        let mut state = State {
            vault_share_supply: Uint128::ZERO,
            ..Default::default()
        };
        let mut user_state = UserState {
            margin: UsdValue::new_int(10),
            ..Default::default()
        };
        let mut vault_user_state = UserState::default();

        let shares = _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_int(5),
            None,
        )
        .unwrap();

        assert!(shares > Uint128::ZERO);
        assert_eq!(user_state.margin, UsdValue::new_int(5));
    }

    // ---- Test: deposit exceeding cap rejected ----
    #[test]
    fn deposit_exceeding_cap_rejected() {
        let storage = MockStorage::new();

        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        let param = Param {
            vault_deposit_cap: Some(UsdValue::new_int(8)),
            ..Default::default()
        };
        let mut state = State {
            vault_share_supply: Uint128::ZERO,
            ..Default::default()
        };
        let mut user_state = UserState {
            margin: UsdValue::new_int(10),
            ..Default::default()
        };
        let mut vault_user_state = UserState {
            margin: UsdValue::new_int(5),
            ..Default::default()
        };

        let err = _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_int(5),
            None,
        )
        .unwrap_err();

        assert!(err.to_string().contains("vault deposit cap exceeded"));
    }

    // ---- Test: deposit exactly at cap succeeds ----
    #[test]
    fn deposit_exactly_at_cap_succeeds() {
        let storage = MockStorage::new();

        let perp_querier = NoCachePerpQuerier::new_local(&storage);

        let param = Param {
            vault_deposit_cap: Some(UsdValue::new_int(10)),
            ..Default::default()
        };
        let mut state = State {
            vault_share_supply: Uint128::ZERO,
            ..Default::default()
        };
        let mut user_state = UserState {
            margin: UsdValue::new_int(10),
            ..Default::default()
        };
        let mut vault_user_state = UserState {
            margin: UsdValue::new_int(5),
            ..Default::default()
        };

        let shares = _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_int(5),
            None,
        )
        .unwrap();

        assert!(shares > Uint128::ZERO);
        assert_eq!(vault_user_state.margin, UsdValue::new_int(10));
    }

    /// Regression test for a bug observed on testnet where a user became
    /// immediately liquidatable after adding liquidity to the vault.
    ///
    /// Old logic: `ensure!(user_state.margin >= amount)` — only checks the raw
    /// margin balance, ignoring unrealized PnL, funding payments, and initial
    /// margin requirements for open positions.
    ///
    /// Consequence: a user with $250 margin, 1 ETH long (entry $2000, oracle
    /// $1950 → $50 unrealized loss), and 10% initial margin ratio has only $5
    /// of available margin. The old check would let them deposit $150 into the
    /// vault (since $250 >= $150), leaving equity of $50 against a $97.50
    /// maintenance margin — immediately liquidatable.
    ///
    /// Fix: `ensure!(compute_available_margin(...) >= amount)` — accounts for
    /// unrealized PnL, funding, initial margin on open positions, and reserved
    /// margin for resting orders. The operation is now rejected before any
    /// state mutation occurs.
    #[test]
    fn add_liquidity_rejects_when_available_margin_insufficient() {
        // User: $250 margin, 1 ETH long @ entry $2000.
        let mut user_state = UserState {
            margin: UsdValue::new_int(250),
            positions: btree_map! {
                eth::DENOM.clone() => Position {
                    size: Quantity::new_int(1),
                    entry_price: UsdPrice::new_int(2000),
                    entry_funding_per_unit: FundingPerUnit::new_int(0),
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            },
            ..Default::default()
        };

        // Pair: 10% initial margin ratio, 5% maintenance margin ratio.
        let pair_params = hash_map! {
            eth::DENOM.clone() => PairParam {
                initial_margin_ratio: Dimensionless::new_permille(100),
                maintenance_margin_ratio: Dimensionless::new_permille(50),
                ..Default::default()
            },
        };
        let pair_states = hash_map! {
            eth::DENOM.clone() => PairState {
                funding_per_unit: FundingPerUnit::new_int(0),
                index_price: UsdPrice::new_percent(195_000),
                ..Default::default()
            },
        };

        // Oracle: ETH at $1950 → unrealized loss of $50.
        // equity = $250 + (-$50) = $200
        // initial margin used = 1 * $1950 * 10% = $195
        // available margin = $200 - $195 = $5

        let perp_querier = NoCachePerpQuerier::new_mock(pair_params, pair_states);

        let param = default_param();
        let mut state = state_with_supply(0);
        let mut vault_user_state = UserState::default();

        // Attempting to add $150 when only $5 is available must fail.
        let err = _add_liquidity(
            &perp_querier,
            &param,
            &mut state,
            &mut user_state,
            &mut vault_user_state,
            UsdValue::new_int(150),
            None,
        )
        .unwrap_err();

        assert!(err.to_string().contains("insufficient available margin"));
    }
}
