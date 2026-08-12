# Dango Contract System

Dango is a suite of smart contracts that together form a perpetual
futures exchange, oracle, token ledger, bridge aggregator, and account system. All
contracts are first-party and execute natively via `RustVm`.

## 1. Shared Types (`dango/exchange/types/`)

All contracts reference a central `AppConfig` that stores addresses of every system
contract:

```rust
pub struct AppAddresses {
    pub account_factory: Addr,
    pub dex: Addr,
    pub gateway: Addr,
    pub hyperlane: Hyperlane<Addr>,
    pub oracle: Addr,
    pub perps: Addr,
    pub warp: Addr,
}
```

Other shared types include authentication types (`Key`, `Signature`, `Credential`,
`SignDoc`, `Metadata`) and price types
(`PrecisionlessPrice`, `PrecisionedPrice`).

## 2. Bank (`dango/exchange/bank/`)

The bank contract manages all token balances, transfers, mints, and burns.

### State layout

| Storage              | Key                    | Value      | Purpose                                         |
| -------------------- | ---------------------- | ---------- | ----------------------------------------------- |
| `NAMESPACE_OWNERS`   | `Part` (denom segment) | `Addr`     | Who can mint/burn tokens under this namespace   |
| `METADATAS`          | `Denom`                | `Metadata` | Token name, symbol, decimals                    |
| `SUPPLIES`           | `Denom`                | `Uint128`  | Total supply per denom                          |
| `BALANCES`           | `(Addr, Denom)`        | `Uint128`  | Account balances                                |
| `ORPHANED_TRANSFERS` | `(Addr, Addr)`         | `Coins`    | Dead-letter transfers to non-existent contracts |
| `TRANSFERS_ENABLED`  | --                     | `bool`     | Whether transfers are allowed; absent = allowed |

### Operations

- **Transfer.** Moves coins between accounts. This is implemented at the host level
  via `BankMsg::Transfer`, not as a contract execute message.
- **Mint.** `Mint { to, coins }` -- caller must be the namespace owner for each denom.
  If the recipient contract doesn't exist, coins go to `ORPHANED_TRANSFERS`.
- **Burn.** `Burn { from, coins }` -- caller must be the namespace owner.
- **Force transfer.** `ForceTransfer { from, to, coins }` -- namespace owner can
  move funds arbitrarily. Used by the perps contract to settle PnL.
- **Set transfers enabled.** `SetTransfersEnabled(bool)` -- chain owner only.

### Disabling transfers

Every balance movement caused by a transfer passes through `bank_execute`: the
`Transfer` message, the funds attached to an `Execute` or `Instantiate` message,
and the gas fee withheld by the state machine. That makes it the one place where
transfers can be switched off, which the wind-down does.

While disabled, a transfer goes through only if it satisfies both conditions:

1. The sender or the recipient is the gateway contract, or the recipient is the
   chain owner. The first two cover bridge deposits, withdrawals, and refunds;
   the third covers the gas fee.
2. The recipient exists. No new orphaned transfer may be created, even on an
   allowed leg, or a bridge deposit to an account that was never created would
   strand tokens in the bank again.

`mint` carries the same recipient check, since it has its own orphan branch.
`burn` and `RecoverTransfer` are not gated: neither can strand funds once the
map is empty and no new entry can be created.

An absent `TRANSFERS_ENABLED` means enabled, so a chain created before the
wind-down needs no genesis change and no migration of this item.

### Access control

Namespace ownership is assigned once by the chain owner and cannot be overwritten.
For example, the perps contract owns the `perp/` namespace.

### Security considerations

- **Orphaned transfers:** If a contract is instantiated but not yet registered, mints
  to it become dead letters. Recovery requires an explicit `RecoverTransfer` call.
  There is no automatic expiry or governance recovery.
- **Trust in namespace owners:** The bank unconditionally trusts namespace owners for
  mint/burn/force-transfer. A bug in the perps contract could allow unlimited minting
  of `perp/*` tokens.

## 3. Account Factory (`dango/exchange/account-factory/`)

Creates and manages user accounts.

### State layout

