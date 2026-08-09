# Liquidation & Auto-Deleveraging (ADL)

This document describes how the perpetual futures exchange protects itself from under-collateralised accounts and socialises losses via auto-deleveraging and the insurance fund.

## 1. Liquidation trigger

Every account has an **equity** and a **maintenance margin** (MM):

$$
\mathtt{equity} = \mathtt{collateralValue} + \sum \mathtt{unrealisedPnl} - \sum \mathtt{accruedFunding}
$$

$$
\mathtt{MM} = \sum |\mathtt{positionSize}| \times \mathtt{oraclePrice} \times \mathtt{mmr}
$$

where $\mathtt{mmr}$ is the per-pair maintenance-margin ratio. An account becomes **liquidatable** when

$$
\mathtt{equity} < \mathtt{MM}
$$

Strict inequality: an account whose equity exactly equals its MM is still safe. An account with no open positions is never liquidatable regardless of its equity.

## 2. Close schedule

When an account is liquidatable, the system computes the **minimum set of position closures** needed to restore it above maintenance margin.

1. For every open position, compute its MM contribution:

   $$
   \mathtt{mmContribution} = |\mathtt{size}| \times \mathtt{oraclePrice} \times \mathtt{mmr}
   $$

2. Sort positions by MM contribution **descending** (largest first).

3. Walk the sorted list and close just enough to cover the deficit:

   - $\mathtt{deficit} = \mathtt{MM} − \mathtt{equity} \;/\; (1 + b)$

     where $b$ is the global `liquidation_buffer_ratio` (default 0). When $b > 0$, positions are closed slightly beyond the maintenance boundary so the user's post-liquidation equity exceeds their remaining MM by a factor of $(1 + b)$, preventing repeated small liquidations from minor adverse price movements.

   - For each position:
     - If $\mathtt{deficit} \le 0$: stop
     - $\mathtt{closeSize} = \min \left( \left\lceil \frac{\mathtt{deficit}}{\mathtt{oraclePrice} \times \mathtt{mmr}} \right\rceil,\; |\mathtt{size}| \right)$
     - $\mathtt{deficit} \mathrel{-}= \mathtt{closeSize} \times \mathtt{oraclePrice} \times \mathtt{mmr}$

This produces a vector of $(\mathtt{pairId},\; \mathtt{closeSize})$ entries. Each $\mathtt{closeSize}$ has the **opposite sign** of the existing position (a long is closed with a sell, a short with a buy). Only positions that contribute to the deficit are touched and they may be **partially closed** when the deficit is small relative to the position.

## 3. Position closure

Each entry in the close schedule is executed in two phases:

### 3a. Order book matching

The close is submitted as an **immediate-or-cancel (IOC) limit order** against the on-chain order book. The order's limit price is the **bankruptcy price** (defined in [§3b](#3b-auto-deleveraging-adl)) when the account is solvent ($\mathtt{equity} > 0$), or the **oracle price** when insolvent. It matches resting limit orders at price-time priority. Any filled amount is settled normally (mark-to-market PnL between the entry price and the fill price).

### 3b. Auto-deleveraging (ADL)

If any quantity remains **unfilled** after the order book is exhausted, the system automatically deleverages against counter-parties. The unfilled remainder is closed against the **most profitable counter-positions** at the liquidated user's **bankruptcy price**.

**Counter-party selection:** Positions are indexed by the tuple $(\mathtt{pairId},\; \mathtt{entryPrice},\; \mathtt{user})$. For a long being liquidated (selling), the system finds shorts with the highest entry price (most profitable) first. For a short being liquidated (buying), it finds longs with the lowest entry price first.[^1]

[^1]: This does not perfectly rank by total PnL since it ignores accumulated funding fees, but is a reasonable and efficient approximation.

**Bankruptcy price:** A position's bankruptcy price (BP) is the fill price at which, if the **entire** position were closed at it, the user's total account equity would be exactly zero:

$$
\mathtt{bp} = \mathtt{oraclePrice} - \frac{\mathtt{equity}}{|\mathtt{positionSize}|} \quad (\text{for longs})
$$

$$
\mathtt{bp} = \mathtt{oraclePrice} + \frac{\mathtt{equity}}{|\mathtt{positionSize}|} \quad (\text{for shorts})
$$

The divisor is always the position's **full current size**, even when the close schedule closes only part of the position. An ADL fill at this price therefore moves exactly $\mathtt{equity} / |\mathtt{positionSize}|$ per unit closed from the user to the counter-party:

