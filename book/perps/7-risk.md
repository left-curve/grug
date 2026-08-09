# Risk Parameters

This chapter describes how to choose the risk parameters that govern the perpetual futures exchange — the global `Param` fields and per-pair `PairParam` fields defined in the perps contract. The goal is a systematic, reproducible calibration workflow that balances capital efficiency against tail-risk protection.

## 1. Margin ratios

The **initial margin ratio** (IMR) sets maximum leverage ($1 / \mathtt{imr}$). The **maintenance margin ratio** (MMR) sets the liquidation threshold. Both are per-pair.

The IMR also bounds closed-session index drift: while a market is closed, the index price can move at most $\pm \mathtt{imr}$ from the last regular-session oracle price, so a thin off-hours book cannot walk the mark far from the true price.

### 1.1 Volatility-based derivation

Start from the asset's historical daily return distribution:

1. Collect at least 1 year of daily log-returns.
2. Compute the 99.5th-percentile absolute daily return $r_{99.5}$.
3. Apply a **liquidation-delay factor** $k$ (typically 2–3) to account for the time between the price move and the liquidation execution:

   $$
   \mathtt{mmr} = r_{99.5} \times k
   $$

4. Set IMR as a multiple of MMR:

   $$
   \mathtt{imr} = \mathtt{mmr} \times m, \quad m \in [1.5,\; 4]
   $$

A higher $m$ gives more buffer between position entry and liquidation, reducing bad-debt risk at the cost of lower leverage.

### 1.2 Peer benchmarks

| Asset | Hyperliquid max leverage | Hyperliquid IMR | dYdX IMR |
| ----- | ------------------------ | --------------- | -------- |
| BTC   | 40×                      | 2.5 %           | 5 %      |
| ETH   | 25×                      | 4 %             | 5 %      |
| SOL   | 20×                      | 5 %             | 10 %     |
| HYPE  | 10×                      | 10 %            | —        |

### 1.3 Invariants

The following must hold for every pair:

$$
0 < \mathtt{mmr} < \mathtt{imr} \leq 1
$$

$$
\mathtt{liquidationFeeRate} \leq \mathtt{mmr} - \mathrm{takerFeeRate}
$$

The second constraint ensures a liquidated position can always cover the taker fee and liquidation fee from the maintenance margin cushion.

## 2. Fee rates

Three fee rates apply globally (not per-pair):

| Parameter              | Role                                                       |
| ---------------------- | ---------------------------------------------------------- |
| `maker_fee_rate`       | Charged on limit-order fills; revenue to the vault         |
| `taker_fee_rate`       | Charged on market / crossing fills; revenue to the vault   |
| `liquidation_fee_rate` | Charged on liquidation notional; revenue to insurance fund |

### 2.1 Sizing principles