| Storage              | Key         | Value                             |
| -------------------- | ----------- | --------------------------------- |
| `CODE_HASH`          | --          | `Hash256` (account contract code) |
| `NEXT_USER_INDEX`    | --          | `Counter<UserIndex>`              |
| `NEXT_ACCOUNT_INDEX` | --          | `Counter<AccountIndex>`           |
| `USERS`              | `UserIndex` | `User { name, accounts, keys }`   |
| (Index) `by_key`     | `Hash256`   | → `UserIndex` (MultiIndex)        |
| (Index) `by_account` | `Addr`      | → `UserIndex` (UniqueIndex)       |
| (Index) `by_name`    | `Username`  | → `UserIndex` (UniqueIndex)       |

### User structure

```rust
pub struct User {
    pub name: Option<Username>,                  // Immutable once set
    pub accounts: BTreeMap<AccountIndex, Addr>,  // Max 5 accounts
    pub keys: BTreeMap<Hash256, Key>,            // All signing keys
}
```

### Registration flow

1. User sends tokens to the account factory (deposit ≥ `min_deposit`).
2. User sends a `RegisterUser` message with a signed `RegisterUserData` containing
   the chain ID.
3. Factory verifies signature, creates a new `User` record, deploys an account
   contract, and optionally registers a referrer with the perps contract.

**Constraints:**

- Exactly one message per registration tx (prevents batching attacks).
- Username is immutable after being set.
- Maximum 5 accounts per user.
- Nonce jump limited to 100 (prevents DoS on the nonce set).

## 4. Account (`dango/exchange/account/`)

Single-signature account contract, one instance per user account.

### State

| Storage       | Value                                        |
| ------------- | -------------------------------------------- |
| `STATUS`      | `AccountStatus` (Inactive / Active / Frozen) |
| `SEEN_NONCES` | `BTreeSet<Nonce>` (last 20 nonces)           |

### Authentication flow

When the host receives a transaction, it calls the sender account's `authenticate()`:

1. Deserialize the credential from `tx.credential`.
2. Verify the account is Active (or in Simulate mode).
3. Verify the nonce is valid (not seen, not too far ahead).
4. Verify the signature against the signing key registered in the factory.
5. Return `Response`.

### Operations

- **Force withdrawal.** `ForceWithdrawal { denom, remote, recipient }` -- chain
  owner only. Sends the account's entire balance of `denom` to `recipient` on the
  remote chain, by calling the gateway's `TransferRemote` with the balance
  attached.

  This exists for the wind-down: balances left behind by users who don't withdraw
  before the shutdown are returned to the address they deposited from, which the
  owner supplies as `recipient`. It carries no privilege inside the gateway --
  routes, reserves, fees, personal quotas, and rate limits all apply as they
  would to a withdrawal the user makes themselves, and the resulting withdrawal
  request still needs the guardian's or the owner's approval.

  It works on an `Inactive` account. The account is not the transaction sender,
  so `authenticate` is never involved, and funds attached to an `Execute`
  message skip the recipient's `receive` hook.

## 5. Gas fees

Gas fees are handled directly by the state machine; there is no fee contract. The
parameters live in the chain `Config`:

| Field            | Meaning                                                                                                      |
| ---------------- | ------------------------------------------------------------------------------------------------------------ |
| `gas_token`      | The denom in which fees are paid.                                                                            |
| `gas_fee_rate`   | The amount of `gas_token` charged per unit of gas.                                                           |
| `gas_exemptions` | Senders that pay no fee (e.g. the oracle and the account factory, which submit protocol-level transactions). |

Before a transaction is authenticated, the state machine withholds `ceil(gas_limit * gas_fee_rate)`
of `gas_token` from the sender and credits it to the chain owner.
The fee is charged even if authentication or message execution fails. There is no
refund of unused gas.

## 6. Oracle (`dango/exchange/oracle/`)

Price feed aggregation for derivatives trading.

### State

| Storage                | Key             | Value                |
| ---------------------- | --------------- | -------------------- |
| `PRICE_SOURCES`        | `Denom`         | `PriceSource`        |
| `PYTH_TRUSTED_SIGNERS` | `[u8]` (pubkey) | `Timestamp` (expiry) |
| `PYTH_PRICES`          | `PythId`        | `Price`              |

### Price structure

```rust
pub struct Price {
    pub humanized_price: UsdPrice, // e.g., 50000.0 for $50k BTC
    pub timestamp: Timestamp,      // Feed age
}
```

### Trust model

- The oracle trusts Pyth network signers whose public keys are registered in
  `PYTH_TRUSTED_SIGNERS` with expiry timestamps.
- The chain owner (governance) controls which signers are trusted.
- There is no automated slashing or removal of malicious signers -- governance
  intervention is required.