- If the user is **solvent** ($\mathtt{equity} > 0$), the BP sits slightly on the favourable side of the oracle for the counter-party (below oracle when closing a long, above when closing a short). The counter-party receives the user's per-unit equity share as compensation for the forced close; the user keeps the equity attributable to the unclosed remainder, and never goes negative.
- If the user is **insolvent** ($\mathtt{equity} \le 0$), the BP overshoots the oracle in the user's favour (above oracle when closing a long, below when closing a short). The counter-party is force-closed at a worse-than-oracle price — it absorbs what would otherwise be bad debt. The close schedule fully closes every position of an insolvent account, so a pure-ADL liquidation leaves the account at exactly zero equity.

ADL does not fill the counter-party's resting limit orders; their position is force-reduced directly. The shrunken position can, however, cause their resting **reduce-only** orders to be resized or cancelled, maintaining the invariant that the total size of a user's reduce-only orders never exceeds their position size.

Liquidation fills (both order-book and ADL) carry **zero trading fees** for both taker and maker.

Order-book fills during liquidation emit `order_filled` events with a `fill_id` just like regular matches (see the [events reference](8-api.md#7-events-reference)); ADL fills do _not_ — they use the separate `deleveraged` and `liquidated` events, which carry no `fill_id`, because ADL is a position transfer at the bankruptcy price rather than an order-book match.

## 4. Liquidation fee

After all positions in the schedule are closed, a one-time liquidation fee is charged:

$$
\mathtt{rawFee} = \mathtt{closedNotional} \times \mathtt{liquidationFeeRate}
$$

$$
\mathtt{remainingEquity} = \max (0,\; \mathtt{equityAfterCloses})
$$

$$
\mathtt{fee} = \min (\mathtt{rawFee},\; \mathtt{remainingEquity})
$$

where $\mathtt{equityAfterCloses}$ is the account's equity once the scheduled closes are settled — i.e. the post-close margin plus the unrealised PnL of any positions left open.

The fee is deducted from the user's margin and routed to the **insurance fund** (not the vault). It is capped at the remaining **equity** — not margin alone — so the fee itself never drives equity below zero and therefore never creates bad debt. The cap matters precisely when margin and equity diverge: an account can reach liquidation with negative margin but positive equity (its open positions hold unrealised profit), and capping at margin would skip a fee the account can clearly afford; conversely, capping at margin when remaining positions are underwater could charge a fee that pushes equity negative.

## 5. PnL settlement

All PnL from the liquidation fills (user, book makers, ADL counter-parties) is settled atomically as in-place USD margin adjustments — no token transfers occur. Both user and maker PnL are applied via the same settlement logic described in [Order matching §8](2-order-matching.md#8-pnl-settlement).

## 6. Bad debt

After PnL and fee settlement, if the user's **equity** is negative the absolute value is bad debt. The account is topped up to exactly zero equity — the bad debt is credited to the user's margin — and the same amount is subtracted from the **insurance fund**:

$$
\mathtt{badDebt} = |\min(0,\; \mathtt{equity\ after\ settlement})|
$$

$$
\mathtt{user.margin} \mathrel{+}= \mathtt{badDebt} \quad (\text{so } \mathtt{equity} \gets 0)
$$

$$
\mathtt{insuranceFund} \mathrel{-}= \mathtt{badDebt}
$$

Bad debt is a **negative-equity** condition, not merely a negative margin balance. A cross-margined account can carry negative margin while still solvent when its remaining positions hold unrealised profit (see [margin §3](1-margin.md)); recognising bad debt off the margin sign in that case would transfer insurance-fund value to a solvent user. When the account is fully closed — the normal insolvent path — no positions remain, so equity equals margin and crediting $\mathtt{badDebt}$ is identical to flooring margin to zero.

The insurance fund may go negative. A negative insurance fund represents unresolved bad debt — future liquidation fees will replenish it.

Note: when an insolvent account's positions are fully ADL'd at their bankruptcy prices — each computed from the account's equity at the moment that position is processed — the user's equity is zeroed by construction. Bad debt from ADL fills is therefore zero. Bad debt arises only from **book fills** at prices worse than the bankruptcy price (e.g., thin order books with deep bids/asks far from oracle); see [Example 4](#example-4--insolvent-close-fully-absorbed-by-the-book).

## 7. Insurance fund

The insurance fund is a separate pool from the vault that absorbs bad debt and is funded by liquidation fees.

**Funding:** Every liquidation fee ([§4](#4-liquidation-fee)) is credited to the insurance fund.

**Usage:** Every bad debt event ([§6](#6-bad-debt)) is debited from the insurance fund.

**Negative balance:** The insurance fund may go negative when accumulated bad debt exceeds accumulated fees. This is the simplest approach — no special trigger or intervention is needed. Future liquidation fees will naturally replenish the fund.

Other users' bad debt and liquidation fees never touch the vault's margin — this isolates liquidity providers from external liquidation losses. However, the vault itself is subject to liquidation like any other account. If the vault's equity falls below its maintenance margin, its positions are closed following the same procedure described above. The vault's own liquidation fee goes to the insurance fund, and any bad debt is absorbed by it.

## Examples

All examples use:

| Parameter                      | Value                                 |
| ------------------------------ | ------------------------------------- |
| Pairs                          | ETH / USD (1–6); plus BTC / USD (7–8) |
| Maintenance-margin ratio (mmr) | 5 % — both pairs                      |
| Liquidation-fee rate           | 0.1 %                                 |
| Liquidation buffer ratio ($b$) | 0                                     |
| Settlement currency            | USDC at \$1                           |

Cast:

- **Alice** is the account being liquidated.
- **Bob** holds the exact opposite position(s), opened against Alice at her entry price — being the most profitable counter-position, he is the ADL counter-party.
- **Carol** is a third-party maker who supplies order-book liquidity where stated.

Examples 1–6 cover a single position, ordered from the most ideal situation to the least: Alice solvent (1–3) then insolvent (4–6), with the order book absorbing all (1, 4), part (2, 5), or none (3, 6) of the close. Examples 7–8 cover an account with two positions. A final example covers the cross-margin edge case where the account reaches liquidation with **negative margin but positive equity** — the case the equity caps in §4 and §6 exist for.

All eight numbered examples — plus mirrored variants with the sides flipped — are implemented as end-to-end tests in [`dango/testing/tests/perps/liquidation_spec.rs`](https://github.com/left-curve/left-curve/blob/main/dango/testing/tests/perps/liquidation_spec.rs), asserting every figure below exactly. The negative-margin edge case is likewise covered by an end-to-end test there.

### Example 1 — Solvent; close fully absorbed by the book

**Setup**

|             | Alice       | Bob          | Carol               |
| ----------- | ----------- | ------------ | ------------------- |
| Position    | Long 10 ETH | Short 10 ETH | Bid 8 ETH @ \$1,800 |
| Entry price | \$2,000     | \$2,000      | —                   |
| Margin      | \$2,180     | \$10,000     | —                   |

**ETH drops to \$1,800**

_Alice's account_

$$
\mathtt{equity} = \$2{,}180 + 10 \times (\$1{,}800 - \$2{,}000) = \$180
$$

$$
\mathtt{MM} = 10 \times \$1{,}800 \times 5\% = \$900
$$

$$
\$180 < \$900 \;\Rightarrow\; \text{liquidatable (solvent)}
$$

_Close schedule_

$$
\mathtt{deficit} = \$900 - \$180 = \$720
$$

$$
\mathtt{closeSize} = \left\lceil \frac{\$720}{\$1{,}800 \times 5\%} \right\rceil = 8 \text{ ETH} \quad (\text{partial close; 2 ETH remain})
$$

_Bankruptcy price_

$$
\mathtt{bp} = \$1{,}800 - \frac{\$180}{10} = \$1{,}782
$$

Alice is solvent, so the close order's limit price is the BP.

_Execution_

Carol's bid at \$1,800 is above the \$1,782 limit, so the entire 8 ETH close fills at \$1,800. No ADL.

$$
\mathtt{AlicePnL} = 8 \times (\$1{,}800 - \$2{,}000) = -\$1{,}600
$$

_Liquidation fee_

$$
\mathtt{closedNotional} = 8 \times \$1{,}800 = \$14{,}400 \quad (\text{valued at the oracle price})
$$

$$
\mathtt{rawFee} = \$14{,}400 \times 0.1\% = \$14.40
$$

$$
\mathtt{remainingMargin} = \max(0,\; \$2{,}180 - \$1{,}600) = \$580
$$

$$
\mathtt{fee} = \min(\$14.40,\; \$580) = \$14.40
$$

**Final state**

|                | Position                 | Margin / balance                       |
| -------------- | ------------------------ | -------------------------------------- |
| Alice          | Long 2 ETH @ \$2,000     | \$2,180 − \$1,600 − \$14.40 = \$565.60 |
| Bob            | Short 10 ETH (untouched) | \$10,000                               |
| Carol          | Long 8 ETH @ \$1,800     | —                                      |
| Insurance fund | —                        | +\$14.40                               |

No ADL, no bad debt. Alice keeps 2 ETH and equity of \$565.60 − 2 × \$200 = \$165.60.

### Example 2 — Solvent; close partially absorbed by the book, remainder ADL'd

Same as Example 1, except Carol's bid is only **5 ETH** @ \$1,800.

_Execution_

- Book: 5 ETH fill Carol's bid at \$1,800.
- ADL: the remaining 3 ETH close against Bob — the most profitable short — at the bankruptcy price, \$1,782.

$$
\mathtt{AlicePnL} = 5 \times (\$1{,}800 - \$2{,}000) + 3 \times (\$1{,}782 - \$2{,}000) = -\$1{,}000 - \$654 = -\$1{,}654
$$

$$
\mathtt{BobPnL} = -3 \times (\$1{,}782 - \$2{,}000) = +\$654
$$

Closing 3 ETH at the oracle would have realized $3 \times (\$2{,}000 - \$1{,}800) = \$600$ for Bob; the extra \$54 is Alice's per-unit equity concession, $3 \times \$180 / 10 = \$54$.

_Liquidation fee_

Same closed notional as Example 1 (8 ETH valued at the \$1,800 oracle) → fee \$14.40.

**Final state**

|                | Position              | Margin / balance                       |
| -------------- | --------------------- | -------------------------------------- |
| Alice          | Long 2 ETH @ \$2,000  | \$2,180 − \$1,654 − \$14.40 = \$511.60 |
| Bob            | Short 7 ETH @ \$2,000 | \$10,000 + \$654 = \$10,654            |
| Carol          | Long 5 ETH @ \$1,800  | —                                      |
| Insurance fund | —                     | +\$14.40                               |

No bad debt.

### Example 3 — Solvent; book empty, whole close ADL'd

Same as Example 1, but the order book is empty.

_Execution_

The entire 8 ETH close is ADL'd against Bob at the bankruptcy price, \$1,782.

$$
\mathtt{AlicePnL} = 8 \times (\$1{,}782 - \$2{,}000) = -\$1{,}744
$$

$$
\mathtt{BobPnL} = -8 \times (\$1{,}782 - \$2{,}000) = +\$1{,}744
$$

_Liquidation fee_

\$14.40 as before.

**Final state**

|                | Position              | Margin / balance                       |
| -------------- | --------------------- | -------------------------------------- |
| Alice          | Long 2 ETH @ \$2,000  | \$2,180 − \$1,744 − \$14.40 = \$421.60 |
| Bob            | Short 2 ETH @ \$2,000 | \$10,000 + \$1,744 = \$11,744          |
| Insurance fund | —                     | +\$14.40                               |

No bad debt: Alice concedes $8 \times \$18 = \$144$ of her \$180 equity to Bob and keeps the rest.

### Example 4 — Insolvent; close fully absorbed by the book

**Setup**

|             | Alice       | Bob          | Carol                |
| ----------- | ----------- | ------------ | -------------------- |
| Position    | Long 10 ETH | Short 10 ETH | Bid 10 ETH @ \$1,700 |
| Entry price | \$2,000     | \$2,000      | —                    |
| Margin      | \$2,800     | \$10,000     | —                    |

**ETH drops to \$1,700**

_Alice's account_

$$
\mathtt{equity} = \$2{,}800 + 10 \times (\$1{,}700 - \$2{,}000) = -\$200
$$

$$
\mathtt{MM} = 10 \times \$1{,}700 \times 5\% = \$850
$$

Equity is negative — Alice is liquidatable and insolvent.

_Close schedule_

$$
\mathtt{deficit} = \$850 - (-\$200) = \$1{,}050 \geq \mathtt{MM} \;\Rightarrow\; \text{full close of all 10 ETH}
$$

_Bankruptcy price_

$$
\mathtt{bp} = \$1{,}700 - \frac{-\$200}{10} = \$1{,}720
$$

Alice is insolvent, so the close order's limit price is the oracle price (\$1,700), not the BP.

_Execution_

Carol's bid at \$1,700 fills the entire 10 ETH — at a price **lower than the BP**. No ADL.

$$
\mathtt{AlicePnL} = 10 \times (\$1{,}700 - \$2{,}000) = -\$3{,}000
$$

_Liquidation fee_

$$
\mathtt{remainingMargin} = \max(0,\; \$2{,}800 - \$3{,}000) = \$0 \;\Rightarrow\; \mathtt{fee} = \$0
$$

_Bad debt_

$$
\mathtt{badDebt} = |\min(0,\; \$2{,}800 - \$3{,}000)| = \$200
$$

Equivalently: each of the 10 ETH filled \$20 below the \$1,720 BP. Alice's margin is floored to zero and the insurance fund covers the \$200.

**Final state**

|                | Position                 | Margin / balance |
| -------------- | ------------------------ | ---------------- |
| Alice          | — (fully liquidated)     | \$0              |
| Bob            | Short 10 ETH (untouched) | \$10,000         |
| Carol          | Long 10 ETH @ \$1,700    | —                |
| Insurance fund | —                        | −\$200           |

### Example 5 — Insolvent; close partially absorbed by the book, remainder ADL'd

Same as Example 4, except Carol's bid is only **4 ETH** @ \$1,700.

_Execution_

- Book: 4 ETH fill at \$1,700 (below the \$1,720 BP).
- ADL: the remaining 6 ETH close against Bob at the BP, \$1,720.

$$
\mathtt{AlicePnL} = 4 \times (\$1{,}700 - \$2{,}000) + 6 \times (\$1{,}720 - \$2{,}000) = -\$1{,}200 - \$1{,}680 = -\$2{,}880
$$

$$
\mathtt{BobPnL} = -6 \times (\$1{,}720 - \$2{,}000) = +\$1{,}680
$$

Bob is force-closed \$20 above oracle on 6 ETH — he absorbs \$120 of Alice's insolvency that would otherwise become bad debt.

_Bad debt_

$$
\mathtt{margin} = \$2{,}800 - \$2{,}880 = -\$80 \;\Rightarrow\; \mathtt{badDebt} = \$80
$$

The \$80 equals the book-filled portion's shortfall from the BP: $4 \times (\$1{,}720 - \$1{,}700)$. The fee is \$0 (no remaining margin).

**Final state**

|                | Position              | Margin / balance              |
| -------------- | --------------------- | ----------------------------- |
| Alice          | — (fully liquidated)  | \$0                           |
| Bob            | Short 4 ETH @ \$2,000 | \$10,000 + \$1,680 = \$11,680 |
| Carol          | Long 4 ETH @ \$1,700  | —                             |
| Insurance fund | —                     | −\$80                         |

### Example 6 — Insolvent; book empty, whole close ADL'd

Same as Example 4, but the order book is empty.

_Execution_

All 10 ETH are ADL'd against Bob at the BP, \$1,720.

$$
\mathtt{AlicePnL} = 10 \times (\$1{,}720 - \$2{,}000) = -\$2{,}800
$$

$$
\mathtt{margin} = \$2{,}800 - \$2{,}800 = \$0
$$

Alice's equity is zeroed **by construction** — no bad debt, despite her insolvency. Bob absorbs the whole \$200 shortfall by buying back 10 ETH at \$20 above oracle:

$$
\mathtt{BobPnL} = -10 \times (\$1{,}720 - \$2{,}000) = +\$2{,}800
$$

(\$200 less than the \$3,000 he would realize closing at the oracle.) The fee is \$0.

**Final state**

|                | Position             | Margin / balance              |
| -------------- | -------------------- | ----------------------------- |
| Alice          | — (fully liquidated) | \$0                           |
| Bob            | — (fully ADL'd)      | \$10,000 + \$2,800 = \$12,800 |
| Insurance fund | —                    | unchanged                     |

### Example 7 — Two positions, solvent

Alice now holds two longs; Bob holds the exact opposite shorts. The order book is empty in this example and the next, so all closes go to ADL.

**Setup**

|             | Alice                     | Bob                       |
| ----------- | ------------------------- | ------------------------- |
| Positions   | Long 10 ETH, long 1 BTC   | Short 10 ETH, short 1 BTC |
| Entry price | ETH \$2,000; BTC \$50,000 | ETH \$2,000; BTC \$50,000 |
| Margin      | \$7,065                   | \$12,000                  |

**ETH drops to \$1,900; BTC drops to \$47,000**

_Alice's account_

$$
\mathtt{equity} = \$7{,}065 + 10 \times (-\$100) + 1 \times (-\$3{,}000) = \$3{,}065
$$

$$
\mathtt{MM} = 10 \times \$1{,}900 \times 5\% + 1 \times \$47{,}000 \times 5\% = \$950 + \$2{,}350 = \$3{,}300
$$

$$
\$3{,}065 < \$3{,}300 \;\Rightarrow\; \text{liquidatable (solvent)}
$$

_Close schedule_

$$
\mathtt{deficit} = \$3{,}300 - \$3{,}065 = \$235
$$

Positions are processed in descending order of MM contribution: BTC (\$2,350) before ETH (\$950).

$$
\mathtt{closeSize_{BTC}} = \left\lceil \frac{\$235}{\$47{,}000 \times 5\%} \right\rceil = 0.1 \text{ BTC}
$$

Closing 0.1 BTC removes $0.1 \times \$2{,}350 = \$235$ of MM — the deficit is fully covered, so the ETH position is not scheduled at all.

_Bankruptcy price (BTC)_

$$
\mathtt{bp} = \$47{,}000 - \frac{\$3{,}065}{1} = \$43{,}935
$$

The numerator is the **whole-account** equity — including the ETH position's unrealized PnL — divided by the BTC position's full size (1 BTC).

_Execution_

0.1 BTC is ADL'd against Bob at \$43,935.

$$
\mathtt{AlicePnL} = 0.1 \times (\$43{,}935 - \$50{,}000) = -\$606.50
$$

$$
\mathtt{BobPnL} = +\$606.50
$$

_Liquidation fee_

$$
\mathtt{fee} = 0.1 \times \$47{,}000 \times 0.1\% = \$4.70
$$

**Final state**

|                | Positions                   | Margin / balance                         |
| -------------- | --------------------------- | ---------------------------------------- |
| Alice          | Long 10 ETH, long 0.9 BTC   | \$7,065 − \$606.50 − \$4.70 = \$6,453.80 |
| Bob            | Short 10 ETH, short 0.9 BTC | \$12,000 + \$606.50 = \$12,606.50        |
| Insurance fund | —                           | +\$4.70                                  |

No bad debt. Alice's equity is \$6,453.80 − \$1,000 − 0.9 × \$3,000 = \$2,753.80.

### Example 8 — Two positions, insolvent

Same positions and margins as Example 7; prices fall further.

**ETH drops to \$1,800; BTC drops to \$44,000**

_Alice's account_

$$
\mathtt{equity} = \$7{,}065 + 10 \times (-\$200) + 1 \times (-\$6{,}000) = -\$935
$$

$$
\mathtt{MM} = 10 \times \$1{,}800 \times 5\% + 1 \times \$44{,}000 \times 5\% = \$900 + \$2{,}200 = \$3{,}100
$$

_Close schedule_

$$
\mathtt{deficit} = \$3{,}100 - (-\$935) = \$4{,}035 \geq \mathtt{MM} \;\Rightarrow\; \text{both positions fully closed, BTC first}
$$

_Entry 1: BTC_

$$
\mathtt{bp_{BTC}} = \$44{,}000 - \frac{-\$935}{1} = \$44{,}935
$$

The full 1 BTC is ADL'd against Bob at \$44,935:

$$
\mathtt{margin}: \$7{,}065 + 1 \times (\$44{,}935 - \$50{,}000) = \$2{,}000
$$

Alice's equity is now $\$2{,}000 + 10 \times (\$1{,}800 - \$2{,}000) = \$0$ — the first position's ADL absorbed the entire account shortfall.

_Entry 2: ETH_

The BP is recomputed from the account's **current** equity, which is now zero:

$$
\mathtt{bp_{ETH}} = \$1{,}800 - \frac{\$0}{10} = \$1{,}800 = \text{oracle price}
$$

All 10 ETH are ADL'd against Bob at \$1,800:

$$
\mathtt{margin}: \$2{,}000 + 10 \times (\$1{,}800 - \$2{,}000) = \$0
$$

_Liquidation fee and bad debt_

Remaining margin is \$0, so the fee is \$0; the margin is exactly zero, so there is **no bad debt**.

**Final state**

|                | Positions            | Margin / balance                        |
| -------------- | -------------------- | --------------------------------------- |
| Alice          | — (fully liquidated) | \$0                                     |
| Bob            | — (fully ADL'd)      | \$12,000 + \$5,065 + \$2,000 = \$19,065 |
| Insurance fund | —                    | unchanged                               |

Bob's two ADL fills realize $1 \times (\$50{,}000 - \$44{,}935) = \$5{,}065$ on BTC and $10 \times (\$2{,}000 - \$1{,}800) = \$2{,}000$ on ETH. Alice's \$935 shortfall is absorbed entirely by the BTC fill's above-oracle premium.

### Edge case — negative margin, positive equity

Every example above reaches the fee and bad-debt steps with margin equal to equity: a solvent account is only partially closed but its margin stays positive, and an insolvent account is fully closed, leaving no positions so that margin equals equity. Neither the §4 fee cap nor the §6 bad-debt check can tell margin and equity apart in those cases.

A cross-margined account can, however, reach liquidation with **negative margin but positive equity**. The cash margin balance goes negative — while the account stays solvent — when the trader extracts an open position's unrealised profit before it is realised: by withdrawing against it ([margin §3](1-margin.md)), or by realising a loss on a different position against it. The account is still solvent because that position's unrealised profit is part of equity. This is the case the **equity** caps exist for.

**Setup**

|             | Alice                |
| ----------- | -------------------- |
| Position    | Long 10 ETH          |
| Entry price | \$2,000              |
| Margin      | **−\$1,450**         |

Carol rests a 5-ETH bid at the \$2,200 oracle.

**ETH at \$2,200**

$$
\mathtt{equity} = -\$1{,}450 + 10 \times (\$2{,}200 - \$2{,}000) = \$550
$$

$$
\mathtt{MM} = 10 \times \$2{,}200 \times 5\% = \$1{,}100
$$

$$
\$0 < \$550 < \$1{,}100 \;\Rightarrow\; \text{liquidatable, but solvent}
$$

_Close schedule_

$$
\mathtt{deficit} = \$1{,}100 - \$550 = \$550
\qquad
\mathtt{closeSize} = \left\lceil \frac{\$550}{\$2{,}200 \times 5\%} \right\rceil = 5 \text{ ETH} \quad (\text{5 ETH remain})
$$

_Execution_

Alice is solvent, so the close order's limit price is the bankruptcy price $\mathtt{bp} = \$2{,}200 - \$550 / 10 = \$2{,}145$. Carol's bid at \$2,200 is above that limit, so the 5-ETH close fills on the book at \$2,200 (no ADL):

$$
\mathtt{AlicePnL} = 5 \times (\$2{,}200 - \$2{,}000) = +\$1{,}000 \;\Rightarrow\; \mathtt{margin} = -\$1{,}450 + \$1{,}000 = -\$450
$$

Closing at the oracle realises no concession, so equity is unchanged at \$550.

_Liquidation fee_

$$
\mathtt{rawFee} = 5 \times \$2{,}200 \times 0.1\% = \$11.00
$$

$$
\mathtt{equityAfterCloses} = -\$450 + 5 \times (\$2{,}200 - \$2{,}000) = \$550
\;\Rightarrow\;
\mathtt{fee} = \min(\$11.00,\; \$550) = \$11.00
$$

The margin-based cap would instead have used $\max(0,\; -\$450) = \$0$ and skipped the fee entirely.

_Bad debt_

$$
\mathtt{equity\ after\ settlement} = (-\$450 - \$11) + 5 \times \$200 = \$539 > 0 \;\Rightarrow\; \textbf{no bad debt}
$$

The margin-based check would instead have seen $\mathtt{margin} = -\$461 < 0$, paid \$461 from the insurance fund, and floored Alice's margin to zero — handing \$461 of insurance-fund value to a solvent account and inflating her equity by the same amount.

**Final state**

|                | Position             | Margin / balance                  |
| -------------- | -------------------- | --------------------------------- |
| Alice          | Long 5 ETH @ \$2,000 | −\$450 − \$11 = −\$461            |
| Carol          | Long 5 ETH @ \$2,200 | —                                 |
| Insurance fund | —                    | +\$11.00 (fee only; no bad debt)  |

Alice keeps 5 ETH and equity of −\$461 + 5 × \$200 = \$539, still solvent. Her margin stays negative — a valid cross-margin state, backed by the open position's unrealised profit, that resolves itself when she eventually closes the position and realises that profit into margin.
