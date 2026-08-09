# Activity projection — design

The foundational projection of the archive. For every block it
produces three Postgres tables that together answer, for any account address,
**"what did it do and what touched it"**:

- `transactions` — one row per executed unit (a transaction *or* a cronjob);
- `events` — the **merged event log + participation index**: one row per
  *(event × participant address)*, plus an empty-address row for kept events
  with no participant;
- `event_data` — the event payload (`zstd(borsh(event))`), split out and kept
  only for the configured priority types.

It is a **single projection**: one `process(block)` flattens the block's
events once and writes all three tables in one commit (one `Ctx`, one cursor).

Only events whose commitment status is **`Committed`** are indexed. A reverted
or failed event (a transfer inside a tx that later failed, say) never took
effect on-chain, so surfacing it in the feeds would show activity that did not
happen — the same rule the in-process indexer applies when it reads transfers
(`Committed` only). The failed unit itself stays visible: its `transactions`
row is written regardless, with `success = false` (that is deliberate — see
§ Identity on failed txs); only its side-effect events are dropped. This rule
is fixed, not a config knob, and like every write-time filter it is **not
retroactive** (§ Configuration).

The commitment check alone is sufficient — a failed-but-**handled** submessage
(reply on error) is covered by construction: its state changes are reverted
even though the tx continues, and the flattener downgrades its whole subtree
to `Failed` regardless of the enclosing group's status
(`SubEventStatus::Handled` in `dango/core/types/src/events/flatten.rs`), while
the reply that handled it keeps `Committed` — its effects are real. So within
`Committed` the event status is `Ok` by construction, and no separate
`event_status` filter is needed.

## Queries it must serve

Eight feeds, all **newest-first** (`block_height DESC`, then in-block position),
bounded by `LIMIT N`, and **keyset-paginated** (cursor on the ordering tuple,
never `OFFSET`):

| # | Feed | Filter |
|---|------|--------|
| 1 | transactions involving X | `address = X` — sender **or** party |
| 2 | events by type | `event_type = T` |
| 3 | contract events by contract | `contract = C` |
| 4 | contract events by contract + name | `contract = C AND name = ANY([…])` |
| 5 | events involving X | `address = X` |
| 6 | events involving X + type | `address = X AND event_type = T` |
| 7 | contract events of C involving X | `address = X AND contract = C` |
| 8 | contract events of C involving X + name | `address = X AND contract = C AND name = ANY([…])` |

Two properties of the filters shape the schema:

- **Q1 spans two sources.** "Involving X" means X is the unit **sender** *or* a
  **party** to one of its events. The sender lives in `transactions`, the
  participation in `events`; Q1 merges the two (see Access paths). The two are
  genuinely different sets — the sender is **not** auto-added as a participant
  (only `Transfer` / `ContractEvent` parties are; `Authenticate` is
  blacklisted) — so an optional `role` filter narrows to one side (`SENDER`
  reads only `transactions.sender`, `PARTICIPANT` only the `events` rows);
  omitted ⇒ the union. Cron units (no sender, but X can be a party) are
  **included by default**; an optional `kind` filter narrows to transactions or
  cronjobs only (and `SENDER` + cron-only matches nothing).
- **`contract_event_name` is always paired with `contract`** (never queried on
  its own) and may be a **single value or a list**. It is
  **low-cardinality** — the busiest contract (perps) has ~15 distinct names — but
  low cardinality does not make a *filter* cheap: paired with a hot contract it
  needs its own `(contract, name)` **seek** index to stay bounded (see Indexes).

## Why one merged `events` table (not events + involvement)

An earlier design had two tables: a narrow `events` (one row per event) and an
`involvement` fan-out (one row per participant) carrying the event's filter
attributes *denormalized* so address+attribute queries (queries 7/8) stay a single
seek. But once the attributes are denormalized, the two tables hold **nearly
identical columns**, differing only by `address`. Merging them — PK extended
with `address`, one row per (event × participant) — removes that duplication.

This is sound because of two measured facts (mainnet, last 100 blocks, the real
flatten + `extract_addresses` + system-contract blacklist):

- **Most events are address-less system noise** that the **event-type
  blacklist** (§ Configuration) drops anyway: `guest` 47%, `withhold` /
  `authenticate` / `finalize` ~11% each, leaving `contract_event` at ~6%. After
  the default blacklist the surviving stream is ~**81% smaller**, and what
  remains is mostly the meaningful, address-bearing events.
- **Fan-out is tiny: a contract event carries ~1 non-system address** (the
  emitting contract is blacklisted as a participant, leaving the user). So one
  event ≈ one row — the "K rows per event" cost is ~1×.