- Consuming contracts (perps) enforce staleness checks before using prices.

## 7. Perpetual Futures Exchange (`dango/exchange/perps/`)

The primary audit target. A leveraged perpetual futures exchange with a vault-based
counterparty (market maker).

> **Note:** Detailed mechanism design is documented separately in the
> [Perps section](../perps/1-margin.md) of this book. This chapter focuses on the
> smart contract implementation details relevant to security auditing.

### Source files

```text
dango/exchange/perps/src/
├── lib.rs                  # Entry points (instantiate, execute, query, cron_execute)
├── state.rs                # All storage definitions
├── query.rs                # Query implementations
├── cron.rs                 # Scheduled tasks (funding, conditional orders)
├── core/                   # Pure business logic
│   ├── margin.rs           # Equity, maintenance margin, available margin
│   ├── funding.rs          # Funding rate computation, impact prices
│   ├── fees.rs             # Trading fee calculations (volume-tiered)
│   ├── closure.rs          # Liquidation eligibility, closeout calculations
│   ├── vault.rs            # Vault quoting (bid/ask sizes and prices)
│   ├── fill.rs             # Order fill execution
│   ├── oi.rs               # Open interest constraints
│   ├── liq_price.rs        # Liquidation price computation
│   ├── target_price.rs     # Price constraints for orders
│   ├── min_size.rs         # Minimum order size validation
│   └── decompose.rs        # Decomposing fills into open/close portions
├── trade/                  # State mutations for trading
│   ├── submit_order.rs
│   ├── submit_conditional_order.rs
│   ├── cancel_order.rs
│   ├── cancel_conditional_order.rs
│   ├── deposit.rs
│   └── withdraw.rs
├── vault/                  # Vault (LP) operations
│   ├── add_liquidity.rs
│   ├── remove_liquidity.rs
│   └── refresh.rs          # Vault market-making order placement
├── maintain/               # Maintenance operations
│   ├── configure.rs        # Parameter updates (owner-only)
│   └── liquidate.rs        # Forced position closeout
├── referral/               # Referral system
├── volume.rs               # Trading volume accumulation
├── position_index.rs       # Position tracking by entry price
└── liquidity_depth.rs      # Order book depth aggregation
```

### State layout

**Global state:**

```rust
STATE: Item<State> {
    last_funding_time: Timestamp,
    vault_share_supply: Uint128,
    insurance_fund: UsdValue,      // Covers bad debt from liquidations
    treasury: UsdValue,            // Accumulated protocol fees
}

PARAM: Item<Param> {
    max_unlocks: u32,
    max_open_orders: u32,
    maker_fee_rates: RateSchedule,      // Volume-tiered schedule
    taker_fee_rates: RateSchedule,
    protocol_fee_rate: Udec128,         // Fraction of fees → treasury
    liquidation_fee_rate: Udec128,
    liquidation_buffer_ratio: Udec128,
    funding_period: Duration,
    vault_total_weight: Udec128,
    vault_cooldown_period: Duration,
    referral_active: bool,
    min_referrer_volume: UsdValue,
    referrer_commission_rates: RateSchedule,
    vault_deposit_cap: Option<UsdValue>,
}
```

**Per-pair state:**

```rust
PAIR_PARAMS: Map<&PairId, PairParam> {
    tick_size, min_order_size, max_abs_oi,
    max_abs_funding_rate,
    initial_margin_ratio,               // 1/leverage (e.g., 0.1 = 10x)
    maintenance_margin_ratio,           // Liquidation trigger
    impact_size,                        // Notional for impact price sampling
    vault_liquidity_weight,             // Fraction of vault margin allocated
    vault_half_spread,                  // Base bid-ask spread around oracle
    vault_max_quote_size,               // Max single-side vault order size
    vault_size_skew_factor,             // Inventory skew → size tilt
    vault_spread_skew_factor,           // Inventory skew → spread tilt
    vault_max_skew_size,                // Skew saturation point
    bucket_sizes: BTreeSet<UsdPrice>,   // Liquidity depth granularities
}

PAIR_STATES: Map<&PairId, PairState> {
    long_oi,                            // Total long open interest
    short_oi,                           // Total short open interest (abs)
    funding_per_unit,                   // Cumulative funding accumulator
    funding_rate,                       // Current per-day rate (clamped)
}
```

**Per-user state:**