- **Taker fee** should exceed the typical half-spread of the most liquid pair so the vault earns positive expected value on every fill against a taker.
- **Maker fee** can be zero, slightly positive, or negative (rebate). A zero maker fee attracts resting liquidity; a negative maker fee pays the maker on every fill. The absolute value of the maker fee rate must not exceed the taker fee rate, otherwise the exchange loses money on each trade.
- **Liquidation fee** must satisfy the invariant in [§1.3](#13-invariants). It should be large enough to fund the insurance pool but small enough that a liquidated user retains some margin when possible.

### 2.2 Industry benchmarks

| Exchange    | Maker  | Taker  |
| ----------- | ------ | ------ |
| Hyperliquid | 0.015% | 0.045% |
| dYdX        | 0.01%  | 0.05%  |
| GMX         | 0.05%  | 0.07%  |

## 3. Funding parameters

Funding anchors the perp price to the oracle. Two per-pair parameters and one global parameter control its behaviour (see [Funding](3-funding.md) for mechanics):

| Parameter              | Scope    | Calibration guidance                                                             |
| ---------------------- | -------- | -------------------------------------------------------------------------------- |
| `funding_period`       | Global   | 1–8 hours. Shorter periods track the premium more tightly but increase gas cost. |
| `max_abs_funding_rate` | Per-pair | See [§3.1](#31-max-funding-rate).                                                |
| `impact_size`          | Per-pair | See [§3.2](#32-impact-size).                                                     |

### 3.1 Max funding rate

The max daily funding rate limits how much a position can be charged per day. A useful rule of thumb:

$$
\mathtt{maxAbsFundingRate_{\mathrm{daily}}} \leq \frac{\mathtt{imr}}{T_{\mathrm{liq}}}
$$

where $T_{\mathrm{liq}}$ is the number of days it should take sustained max-rate funding to liquidate a fully leveraged position. For $T_{\mathrm{liq}} = 3$ days and $\mathtt{imr} = 5\%$:

$$
\mathtt{maxRate} = \frac{0.05}{3} \approx 1.67\% \text{ per day}
$$

### 3.2 Impact size

The `impact_size` determines how deep the order book is walked to compute the premium. Set it to a representative trade size — large enough that the premium reflects real depth, small enough that thin books don't produce zero premiums too often. A good starting point is 1–5% of the target max OI.

## 4. Capacity parameters

### 4.1 Max open interest

The maximum OI per side caps the exchange's aggregate exposure:

$$
\mathtt{maxAbsOI} \leq \frac{\mathtt{vaultEquity} \times w_{\mathrm{pair}}}{\mathtt{mmr} \times \mathtt{tailLossFactor}}
$$

where $w_{\mathrm{pair}}$ is the pair's weight fraction and $\mathtt{tailLossFactor}$ (2–5) is a safety multiplier reflecting how many times maintenance margin the vault could lose in a tail event.

Start conservatively — it is easy to raise OI caps but dangerous to lower them (existing positions above the cap cannot be force-closed).

### 4.2 Min order size

Prevents dust orders. Set to a notional value that covers at least 2× the gas cost of processing the order. Typical values: \$10–\$100.

### 4.3 Tick size

The minimum price increment. Too small increases book fragmentation; too large creates implicit spread. Rule of thumb:

$$
\mathtt{tickSize} \approx \mathtt{oraclePrice} \times 10^{-4} \text{ to } 10^{-3}
$$

For BTC at \$60,000: tick sizes of \$1–\$10 are reasonable.

## 5. Vault parameters

The vault's market-making policy is controlled by three per-pair parameters and two global parameters (see [Vault](5-vault.md) for mechanics):

### 5.1 Half-spread

The half-spread should be calibrated to short-term intraday volatility so the vault earns a positive edge:

$$
\mathtt{halfSpread} \geq \sigma_{\mathtt{intraday}} + \mathtt{makerFeeRate}
$$

where $\sigma_{intraday}$ is the standard deviation of intra-block price changes. A larger spread protects against adverse selection but reduces fill probability.

### 5.2 Max quote size

Caps the vault's resting order size per side per pair. Should be consistent with `max_abs_oi` — the vault should not be able to accumulate more exposure than the system can handle:

$$
\mathtt{maxQuoteSize} \leq \mathtt{maxAbsOI} \times f, \quad f \in [0.3,\; 0.8]
$$

### 5.3 Liquidity weight

Determines what fraction of total vault margin is allocated to each pair. Higher-volume, lower-risk pairs should receive higher weights. The sum of all weights equals `vault_total_weight`.

### 5.4 Cooldown period

Prevents LPs from front-running known losses. Should exceed the funding period and be long enough that vault positions cannot be manipulated by short-term deposit/withdraw cycles. Typical values: 7–14 days.

## 6. Operational limits

| Parameter         | Calibration guidance                                                                                           |
| ----------------- | -------------------------------------------------------------------------------------------------------------- |
| `max_unlocks`     | Number of concurrent withdrawal requests per user. 5–10 is typical; prevents griefing with many small unlocks. |
| `max_open_orders` | Maximum resting limit orders per user across all pairs. 50–200; prevents order-book spam.                      |

## 7. Calibration workflow

The following checklist produces a complete parameter set from scratch:

1. **Collect data** — Gather ≥ 1 year of daily and hourly OHLCV data for each asset.

2. **Compute volatility** — For each asset, compute $r_{99.5}$ (daily 99.5th percentile absolute return) and $\sigma_{intraday}$ (hourly return standard deviation).

3. **Set margin ratios** — Derive MMR from $r_{99.5}$ ([§1.1](#11-volatility-based-derivation)), then IMR as a multiple of MMR. Cross-check against peer benchmarks ([§1.2](#12-peer-benchmarks)).

4. **Set fees** — Choose maker/taker/liquidation fee rates satisfying [§2.1](#21-sizing-principles) and the invariant in [§1.3](#13-invariants).

5. **Set funding** — Pick `funding_period`, derive `max_abs_funding_rate` ([§3.1](#31-max-funding-rate)), and calibrate `impact_size` ([§3.2](#32-impact-size)).

6. **Size exposure** — Set `max_abs_oi` from vault equity and tail-risk tolerance ([§4.1](#41-max-open-interest)).

7. **Set order constraints** — Choose `min_order_size` and `tick_size` ([§4.2](#42-min-order-size), [§4.3](#43-tick-size)).

8. **Configure vault** — Set `vault_half_spread`, `vault_max_quote_size`, and `vault_liquidity_weight` per pair ([§5](#5-vault-parameters)), and `vault_cooldown_period` globally.

9. **Backtest** — Replay historical price data through the parameter set. Verify:
   - Liquidations occur before bad debt in > 99% of cases.
   - Vault PnL is positive over the test period.
   - Funding rates do not hit the clamp for more than 5% of periods.

10. **Deploy conservatively** — Launch with the conservative profile (lower leverage, higher fees, lower OI caps). Tighten parameters toward the aggressive profile as the system proves stable and liquidity deepens.