Trade-offs the merge accepts (and why they're fine here):

- A kept event with **no** participant (e.g. an `execute`) still needs a row so
  the attribute feeds (queries 2/3/4) don't lose it. It gets **one row keyed by
  the empty byte string** — a real address is always 20 bytes, so `''` can never
  collide, and (unlike `NULL`) an empty value is valid in a primary key.
- The address-less attribute feeds (queries 2/3/4) must `DISTINCT ON` the event
  position to collapse an event's K participant rows. At K≈1 this is a **no-op**;
  it stays correct for K>1 (multi-recipient transfers). The `address` column is
  the **last** index column so those K rows are adjacent and the dedup is local.

Net effect, per 100 blocks (measured, default blacklist): the merge saves ~**24%**
of event/involvement rows versus the two-table model (and up to 50% if the feed
is restricted to address-bearing types) — on top of the ~81% the blacklist
already removes. The cost is the `DISTINCT ON` and a deliberate empty-address
marker.

## Backend: Postgres — and when ClickHouse

All three tables are on **Postgres** for V1: fixed access patterns on **real
columns** (`event_type`, `contract`, `contract_event_name`, `address`) — ordinary
composite B-tree territory with keyset pagination, native joins, and an
exactly-once cursor in the same transaction as the data. One database keeps V1
simple.

This is a large dataset, but the **event-type blacklist** is the dominant size
lever: the raw stream is ~112 flattened events/block, of which ~81% is dropped
noise, leaving ~20/block of meaningful, mostly address-bearing events. Postgres
serves the *queries* fine (every one is a bounded keyset scan on a good index),
so the cost center is **storage and write throughput**, addressed by: the
blacklist, the payload split (below), the involvement blacklist (below), and
offline backfill. (`block_height`-range partitioning is **not** implemented — a
deliberate deferral; see the write-side caveat below for the rationale and the
recovery path if a measured trigger ever demands it.)

A write-side caveat: `activity_events`' indexes are address- / contract-prefixed
(a near-random prefix), so a sustained insert stream scatters across the key
space and splits B-tree pages — unlike `transactions` / `event_data`, whose
`block_height`-led keys append at the right. The lever is the **offline
backfill**: bulk-load with the secondary indexes dropped, then build them on the
populated table (a packed build, not incremental splits). The migrations set a
`fillfactor` of 85 on those indexes (Postgres only) so that build — and later
right-extension — leaves slack; for a live, index-present load the gain is
smaller. `fillfactor` and the offline backfill are the implemented write levers.

`block_height`-range **partitioning is deliberately not implemented.** It would
help *writes* (insert locality) and *retention* (drop an old range as a whole
partition, instantly), but **not** the reads — every feed seeks on `address` /
`contract` / `event_type`, never a `block_height` range, so there is no partition
pruning (a feed would `MergeAppend` across all partitions). And it is **not a
cheap retrofit**: partitioning a populated table means a rebuild (a new
partitioned table + a copy), not an in-place `ALTER`. So the levers above carry
V1, and if a *measured* trigger ever demands more, the response is, in order:
(1) the **ClickHouse** escape hatch below (committer-ready — the real scale-out);
or (2) for a partitioned Postgres layout specifically (e.g. range-drop retention),
a one-time rebuild. The rebuild is operationally cheap in the planned
**two-indexer + load-balancer** topology: each indexer owns its own raw store and
Postgres, so it is a rolling per-projection re-backfill (bump the projection `id`,
re-create the table partitioned, catch up from the local raw store) on one indexer
while the other serves the load balancer — no shared-state migration, no
downtime.

**ClickHouse is the documented escape hatch.** If volume outgrows Postgres, the
`events` read-path moves to ClickHouse: the filter columns are low-cardinality
and compress ~an order of magnitude better columnar, alternate sort orders are
cheap (projections), and the committer already supports a ClickHouse side **in
the same commit** (flushed + acked before the PG transaction, idempotent by a
per-unit dedup token) — so it is a per-table migration, not a rearchitecture. We
stay Postgres-only until a measured trigger says otherwise.

## Identity: the position, not the hash

The canonical id of a unit is its **position** `(block_height, idx, kind)`; of
an event, `(block_height, category, category_index, event_index)`; of a row in
`events`, that event position **plus `address`**. The transaction **hash is a
non-unique, indexed attribute** — never a key.

The hash is not globally unique. Uniqueness would have to lean on the nonce,
and it doesn't:

- **Failed txs don't consume their nonce.** In `process_tx`
  (`dango/core/app/src/app.rs`), a tx that fails `withhold_fee` (sender can't
  cover the max fee) or `authenticate` returns a failed outcome while the state
  buffer holding the nonce write into `SEEN_NONCES` is **dropped, not
  committed** — yet the tx is still recorded in the block
  (`block_outcome.tx_outcomes[i]`, aligned by index with `block.txs[i]`). So
  identical bytes can be re-included validly in a **later** block.
- **Some senders have no nonce.** A smart contract can be a tx `sender` through
  its own `authenticate` entrypoint; those txs have no nonce at all.

Same bytes ⇒ same `Hash256`, in two different blocks. Within one block it can't
repeat (the mempool dedups by hash). So the position is unique by construction
and the hash is just an indexed column — a lookup by hash may legitimately
return more than one row.

## Tables

### `transactions` — one row per executed unit

| column         | type            | notes                                   |
|----------------|-----------------|-----------------------------------------|
| `block_height` | `BIGINT`        |                                         |
| `idx`          | `INT`           | position within block, per kind         |
| `kind`         | `SMALLINT`      | the unit's `FlatCategory`: 0 = cron, 1 = tx |
| `hash`         | `BYTEA` (32)    | content hash; indexed, **not unique**; NULL for cron |
| `sender`       | `BYTEA` (20)    | account **or contract**; NULL for cron  |
| `success`      | `BOOL`          | `tx_outcome.result.is_ok()`             |
| `timestamp`    | `BIGINT`        | block time, unix nanoseconds            |

Gas (`gas_limit` / `gas_used`) is **not** a column: it lives in the unit's
outcome, recovered through the on-demand `outcome` field (`TxOutcome` /
`CronOutcome`) rather than duplicated into the summary row.

Primary key `(block_height, idx, kind)`. Indexes: `(sender, block_height, idx)`
partial `WHERE sender IS NOT NULL` — the **sender side of query 1** (and a
strict-sender feed), recency pagination via a backward scan, excludes cron;
`(hash)` partial `WHERE hash IS NOT NULL` — the **`transaction(hash)` point
lookup** and the detail view.

No payload column: a unit's messages / credential / error are **hydrated from
the raw block** on the detail view (one block load — see below).

### `events` — merged event log + participation index

One row per **(event × participant address)**. Two row kinds, both keyed by
`(address, block_height, category, category_index, event_index)` with
`event_index >= 0`:

| column                | type         | notes                                   |
|-----------------------|--------------|-----------------------------------------|
| `address`             | `BYTEA`      | 20-byte participant; **empty** = address-less event |
| `block_height`        | `BIGINT`     |                                         |
| `category`            | `SMALLINT`   | the unit's `kind` (`FlatCategory`): 0 = cron, 1 = tx |
| `category_index`      | `INT`        | the tx/cron index (= the unit's `idx`)  |
| `event_index`         | `INT`        | event position within the unit (0-based) |
| `event_type`          | `SMALLINT`   | `FlatEvent` discriminant; **NOT NULL**  |
| `contract`            | `BYTEA` (20) | emitting contract; **set only for contract events**, NULL otherwise |
| `contract_event_name` | `TEXT`       | the contract-event `ty` (`order_filled`, …); set only for contract events (paired with `contract`), NULL otherwise |

- **participation** — `address` = a 20-byte party. K rows for an event with K
  participants (K ≈ 1).
- **address-less event** — `address` = the **empty byte string**. One per kept
  event with no (non-blacklisted) participant, so the attribute feeds never lose
  it. (A real address is always 20 bytes, so `''` never collides; `NULL` would be
  illegal in a PK.)

`contract` and `contract_event_name` are **coupled**: both are set exactly when
the event is a `ContractEvent` (the contract feeds always filter them together),
so `contract IS NOT NULL` means precisely "a contract event".

The **sender side** of "involving X" is **not** a row here (there are no
sentinel rows): it is read from `transactions.sender` and merged with this
participation side at query time (query 1).

The canonical *event* is the position `(block_height, category, category_index,
event_index)`; the unit `(block_height, category, category_index)` joins
`transactions(block_height, kind, idx)`. Distinct events are recovered with
`DISTINCT ON` the position (a no-op at K = 1).

**Indexes** (five). All carry the event-position `(block_height, category,
category_index, event_index)`, so each is a backward scan ordered newest-first
(keyset). On the two **attribute feeds** (by type, by contract) — the ones that
`DISTINCT ON` the position — `address` **trails the position** as the
tiebreaker, so an event's participant rows are adjacent and the resolver's
`ORDER BY <position> DESC, address DESC` is the backward scan verbatim: Index
Scan → Unique → Limit, **no sort node** (verified via `EXPLAIN`; with `address`
ASC — or with `contract_event_name` wedged before `address` — the planner adds
an Incremental Sort). `contract_event_name` therefore sits at the very **tail**
of the un-filtered contract index, where it costs the ordering nothing — but the
tail is useless for *filtering*: the position columns separate it from
`contract`, so a `name =` predicate there is checked per row across the
contract's whole slice, not sought. On a hot contract that slice is tens of
millions of rows, so the name-filtered feed gets a **second** contract index
leading with `(contract, contract_event_name)`.

- `idx_…_addr_type` — `(address, event_type, …)` — **query 6** ("type-T events
  involving X" as a single seek). Plain index (`event_type` is NOT NULL).
- `idx_…_addr_contract` — `(address, contract, …, contract_event_name)` —
  **queries 7/8** (contract events of C involving X, optionally a name list);
  address-led, no `DISTINCT ON`, so `contract_event_name` trails as the in-index
  filter — bounded here because the **address** anchor already limits the scan
  to one participant's events. Partial `WHERE contract IS NOT NULL`.
- `idx_…_type` — `(event_type, …, address)` — **query 2** (events by type).
  Plain.
- `idx_…_contract` — `(contract, …, address, contract_event_name)` —
  **query 3** (contract events of C, *un-filtered*); `address` trails the
  position (tiebreaker), `contract_event_name` at the tail (costs the ordering
  nothing — but not a filter anchor, see above). Partial.
- `idx_…_contract_name` — `(contract, contract_event_name, …, address)` —
  **query 4** (contract events of C narrowed by a name list). Leading with
  `(contract, name)` makes the name a **seek**: the query jumps to the
  (contract, name) group, whose rows are already in position order, so it stays
  a no-sort backward scan and a zero-match page returns at once instead of
  walking the contract's whole slice. A companion to `idx_…_contract`, not a
  replacement — neither column order serves both the filtered and the
  un-filtered feed without a sort. Partial `WHERE contract IS NOT NULL`.

The PK `(address, …)` itself serves **query 5** (events involving X) and, via
`DISTINCT ON` the unit merged with `transactions.sender`, **query 1**.

### `event_data` — the event payload, split out

| column           | type       | notes                |
|------------------|------------|----------------------|
| `block_height`   | `BIGINT`   |                      |
| `category`       | `SMALLINT` |                      |
| `category_index` | `INT`      |                      |
| `event_index`    | `INT`      |                      |
| `data`           | `BYTEA`    | `zstd(borsh(event))` |

Primary key `(block_height, category, category_index, event_index)` — the event
position. A row exists **only** for priority-type events; a missing row means
"hydrate the payload from the raw block". No secondary indexes — reached only by
point lookup on the detail view. Splitting the payload out keeps the hot,
frequently-scanned `events` rows fixed-width and index-dense (the cold,
variable-size blob lives apart): narrower rows, smaller indexes, better cache.

## Access paths

All newest-first, keyset-paginated (no `OFFSET`). The keyset is a row-comparison
on the position — `(block_height, category, category_index, event_index) <
$cursor`, all DESC — which the index serves directly.

> The SQL below drops the table prefix for brevity. The physical tables the
> resolver SQL targets are **`activity_transactions`**, **`activity_events`**,
> **`activity_event_data`** (the `activity_` prefix keeps the names unique in the
> app's shared schema / migration history) — never the bare `transactions` /
> `events`, which compile fine in the hand-written SQL but fail at runtime
> against Postgres. The `feeds_execute_against_postgres` integration test runs
> every feed against a real Postgres so that can't regress unnoticed.

**Q1 — transactions involving X** (sender **or** party). Two sources, merged:
the **involved** side is `DISTINCT ON` the unit over X's participation rows; the
**sender** side is a direct filter on `transactions`. Each side takes N, they are
unioned, deduped (a unit where X is both sender and party is the common case),
and the top N kept. Cron units come only from the involved side (cron has no
sender); `AND category = 1` on that side narrows to transactions only.

```sql
-- involved side (events): the distinct units X is a party to. Plain DISTINCT
-- over the 3-column unit position; the PK (address, block_height, category,
-- category_index, …) serves it as a backward scan.
(SELECT DISTINCT block_height, category, category_index
 FROM events
 WHERE address = $X        -- [AND category = $kind]  [AND (unit) < $cursor]
 ORDER BY block_height DESC, category DESC, category_index DESC
 LIMIT $N)
UNION                      -- arms MUST be parenthesised: Postgres rejects
                           -- `SELECT … LIMIT n UNION …` without parens
-- sender side (transactions): every row is kind = Tx (cron has no sender), so
-- `kind` is dropped from the ORDER BY and the (sender, block_height, idx) index
-- serves it with no sort. The keyset still compares the full unit position.
(SELECT block_height, kind AS category, idx AS category_index
 FROM transactions
 WHERE sender = $X         -- [AND (block_height, kind, idx) < $cursor]
 ORDER BY block_height DESC, idx DESC
 LIMIT $N)
-- ... then join `transactions` on (block_height, kind, idx), ORDER BY the unit
-- position DESC, LIMIT $N. Next page: AND unit < $cursor on both sides.
```

**Q5–Q8 — events involving X**, optionally + type / contract / (contract+name) —
a single seek on the PK or an `(address, …)` index, no `DISTINCT` (X appears once
per event):

```sql
SELECT * FROM events
WHERE address = $X
  -- AND event_type = $T                      (Q6, via idx_…_addr_type)
  -- AND contract = $C                         (Q7, via idx_…_addr_contract)
  -- AND contract = $C
  --     AND contract_event_name IN ($names…)   (Q8, name filtered in-index)
ORDER BY block_height DESC, category DESC, category_index DESC, event_index DESC
LIMIT $N;
```

**Q2–Q4 — events by type / contract / (contract+name)**, no address — on the
`(<attr>, …, address, …)` indexes, with `DISTINCT ON` the position to collapse
an event's participant rows (a no-op at K = 1). The tiebreaker is `address`
**DESC** — same direction as the position — so the whole `ORDER BY` is the
backward index scan verbatim (Index Scan → Unique → Limit, no sort node);
`address` ASC would force an Incremental Sort:

```sql
SELECT DISTINCT ON (block_height, category, category_index, event_index) *
FROM events
WHERE contract = $C                             -- or event_type = $T (Q2)
  -- AND contract_event_name IN ($names…)        (Q4)
ORDER BY block_height DESC, category DESC, category_index DESC, event_index DESC, address DESC
LIMIT $N;
```

The **name list** (Q4/Q8) is a plain `contract_event_name IN ($names…)`, never
a per-name UNION. **Q8** (address-anchored) keeps the name as an in-index filter
that stays off the heap and is already bounded by the participant's own events.
**Q4** (contract-anchored) is the trap: on the un-filtered contract index the
name sits past the position columns, so `IN (…)` there is a per-row check over
the contract's whole slice — a low- or zero-selectivity name (say one only a
*different* contract emits) walks all of it, tens of millions of rows on a hot
contract, seconds-to-minutes for an often-empty page, and a client disconnect
does not cancel it. Q4 is therefore served by `idx_…_contract_name`, whose
leading `(contract, contract_event_name)` turns `IN (…)` into a per-name seek:
the match-set is already in position order, the work is bounded by
*(contract, name)* activity, and a zero-match page returns at once
(`EXPLAIN`-verified: `Limit` cost ~21k → ~34).

### Index summary — every feed is anchored by a selective seek

| Query | Index | Cost |
|---|---|---|
| Q1 tx involving X | `events` PK (involved) ∪ `transactions (sender, …)` → merge | 2 seeks + merge ~2N |
| Q2 events by type T | `events (event_type, …, address)` + `DISTINCT ON` | seek + ~K·N |
| Q3 contract events of C | `events (contract, …, address)` + `DISTINCT ON` | seek + ~K·N |
| Q4 + name list | `events (contract, name, …)` seek | seek + N (small) |
| Q5 events involving X | `events` PK `(address, …)` | seek + N |
| Q6 + type T | `events (address, event_type, …)` | seek + N |
| Q7 contract C involving X | `events (address, contract, …)` | seek + N (small) |
| Q8 + name list | same index, `name = ANY()` | seek + N (small) |
| payload (detail) | `event_data` PK, else raw block | point lookup |

No feed scans the whole table — each is anchored on a selective equality
(sender / address / contract / type) then a `block_height`-ordered scan, so the
cost is bounded by **one entity's activity**, not the table total. **The query
layer must write the attribute feeds as `DISTINCT ON (<position>) … ORDER BY
<position>, address … LIMIT n`** so Postgres streams + short-circuits; a
non-matching `ORDER BY` materializes the whole match set instead.

## What is stored vs hydrated from the raw block

The raw block is always reachable through `source.get(height)` — in V1 a single
page-cached read of the node's on-disk cache file, the source of truth for
payloads. The projection stores the **queryable index**, not a second copy.

- **Stored in PG:** the three tables above; the event payload (`event_data`)
  only for the configured priority types.
- **Hydrated from the raw block on demand:** a transaction's full
  payload/messages/error (the tx detail view), and the payload of any event with
  no `event_data` row (non-priority types).

The split follows the numbers. A **tx detail** is block-local: one `source.get`
returns the tx and *all* its events — cheap. An **event feed**, filtered and
ordered across blocks, hits ~one matching event per block, so a page would
otherwise mean ~one cold-block read per row — wasteful and SLA-breaking on
historical (cold) data. So the priority payload lives in `event_data` and the
feed never hydrates; only the rare detail of a non-priority event pays one
block load.

## Per-block processing

`process(&mut ctx, block)` flattens the block's events once (tx units via
`flatten_tx_events`, cron units via `flatten_commitment_status` on a fresh
`EventId`), then for each unit:

For each `tx_outcome[i]` aligned with `block.txs[i] = (tx, hash)`:

1. write one `transactions` row (`kind = tx`, `sender`, gas, success, `hash`,
   `timestamp`) — this row is also the **sender side** of query 1, read back
   from `transactions.sender`;
2. for each flattened event of the tx:
   - **skip it entirely** unless its `commitment_status` is `Committed` —
     reverted / failed events are not activity (the unit row above already
     records the failure);
   - **skip it entirely** if `event_type_filter` rejects its type;
   - write its `event_data` row if `event_data_filter` allows its type;
   - extract participants if `involvement_filter` allows its type (the chain's
     `Extractable::extract_addresses`, **minus the blacklist**); write one
     `events` row per participant, each carrying `event_type` / `contract` /
     `contract_event_name`. If there is **no** participant, write a single
     empty-address row so the event still appears in the attribute feeds.

For each `cron_outcome` (same block): the same, with `kind = cron` and
`sender = NULL`. A cron has no sender, so it never contributes to the sender side
of query 1 — only to the participation side (which is why cron units reach query
1 through "involved", and the optional `kind` filter can drop them).

Why two separate scopes — `event_type_filter` (keep the event at all) vs
`involvement_filter` (extract participants): the first drops the address-less
system *noise* (guest/withhold/authenticate/finalize) that nobody queries;
`involvement_filter` decides, among the **kept** events, which ones fan out by
participant (the rest get one empty-address row). The contract feeds read
`events.contract`, a different axis from participation — so blacklisting a
contract from *participation* never hides its emitted contract events.

## Configuration

The three event-type filters are each a `WhiteOrBlackList<EventType>` — config
picks `{ whitelist = [...] }` or `{ blacklist = [...] }`; the table shows the
default polarity.

| key                    | default                          | effect                                                          |
|------------------------|----------------------------------|-----------------------------------------------------------------|
| `event_type_filter`    | blacklist `[guest, withhold, authenticate, finalize, backrun, reply]` | which event types are kept (others: no `events` row, no payload) — the default drops the address-less system noise (~81% of the raw stream) |
| `event_data_filter`    | whitelist `[transfer, contract_event]` | which event types' payload is stored in `event_data`; others lazy-load from the raw block |
| `involvement_filter`   | whitelist `[transfer, contract_event]` | among kept events, which fan out by participant (others get one empty-address row) |
| `involvement_blacklist`| **empty**; the CLI fills it from the node's `app_config` | addresses excluded from participation at write time — the deployment's system contracts (perps, taxman, oracle, bank, dex, account_factory, gateway, warp, hyperlane.*) |

Caveat — the default `involvement_blacklist` is **empty**, but the CLI fills it
at startup by querying the node's `app_config` and extracting every address in
it (the system contracts) via `Extractable`, merged with any from config. This
matters because
`CheckedContractEvent::extract_addresses` inserts the
**emitting contract** itself as a participant. So until the system-contract
blacklist is wired, every contract event fans out to the contract *and* the user
(fan-out K ≥ 2, not the ~1 assumed in § "Why one merged `events` table"), and a
contract appears as a *party* to its own events — i.e. `eventsInvolving(<system
contract>)` returns everything it emitted. The contract feeds are unaffected
(they read `events.contract`, a different axis). Because the blacklist is applied
at write time, **populate it before the first full backfill** — adding entries
later only stops *new* rows; the already-written contract-as-participant rows
need a targeted `DELETE` (or a re-backfill).

Maintenance note: all applied at **write time**, so changes are not retroactive.
*Loosening* `event_type_filter`, adding to `event_data_filter`, broadening
`involvement_filter`, or *removing* a `involvement_blacklist` entry requires a
re-backfill (drop tables + cursor, bump the projection id, re-run) to populate
the now-wanted rows; *widening* the blacklist leaves already-written rows until a
targeted `DELETE`. Prefer settling them before the first full backfill.

## Commit / idempotency

Every key is the deterministic position (+ `address`), and every write is an
upsert (`INSERT … ON CONFLICT DO NOTHING` on the primary keys), so a post-crash
replay of a block is a no-op. All three tables plus the cursor commit in **one
Postgres transaction** — the archive's PG-side exactly-once. No
ClickHouse path in V1.

## Crate layout

`projection/src/activity.rs` (the `Projection` impl + `ActivityConfig`,
`id = "activity"`, `min_height` left at the default genesis floor) ·
`projection/src/activity/`: `event_type.rs` (the `EventType`
discriminant stored in `event_type`, plus the per-event contract / name
taxonomy) · `entity/` (sea-orm types for the three tables) · `idens.rs`
(migration identifiers) · `migrations/` (one file per table) + `migrations.rs`
(assembles them, names prefixed `…activity…` for the shared `seaql_migrations`
history — mirrors `app/src/committer/`). The read surface (below) lives in
`http/` (`feeds.rs` DB queries · `services/` actix handlers + scopes, one module
per resource: `transaction.rs`, `events.rs` · `hydrate.rs` eager payload
hydration · `types.rs` objects + enums · `pagination.rs` keyset · `error.rs`).
Adds `dango-primitives` to the
`projection` crate (for `FlatCategory` / `EventId` / `Extractable` /
`flatten_tx_events` / `flatten_commitment_status`), plus `zstd`/`borsh` for the
`event_data` payloads.

Flattening note: tx units use the canonical `flatten_tx_events`; cron units use
`flatten_commitment_status` on a fresh per-unit `EventId`. Both number
`event_index` from 0 within the unit, contiguously: most leaves self-advance the
index (`next_id.event_index += 1`), the few that don't (`Configure` / `Upgrade`
/ `Upload`) are re-synced by their parent's `increment_idx` after each child, so
a unit's commitment groups stay contiguous and never collide. The invariant the
projection leans on is that the `event_index` values are a dense `0..n` matching
the flattened Vec position — the write path stores `id.event_index`, the read
path (`hydrate_events`'s non-priority fallback) finds an event **by that value**,
and a `debug_assert` in `push_events` pins the equality so a flatten change
can't silently desync them. Encoding note: `category` / `kind` store the canonical `FlatCategory`
discriminant (cron = 0, tx = 1) directly; the integer value is immaterial to the
queries as long as `events.category` and `transactions.kind` agree on it for the
join.

## Read surface

The eight access paths are served by `#[get]`-routed handlers in
`http/services/` (one module per resource), over the database feeds in
`http/feeds.rs`, folded into **five routes** — two on `transactions`, three on
`events`:

| route | access paths | arguments |
|-------|--------------|-----------|
| `GET /transactions/{hash}` | — | un-paginated; the hash is non-unique, so a **list**, newest-first |
| `GET /transactions/involving/{address}` | 1 | + optional `role`, `kind` |
| `GET /events` | 2, 5, 6 | `type` (a list) and/or `involved` — **at least one required** |
| `GET /events/contract` | 3 / 4 / 7 / 8 | mandatory `contract`, + optional `user`, `names` (a list) |
| `GET /events/perps` | 3 / 4 / 7 / 8, `contract` pre-bound to the perps address | + optional `user`, `names` (a list) |

`/events` folds the by-type feed (Q2) and the involving feeds (Q5/Q6):
`involved` anchors on the address (the type list is then a residual filter),
`type` alone anchors on `event_type` (a single type is the clean `idx_type`
scan, several a bounded `UNION ALL` per type — never a full-type sort). Neither
argument present has no index anchor — it would be a full-table scan + sort — so
it is a **400**. `/events/contract` makes `contract` mandatory, which keeps every
reachable combination index-anchored and makes the unsupported shapes (a name
filter without a contract, a type filter with a contract) structurally
impossible.

`/events/perps` is a **shortcut**, not a new access path: the same
contract-events feeds with the `contract` argument pre-bound to the
deployment's perps address, so the dominant consumer (perps activity) never has
to carry the address around. The address is injected at construction — the cli
resolves it from the node's `app_config` (the typed `addresses.perps`, next to
the shape-agnostic `Extractable` harvest that seeds the involvement blacklist)
and hands it to the projection through `ActivityConfig::perps_contract`, which
the events scope carries as scope-local app data. The pre-bound contract is the
index anchor, so the route needs no mandatory argument; when no address was
injected (`None`), the route is simply not mounted. Read-time only — the field
affects no written row, so changing it needs no re-backfill.

Each runs the access path above verbatim: hand-written, `Binder`-parameterized
SQL (`DISTINCT ON`, the involved ∪ sender union, the in-index `IN (…)` name
list, the row-comparison keyset), mapped back to the sea-orm models, and run
inside a transaction that applies `SET LOCAL statement_timeout` — a per-request
bound scoped to the read path (ingest and migrations share the pool but not this
transaction), so a pathological plan is cancelled server-side rather than
pinning a pool connection past a client disconnect. The
**type surface** — the `Transaction` / `Event` objects and the `UnitKind` /
`AddressRole` enums — lives in `http/types.rs`; the **keyset machinery** —
opaque `hex(json(tuple))` cursors, the page-size clamp, and the slim
`{ items, pageInfo }` envelope (`limit + 1` ⇒ `hasNextPage`) — in
`http/pagination.rs`. Addresses and hashes are grug's own `Addr` / `Hash256`:
their serde already speaks the canonical hex dialect, so the read API uses them
directly on input (`web::Path<Addr>` parses the hex) and output (no wrapper),
decoding the stored `BYTEA` back with `Addr::try_from` / `Hash256::try_from`;
`timestamp` is exposed as RFC 3339, never a 64-bit-lossy integer.

On top of the indexed columns, `Transaction` carries `tx` (the full submitted
transaction) and `outcome` (the execution outcome), and `Event` carries `data`
(the decoded `FlatEvent` payload). REST has no field selection, so these are
hydrated **eagerly**, in `http/hydrate.rs`: once a feed returns a page, the
handler batch-loads the page's distinct blocks through `block-source`'s
`load_blocks` (one `source.get` per height, deduped) and fills every row. `tx`
is `null` for cron units (which have no transaction); `outcome` carries the
unit's `TxOutcome` for a transaction and its `CronOutcome` for a cronjob,
externally tagged in the JSON (`{"transaction": …}` / `{"cron": …}`) so it is
self-describing. `MAX_LIMIT` bounds the per-page hydration cost; a block the
source no longer holds leaves that row's detail `null`.

For `Event.data` the priority payload never needs a block: each feed's inner
query is wrapped (`with_event_data`) into a subquery whose `data` is pulled per
row by a **correlated** lookup on `event_data`'s primary key — a scalar
subquery, not a join. The correlation guarantees a point index probe per row
however large `event_data` grows (a `LEFT JOIN` lets the planner hash-join with
a full `event_data` scan — `EXPLAIN`-verified on a small table); and the inner
`LIMIT` blocks subquery flattening, so the inner's backward-scan (no-sort) plan
is untouched (also `EXPLAIN`-verified: `Index Scan Backward → Unique → Limit`,
with the payload as a `SubPlan` index probe and no top-level sort). The priority
blob is `zstd`/`borsh`-decoded in `event_from_row`. Non-priority events are
absent from `event_data` (`data` comes back `NULL`), so for them
`hydrate_events` loads the unit's block and re-flattens it via the write path's
own `flatten_unit` — one source of truth for `event_index`. Net: priority
payloads add no round-trip; non-priority cost one (batched, deduped) block read
per distinct block in the page.

The routes are served by the projection-agnostic `httpd` crate, which injects
the Postgres pool and the block source as actix app data; the projection
contributes them through `Projection::services()` — two `web::scope`s
(`/transactions`, `/events`) of `#[get]`-routed handlers, grouped in
`http/services/` one module per resource — and the app gathers every
projection's scopes when it builds the server. The docs follow the same path:
`Projection::api_doc()` returns the OpenAPI fragment derived from the
`#[utoipa::path]` annotations on those same handlers (each resource module owns
its fragment, `/events/perps` included exactly when mounted), and the httpd
merges it into the document it serves at `/openapi.json` / `/docs/`.

## Out of scope

Still handled elsewhere (or not yet built): **live subscriptions**. The detail
layer is served eagerly — unit-level (`Transaction`'s `tx` and `outcome`,
`TxOutcome` / `CronOutcome`) and per-event (`Event`'s `data`, from the
`event_data` join with a block fallback). The tables and feeds here fix the
storage and read model those build on.