```rust
USER_STATES: IndexedMap<Addr, UserState> {
    margin: UsdValue,                   // Deposited collateral (USDC)
    vault_shares: Uint128,              // LP shares owned
    positions: BTreeMap<PairId, Position>,
    unlocks: VecDeque<Unlock>,          // Pending vault withdrawals
    reserved_margin: UsdValue,          // Collateral reserved for resting orders
    open_order_count: u32,              // Resting limit order count
}

Position {
    size: Int128,                       // Positive=long, negative=short
    entry_price: UsdPrice,
    entry_funding_per_unit: Dec128,
    conditional_order_above: Option<ConditionalOrder>,
    conditional_order_below: Option<ConditionalOrder>,
}
```

**Order book:**

```rust
BIDS: IndexedMap<OrderKey, LimitOrder>   // OrderKey = (PairId, Price, OrderId)
ASKS: IndexedMap<OrderKey, LimitOrder>

// ADL position tracking (sorted by entry price for selection)
LONGS: Set<(PairId, UsdPrice, Addr)>
SHORTS: Set<(PairId, UsdPrice, Addr)>
```

**Other state:**

```rust
VOLUMES: Map<(Addr, Timestamp), UsdValue>  // Per-user per-day volume
REFEREE_TO_REFERRER: Map<UserIndex, UserIndex>
FEE_SHARE_RATIO: Map<UserIndex, FeeShareRatio>
COMMISSION_RATE_OVERRIDES: Map<UserIndex, CommissionRate>
```

### Critical flows

#### Order submission (`trade/submit_order.rs`)

1. Load user state and pair state/params.
2. Validate: minimum size (or reduce-only exempt), tick alignment, slippage vs oracle
   (market orders), max open orders.
3. Decompose order into closing portion (vs existing position) and opening portion
   (new risk).
4. For opening portion: check OI constraints (`long_oi + size ≤ max_abs_oi`) and
   initial margin (`available_margin ≥ required`).
5. Match against resting orders in the order book (which may include orders
   placed by the vault or by other traders).
6. For fills: compute trading fee (volume-tiered), apply funding
   (`entry_funding_per_unit = current`), settle PnL.
7. Resting (unfilled) portion: reserve margin, place on book with TP/SL children.
8. Post-trade validation: `available_margin ≥ 0` (reverts entire order otherwise).

#### Funding (`cron.rs` → `core/funding.rs`)

1. Sample order book impact prices (best bid/ask for `impact_size` notional).
2. Compute midpoint premium vs oracle price.
3. Clamp to `max_abs_funding_rate` per day, scale by elapsed time.
4. Update `pair_state.funding_per_unit += delta`.
5. Funding settles lazily on position close: `accrued = size × (current_cumulative -
   entry_cumulative)`.

#### Liquidation (`maintain/liquidate.rs`)

1. Compute equity = `margin + Σ(unrealized_pnl) - Σ(accrued_funding)`.
2. Compute maintenance margin = `Σ(|size| × price × mm_ratio)`.
3. If `equity < maintenance_margin`:
   a. Cancel all resting orders (refund reserved margin).
   b. Close **enough** of the user's positions to restore
      `equity ≥ maintenance_margin` (with a buffer controlled by
      `liquidation_buffer_ratio`). Not all positions are necessarily closed.
   c. Positions are closed against resting orders in the book at the target
      price. Only if there is insufficient book liquidity within the target
      price does the engine resort to **auto-deleveraging (ADL)** against
      profitable counter-parties.
   d. Collect liquidation fee → insurance fund.
   e. Cover any remaining bad debt from insurance fund.

#### Vault (LP) system (`vault/`)

The vault acts as a passive market maker, placing orders around the oracle price:

- **Share price:** `vault_equity / vault_shares + VIRTUAL_ASSETS / VIRTUAL_SHARES`
  (ERC-4626-style virtual shares prevent share inflation attacks).
- **Add liquidity:** Mint shares at current share price. Slippage-protected via
  `min_shares_to_mint`.
- **Remove liquidity:** Burn shares, queue withdrawal for `vault_cooldown_period`.
- **Quoting:** Inventory-based skew tilts bid/ask sizes and spreads to manage
  directional exposure.

```text
bid_price = oracle × (1 - half_spread × (1 - skew × spread_skew_factor))
ask_price = oracle × (1 + half_spread × (1 + skew × spread_skew_factor))

skew = vault_inventory / vault_max_skew_size  [clamped to [-1, 1]]
```

### Access control

| Operation                            | Who can call            |
| ------------------------------------ | ----------------------- |
| `Configure` (params)                 | Chain owner only        |
| `SubmitOrder`, `Deposit`, `Withdraw` | Any active account      |
| `Liquidate`                          | Anyone (permissionless) |
| `AddLiquidity`, `RemoveLiquidity`    | Any active account      |
| `cron_execute`                       | Chain (automatic)       |

## 8. Gateway (`dango/exchange/gateway/`)

Bridge aggregator for cross-chain token transfers.

### State

| Storage           | Key               | Value                        |
| ----------------- | ----------------- | ---------------------------- |
| `ROUTES`          | `(Addr, Remote)`  | `Denom`                      |
| `REVERSE_ROUTES`  | `(Denom, Remote)` | `Addr`                       |
| `RATE_LIMITS`     | --                | `BTreeMap<Denom, RateLimit>` |
| `WITHDRAWAL_FEES` | `(Denom, Remote)` | `Uint128`                    |
| `OUTBOUND_QUOTAS` | `Denom`           | `Uint128`                    |

### Cross-chain flow

**Inbound:** Remote bridge delivers tokens → gateway mints wrapped tokens → transfers
to recipient (or orphaned transfer if contract not deployed).

**Outbound:** User sends tokens to gateway → rate limit check → withdrawal fee deducted
→ local tokens burned → cross-chain message sent.

### Rate limiting

```rust
RateLimit = Bounded<Udec128, ZeroInclusiveOneExclusive>
// e.g., 0.1 = max 10% of supply per period
```

### Trust model

Trusts Hyperlane validators/ISM. Governance controls bridge configuration, fees, and
rate limits.

## 9. Vesting (`dango/exchange/vesting/`)

Token vesting with linear schedules and optional cliffs.

### State

| Storage              | Key    | Value      |
| -------------------- | ------ | ---------- |
| `UNLOCKING_SCHEDULE` | --     | `Schedule` |
| `POSITIONS`          | `Addr` | `Position` |

## 10. Upgrade (`dango/exchange/upgrade/`)

Handles state migrations during chain upgrades. Example: migrating `PairParam` to add
new vault skew fields with zero defaults.

## 11. Inter-Contract Interaction Map

```text
┌──────────────┐  RegisterUser ┌──────────┐  mint   ┌──────┐
│ Account      │◄──────────────│ Account  │────────►│ Bank │
│ (per user)   │               │ Factory  │         │      │
└──────────────┘               └────┬─────┘         └──┬───┘
                                    │ referral         │
                                    ▼                  │
                              ┌──────────┐             │
                              │  Perps   │◄────────────┘ force_transfer
                              │          │  (PnL settlement)
                              └────┬─────┘
                                   │ query prices
                                   ▼
                              ┌──────────┐
                              │  Oracle  │
                              └──────────┘
```

Key interaction patterns:

- **Perps ↔ Bank:** Force-transfers for margin deposits/withdrawals and PnL settlement.
- **Perps → Oracle:** Price queries with staleness checks.
- **Account Factory → Perps:** Referral registration on user creation.
- **Account → Factory:** Key and nonce lookups during authentication.

## 12. Security-Relevant Properties

### Invariants to verify

1. **Bank solvency:** `Σ(BALANCES[addr][denom]) = SUPPLIES[denom]` for all denoms.
2. **Perps margin:** For any non-liquidatable user, `equity ≥ maintenance_margin`.
3. **OI balance:** `pair_state.long_oi - pair_state.short_oi = Σ(positions.size)`
   across all users for each pair.
4. **Vault shares:** `STATE.vault_share_supply = Σ(user_state.vault_shares)`.
5. **Reserved margin consistency:** `user_state.reserved_margin =
   Σ(resting_order.margin_required)` for that user.
6. **Order count:** `user_state.open_order_count =` count of resting orders for
   that user.

### Trust boundaries within Dango

| Contract        | Trusts                             | Trusted by                     |
| --------------- | ---------------------------------- | ------------------------------ |
| Bank            | Namespace owners (unconditionally) | Everyone (for balance queries) |
| Oracle          | Pyth signers (governance-managed)  | Perps (for price feeds)        |
| Perps           | Oracle (prices), Bank (balances)   | Users (for margin custody)     |
| Account Factory | --                                 | Accounts (for key lookups)     |
| Gateway         | Hyperlane validators               | Bank (for mint/burn)           |
