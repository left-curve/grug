# `RemoteBlockSource` — design

Concrete implementation of the [`BlockSource`](../DESIGN.md#blocksource) trait
for the **detached** deployment: the archive runs on a host where
**no dango node runs**, so raw blocks are not available on the local
filesystem and the `LocalBlockSource` strategy (read the node's cache files
off disk) does not apply.

> See [DESIGN.md](../DESIGN.md) for the shared contract (`BlockSource`,
> `Projection`, app loop) and [local-block-source.md](./local-block-source.md)
> for the co-located V1 strategy. This document covers only the
> `RemoteBlockSource` concretions. Remaining gaps and review findings are
> tracked in
> [remote-block-source-known-issues.md](./remote-block-source-known-issues.md).

## Scope: what we build now vs. the end goal

The **end goal** (agreed with the team, _not_ built now but kept in view): a
set of sentinel nodes upload finalized blocks to an object store (B2) as
compressed chunks while keeping a hot window (24–48h) of recent blocks. The
`RemoteBlockSource` then pulls cold history from B2 and recent blocks from a
sentinel. This is the tiered vision in `~/specs/left-curve/`
(`indexer-storage-architecture.md`, `indexer-roles-and-responsibilities.md`).

The **step we build now**: deploy the archive on a node-less
server and have it pull _all_ blocks from a sentinel node (which today still
holds the entire history). No B2, no chunking. This is enough to validate the
whole `RemoteBlockSource` shape — own storage, remote fetch, gap handling,
live tail — without the cold-archive machinery. The B2 backend slots in later
behind the same fetcher trait (see [End goal](#end-goal-b2-layered-fetcher)).

## Why this impl

With no co-located node, two things change relative to V1:

1. **The source must own its storage of raw blocks.** The node's on-disk cache
   is unreachable. The store is a **local embedded RocksDB**: raw blocks are a
   `height → blob` archive — write-once, immutable, read by height, scanned
   sequentially during projection catch-up — which is an **ordered-KV**
   workload, not a relational one. The data is also **re-fetchable** (re-pull
   from the sentinel/B2), so it does not need a database server's durability
   machinery. The store additionally owns the **topology** (the contiguous
   frontier + the gaps above it), so a restart is a checkpoint read, not a scan.
2. **The source must fetch blocks over the network.** Both the historical
   backfill (from genesis) and the live tail come from a remote sentinel.

From the app's perspective the source still exposes the same four-method
`BlockSource` trait. The remote-ness is entirely internal.

## Architecture overview

Three internal pieces plus a thin coordinator, none exposed to the app:

```plain
RemoteBlockSource (impl BlockSource)
 ├── BlockStore                                  [trait; RocksDB impl + memory for tests]
 │     put → Option<frontier> · get · contiguous_frontier · lowest_gap
 │     owns the stored-height topology (frontier + gaps), persisted
 ├── HttpdClient (node `httpd` connection)        [concrete; one node-backed impl]
 │     subscribe_full_blocks() → live tail [tip..∞) as a stream of BlockData
 │     also backs the fetcher via BlockRangeClient (the `/block/full/range` call)
 ├── BlockFetcher                                [trait, ≥1 impl]
 │     spawn(from, to) -> FetchStream   (bounded backfill, dies at `to`)
 │       ├── SentinelBlockFetcher   ← built now
 │       └── ArchiveBlockFetcher    ← later (B2 + sentinel fallback)
 └── coordinator + broadcast_tx + heal_notify    [single serialized writer/broadcaster]
```

The division of labour:

- **`BlockStore`** persists and serves raw blocks **and owns the topology**.
  Its `put` folds each block into the contiguous-frontier/gap bookkeeping and
  reports when the frontier advances, so the coordinator stays a thin broadcast
  driver and the resume point is a checkpoint read. A trait (the RocksDB impl in
  production, an in-memory one for tests).
- **The live tail** follows the chain tip through the node's `full_block`
  subscription, opened on the shared `HttpdClient` (`subscribe_full_blocks`) —
  the _same_ path `LocalBlockSource` uses, only pointed at a remote sentinel. It
  is _always_ a sentinel subscription regardless of backend, because B2 only ever
  serves cold history — the tip always comes from a node. The subscription
  _yields_ fully-assembled `BlockData`; `drain_live` forwards them and the
  coordinator is the single writer to the store. There is no `LiveSubscriber`
  trait: there is one node-backed implementation, so the source holds the
  `HttpdClient` directly.
- **`BlockFetcher`** is the **only** axis that varies between the sentinel-only
  and the future B2-layered setup, so it is the trait that varies. It does
  **bounded** backfill: fetch a contiguous `[from, to]` and terminate.
- **The coordinator** is the single serialized writer + broadcaster. Both
  writers (the live tail and the fetcher) funnel through it; it calls `store.put`
  and broadcasts whenever the store reports the frontier advanced. It holds **no
  topology of its own** — no frontier/tip atomics, no in-memory island map; all
  of that lives in the store. The live `prev` height is a local variable in
  `drain_live` (used only to spot discontinuities), never shared or persisted.

**Fetch is decoupled from store.** The fetcher knows nothing about RocksDB; the
store knows nothing about sentinels or B2. Swapping the fetcher (sentinel →
B2-layered) touches neither the store nor the projections.

## Lifecycle: backfill, then steady state — healed continuously

- **Backfill.** On a fresh or lagging start there are gaps below the live tip.
  Two writers run concurrently: the **subscriber** writes the live tail
  `[L..∞)`, and a **healer** fills the gaps below it, spawning a _bounded_
  fetcher per gap. The store's topology holds disjoint islands until the gaps
  close.
- **Steady state.** Once every gap is filled the healer goes idle and only the
  subscriber writes, each new block being exactly `frontier + 1`. The store is
  trivially contiguous and stays so.
- **Re-healing.** The healer never exits. If the subscriber reconnects at a
  higher tip (a downtime hole) or drops a block, that hole is just a new gap
  below the tip; the healer notices (a discontinuity signal, or a periodic
  re-check) and re-fills it. The system briefly re-enters the dual-writer regime
  and settles back.

Each fetcher is **bounded** (one gap, then it dies) — that is what keeps
backfill from chasing a moving tip. The _healer_ is the continuous supervisor
that spawns one bounded fetcher per gap whenever a gap exists. The subscriber
captures the tip up front and writes the live tail during backfill, which is
what fixes each fetcher's target below the tip.

## Startup sequence

```plain
1. store.open()    // loads the topology checkpoint (O(#ranges), no scan), or
                   //   rebuilds it from the block keys if absent
2. spawn coordinator  // serialized writer: store.put → broadcast the advance
3. spawn drain_live   // owns the subscription: (re)subscribe, feed blocks to
                      //   the coordinator, track `prev` locally for skips
4. spawn healer       // loop: store.lowest_gap() → fill via a bounded fetcher;
                      //   idle when none
```

All three run for the source's lifetime; whichever returns first tears the
others down. There is **no `max_contiguous` seed** — the store owns and derives
the contiguous frontier from the topology it loaded. There is no one-shot
"backfill then stop" phase: the healer keeps the stored prefix gap-free, whether
the gap is the initial history or a later hole.

## The store: local RocksDB

The store is the durable raw-block archive **and** the owner of the
stored-height topology. The `BlockStore` trait:

```rust
async fn put(&self, height, data) -> Option<u64>; // persist + fold into topology;
                                                  //   returns the new frontier iff it advanced
async fn get(&self, height) -> Option<BlockData>; // point read by height
async fn contiguous_frontier(&self) -> Option<u64>; // O(1), from the topology
async fn lowest_gap(&self) -> Option<(u64, u64)>;   // O(1), drives the healer
```

- **`put` is idempotent** (a re-put is a no-op) and is the **single writer's**
  one call: it persists the block, folds the height into the topology, and
  returns the new contiguous frontier **iff this put advanced it**. `None` means
  a duplicate or an island above a gap. On a **bulk-advance** — a put that
  bridges the prefix to an already-stored run — the returned frontier can jump
  far past `height`.
- **`get`** is a point read by height; projections use it in catch-up.
- **`contiguous_frontier` / `lowest_gap`** are answered from the topology in
  O(1) — never a scan.

### The topology: `StoredRanges`

The frontier/gap bookkeeping lives in one type, `StoredRanges` — a coalesced set
of the present-height ranges (`rangemap::RangeInclusiveSet`). One range
`[genesis, X]` ⟺ no gaps; a hole splits it. Its size is **O(#gaps + 1)**, never
O(#blocks): a 100M-block contiguous archive is a _single_ range. It is the
**single owner of the topology math** — `contiguous_top` (the frontier),
`first_gap` (the healer's next target), and `insert` (which reports the frontier
advance) — so every `BlockStore` impl is a thin I/O adapter over it with no
duplicated logic. Because it is tiny, a persistent store can checkpoint it
(`to_ranges`) and reload it in O(#ranges) at boot (`from_ranges`) instead of
scanning every stored height.

This is what kills the cold-start scan: with 100M blocks and one gap at 70M, a
naive "find the frontier" scans 70M keys. Here boot reads the checkpoint and the
frontier is `O(#ranges)` away.

### RocksDB layout

- **`blocks` column family** — key = `height.to_be_bytes()` (big-endian, so
  RocksDB's lexicographic order _is_ height order), value = borsh-encoded
  `BlockData { block, outcome }`. (`BlockData` derives borsh; this is the same
  on-disk format the dango node uses for its block cache files.)
- **default CF** — one key, the topology checkpoint, as a borsh `Vec<(u64,u64)>`.
  Metadata lives in the default CF by convention (mirrors the chain's disk DB,
  whose default CF holds its `latest_version`), and keeping it out of the blocks
  CF means a topology rebuild scan sees only clean 8-byte height keys.

A `put` writes the block (`blocks` CF) and the topology checkpoint (default CF)
in **one `WriteBatch`** — atomic across column families — so the checkpoint is
always consistent with the blocks.

### Durability ordering

`put` computes the next topology on a clone, writes block + checkpoint
atomically, **then** commits the advance to the in-RAM topology mirror:

```plain
1. clone topology, insert height, compute the advance   (RAM untouched)
2. WriteBatch { blocks[h] = block, default[topology] }  → db.write   (durable)
3. commit the new topology to the RAM mirror            (advance now visible)
```

The block is durable (step 2) **before** the frontier reveals it (step 3), so
`h ≤ frontier ⟹ get(h) = Some` holds even across a crash. The topology is
mirrored in RAM (a `std::sync::Mutex<StoredRanges>`, authoritative for queries)
so `contiguous_frontier`/`lowest_gap` are lock-brief and never touch disk; the
guard is never held across the I/O.

### Boot

`open` reads the checkpoint from the default CF and reloads the topology in
O(#ranges). If the checkpoint is absent (a fresh DB, or a one-off recovery) it
**rebuilds** the topology by scanning the block keys — O(n), but only on the
recovery path, and the blocks are re-fetchable anyway.

### CF tuning (blocks)

The `blocks` CF is tuned for the workload — fixed 8-byte big-endian keys, large
immutable compressible values, append-mostly writes, never overwritten or
deleted. (The fixed key size is _not_ where the wins are; the layout is already
ideal for ordered point reads. The tuning targets the values and the writes.)

- **Key-value separation (BlobDB)** — the large block payloads go to blob files,
  leaving the LSM tiny (`key → blob pointer`). Compaction then does **not**
  rewrite the big values (low write-amplification on a multi-million-block
  backfill), and key scans (the topology rebuild) stay cheap. Blocks below
  `min_blob_size` (4 KB) stay inline, so small cron-only blocks pay no
  indirection. Blob compression: zstd.
- **SST compression** — lz4 on the churny upper levels, zstd on the bottommost
  level where ~all of the archive settles (max ratio for the cold bulk).
- **Append-only compaction** — leveled with `dynamic_level_bytes` (minimal space
  amplification), larger memtable, and `max_background_jobs=6` /
  `max_subcompactions=4` so compaction keeps up with the backfill burst (which
  writes far faster than the ~2.79 blk/s live rate) and avoids L0 write stalls.
- **Read path** — whole-key bloom filter, larger block size, and a **1 GiB block
  cache** holding **partitioned** index+filter (`TwoLevelIndexSearch` +
  `partition_filters`, top level pinned): at 100M keys index+filter alone are
  ~350–450 MB, so a small cache or a monolithic per-SST filter would thrash.
- **Blob cache** — a dedicated 512 MiB cache for the payloads (the ~95% of bytes
  that live in blob files, otherwise uncached); helps the query service and any
  projection that re-reads recent blocks.

The WAL is left **un-synced** (default): a process crash is survived (RocksDB
recovers from the WAL on restart), and a power-loss tail loss is safe because
the lost blocks and their checkpoint are dropped together (atomic batch) →
consistent, just behind → the healer re-fetches. The re-fetchable property buys
fast writes for free.

> The thresholds are tuned against the **measured** mainnet workload (2026-06-24:
> ~32.5M blocks at ~2.79 blk/s → ~100M in ~9 months; `BlockData` payloads median
> ~15–25 KB, p90 ~150 KB, max ~0.5 MB borsh). `min_blob_size` stays 4 KB — the
> payloads are well above it, so ~all go to blobs. Surfacing the knobs under
> `remote.*`, and re-checking the cache against the live cache-hit-rate once
> deployed, is a future task.

## The live tail

The live tail is the node's **`full_block`** subscription, opened on the
`HttpdClient` via `subscribe_full_blocks()` (always at the tip) — the same shared
path
`LocalBlockSource` uses, only pointed at a remote sentinel rather than an
in-process `dango-httpd`. Each event carries the **whole `BlockData`** (block +
outcome) as a JSON scalar, decoded directly, so the call hands back just a
**stream of blocks** — the first delivered block is the resume point, with **no
separate tip to return** (it is just `first.height()`, so plumbing it out-of-band
is redundant). `drain_live` forwards the yielded blocks to the coordinator (the
single store writer) and reconnects on drop; `subscribe_full_blocks` only opens
the one subscription and yields. There is no `LiveSubscriber` trait — one
node-backed implementation, so the source holds the `HttpdClient` directly.

The subscription **always opens at the live tip** (no `since`). The node serves
`full_block` from a small in-memory ring (~100 blocks), so resuming at
`frontier + 1` would fail with a "resync required" error whenever the frontier
sits more than ~100 blocks below the tip — i.e. for the entire initial backfill,
and after any downtime longer than the ring — permanently wedging the live tail.
Taking the tip instead makes the downtime hole just another gap below the new
max-stored height: the store reports it and the healer fills it via the fetcher
(`/block/full/range`, which _does_ serve deep history). The chain produces blocks
in order and the subscription delivers them in order, so `[tip..∞)` is contiguous;
everything below the tip is the healer's job.

Because the subscription **carries the payload**, the live tail needs no
follow-up read — neither the V1 disk read nor a per-height RPC. That is the V1→V2
simplification the `full_block` channel buys: V1 read the payload from the node's
local cache file; V2 has no local disk, but the sentinel now ships the payload
inline, so both sources share one subscription that yields complete blocks (see
[Payload delivery](#payload-delivery-one-call-via-full_block)).

Crucially the live tail is **stored immediately**, from the first delivered block
onward, _during_ the backfill (the coordinator persists each yielded block). That
first block raises the store's max height, so everything below it is a bounded
gap the healer fills lowest-first — which keeps each fetcher's target finite and
lets it die. Without storing the live tail during backfill, the fetcher would
chase a moving tip forever.

`drain_live` tracks a local `prev` height — `None` until the first block sets the
baseline; thereafter a jump beyond `prev + 1` wakes the healer (a skip, a
reconnect at a higher tip, or a reorder all trip this; the healer's grace absorbs
a transient reorder). There is no shared or out-of-band tip state.

### Payload delivery: one call via `full_block`

Both the live tail (per live height) and the `SentinelBlockFetcher` (per
backfilled height) get a complete `BlockData` in **one** call against the
sentinel's `httpd`, which loads its cache file once and returns `block` +
`outcome` together:

- **Live tail** — the `full_block` GraphQL subscription; each event is a
  `FullBlock` JSON scalar that `BlockData` (an alias of it) deserializes directly.
- **Backfill** — `GET /block/full/range?from=&to=`, a JSON array of the same
  shape (see [the fetcher](#the-block-fetcher)).

This replaces the earlier two-call assembly (`query_block` +
`query_block_outcome`, which at 100M blocks was ~200M calls for a full backfill
and loaded the _same_ node cache file twice per height — once per half). The
combined routes live in the node's `dango/indexer` httpd and shipped there first;
the archive source depends on a sentinel exposing them. The wire shape is
`{ block, outcome }`, decoded with **serde**: `BlockData` _is_ the node's
`dango_primitives::FullBlock` (which derives `Serialize`/`Deserialize`), so there
is no private wire type to keep in sync with the node — borsh stays the on-disk
format only.

## The block fetcher

The fetcher does **bounded** backfill of one contiguous range and then
terminates. Adapted almost verbatim from the bots `BlockFetcher` — see
[Borrowed pattern](#borrowed-pattern-bots-blockfetcher).

```rust
pub trait BlockFetcher: Send + Sync {
    /// Spawn the fetch task for the inclusive range `[from, to]`. The returned
    /// stream yields blocks ascending and terminates after `to`. Dropping the
    /// stream aborts the task.
    fn spawn(&self, from: u64, to: u64) -> FetchStream;
}
```

`FetchStream` is a backend-agnostic handle: a bounded, backpressured `mpsc`
channel (so a slow store writer throttles the fetcher instead of letting it
balloon RAM) plus abort-on-drop. `recv()` yields the next block; `queue_len()`
is the fetched-but-not-yet-consumed backlog (the reindex-bottleneck signal).

**The consumer validates, not trusts.** A fetcher _should_ emit exactly the
ascending contiguous range, but `backfill_gap` checks each height against the
one it expects and treats a mismatch — or a stream that ends before `to` — as a
failure, never as "range complete". This keeps a misbehaving backend from
silently corrupting the store, uniformly across every fetcher impl.

Design notes:

- **Bounded, one range, dies.** `spawn(from, to)` is the primitive; the healer
  invokes it once per gap (lowest-first). The fresh-start case is the single big
  gap below the live tip.
- **Per-gap, not gap-list.** A single range composes with the future B2 layered
  fetcher: each gap routes to the backend that owns its height range (old → B2,
  recent → sentinel) without the fetcher knowing anything.
- **Shared `FetchStream`.** The channel + backpressure + abort-on-drop are a
  concrete shared type; only the fetch _loop_ (sentinel poll vs. B2 chunk read)
  varies per impl.

### `SentinelBlockFetcher` (built now)

A background task that pulls blocks from a sentinel's `GET /block/full/range`
endpoint (which caps a response at `MAX_BLOCK_RANGE` = 20 heights). Each round
issues one **batch**: up to `parallelism` calls on fixed contiguous strides of
`range_size` heights, each spawned as its own runtime task (guarded by
`AbortOnDrop`, so dropping the fetch task tears down its in-flight strides).
Spawning is what parallelizes the CPU half of a call — body assembly and the
serde_json parse, the dominant per-call cost in dense eras — across the
runtime's workers; polled as plain futures inside the fetch task they would
serialize on one thread, capping the backfill near one core of JSON throughput
(~40 MB/s) regardless of `parallelism`. `join_all` over the handles preserves
submission order, so the responses come back height-ordered and are committed
block by block while they stay contiguous with the next expected height — the
stream stays strictly ascending with no reorder buffer. The first anomaly (a request error or
timeout, an empty response, or a run that breaks contiguity — which is also how
a **short** run surfaces, since the next fixed stride then no longer aligns)
discards the rest of the batch, and the next round re-issues from the first
height not yet delivered, after a backoff. Refetching a discarded tail trades a
little bandwidth for never buffering out-of-order blocks; anomalies are
transient and rare, so the cost is nil in steady state.

Why parallel at all: in block-dense height regions a call is **payload-bound**
(hundreds of KB per block — the sentinel's disk read + JSON serialization on
one end, our body assembly + JSON parse on the other), so a serial pull caps
at one response-time per `range_size` blocks however fast the store writes.
`parallelism` overlaps the HTTP waits *and* spreads the parses over that many
workers; `parallelism = 1` restores the serial pull. No adaptive ramp:
every height in a gap is below the live tip and therefore exists, so there is
no tip to probe for. The endpoint returns the **full `Block`** (not just
`block.info`), since projections need txs and events.

## The coordinator: forward the advance to the broadcast

The coordinator is a single task draining a bounded channel fed by both writers
(the subscriber via `drain_live`, the healer via its fetcher). All topology lives
in the store, so the coordinator does almost nothing:

```rust
while let Some(block) = coordinator_rx.recv().await {
    let height = block.height();
    let Some(frontier) = self.store.put(height, &block).await? else {
        continue; // duplicate, or an island above a gap — nothing to broadcast
    };
    // The store advanced the frontier. Broadcast only its top: the block we hold
    // on a plain +1, otherwise the stored top of the run we just bridged.
    let top = if frontier == height {
        block
    } else {
        self.store.get(frontier).await?.expect("frontier block is durable")
    };
    self.broadcast_tx.send(Arc::new(top)).ok();
}
```

- **Persist → broadcast.** The block is durable (inside `put`) before any
  projection hears of it. (This replaces the earlier "broadcast-first" ordering:
  on a fast local store the latency saved by broadcasting first is negligible,
  and persist-first is both simpler and the natural shape now that `put` reports
  the advance. A broadcast that is missed on a crash is recovered by the
  projection's Phase-1 `get()` pull.)
- **Classification is in `put`.** Duplicate / island / edge / bridge are all
  decided inside the store; the coordinator only reacts to the returned advance.
- **Bulk-advance.** When `put` returns a frontier far past `height` (a gap-fill
  that bridged a stored island, or an out-of-order block that completed a run),
  the coordinator broadcasts **only that top**; the skipped heights are durable
  and pulled by projections via Phase-1 `get()`, so a large catch-up backlog
  never floods the pubsub. In steady state every live block is `frontier + 1`,
  so `frontier == height` and every block is broadcast — full push latency.

### Write vs. broadcast (the key distinction)

Two different events; conflating them is the easy mistake:

- **Write to the store** happens immediately, by whichever writer produced the
  block — the subscriber writes `[L..∞)` even while the frontier is far below
  `L`.
- **Broadcast** happens only when the **frontier reaches** that block. A block is
  un-broadcastable until the prefix below it is stored. The subscriber therefore
  cannot broadcast directly: it would emit `250` while the frontier is `50`,
  breaking the projection loop.

So during backfill the pubsub is _not_ idle — the coordinator broadcasts as the
frontier climbs — and a projection can ride the broadcast as soon as it reaches
the (still-climbing) frontier. When the last gap closes, the frontier crosses the
live-tail island in one bulk step and settles into steady state.

## Gap handling

The store can hold disjoint islands (a restart, or a later reconnect hole), so
gap-filling is a continuous, lowest-first process:

```plain
store on restart : {1..50, 200..210}   live subscribe resumes at L = 250
→ live block 250 is stored as an island, raising the max stored height
→ store.lowest_gap() = (51, 199)        // the lowest hole in [genesis, max_stored]
→ fill it; lowest_gap() then returns (211, 249); fill it; then None
```

- **The store computes the gap** from its topology, O(1) — the lowest hole at or
  above genesis, bounded by the max stored height. The healer asks for the
  _lowest_ gap only (never the whole list), so a 30M-wide island above the hole
  is never walked.
- **Fill lowest-first.** If `[211..249]` were filled before `[51..199]`, the
  frontier would stay pinned at `50` and no projection could advance. Ascending
  → the frontier climbs immediately and projections drain progressively. A
  liveness requirement, not a preference.
- **The downtime hole is captured for free.** The live block at the new tip
  (`250`) raises the max stored height, so `lowest_gap` reports `[211..249]`
  once `[51..199]` is filled — no separate "tip" plumbing needed; it falls out
  of the topology.

## Upholding the `BlockSource` invariants

| Invariant                               | How it holds                                                                                                                                                                                    |
| --------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `contiguous_frontier()` monotonic       | Only `put` advances the frontier, only upward. Bulk jumps over islands are fine — monotonic ≠ `+1`.                                                                                             |
| `h ≤ frontier ⟹ get(h) = Some`          | `put` persists the block (atomic batch) **before** committing the frontier advance to the RAM mirror that `contiguous_frontier` reads.                                                          |
| broadcast strictly ascending (may skip) | Only the coordinator broadcasts, and only the frontier top each `put` reports. Normally `+1`; on a bulk-advance it jumps and the projection loop pulls the skipped heights via Phase-1 `get()`. |
| `get(h) = None` is "not yet"            | A height inside an unfilled gap returns `None`; the source never GCs a height a projection might still reach (no GC in this version).                                                           |

## Failure modes

| Failure                                        | Behavior                                                                                                                                                                                                                                       |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Subscriber connection drops                    | `drain_live` reconnects with backoff and resumes at the new tip. Blocks missed during the outage become a gap below the new tip; the healer fills it. Frontier stalls at the contiguous prefix meanwhile.                                      |
| Subscriber drops a block mid-stream            | A jump in the delivered heights wakes the healer, which fills the hole from the sentinel RPC (the block exists; only the live _notification_ was lost). A periodic re-check is the backstop.                                                   |
| Sentinel down                                  | Both subscriber and fetcher retry. Frontier stalls; projections keep serving up to the old frontier. No data loss — the store is durable.                                                                                                      |
| Fetcher RPC error / 404 mid-gap                | The fetch task backs off and retries from the failed height. A 404 inside a gap means "not yet on the sentinel" — retry, never "absent". A _permanently_-unservable block would retry forever — a future observability item (known-issues #5). |
| Crash mid-backfill                             | Restart reads the topology checkpoint and resumes; the healer refills from `lowest_gap`. Idempotent `put` makes any re-fetched block harmless.                                                                                                 |
| Crash between block write and frontier advance | The block + checkpoint are one atomic batch, so they land together — the topology on restart already includes the block. No hole, no stale checkpoint.                                                                                         |
| Store `put` / `get` fails (RocksDB error)      | Coordinator halts, source exits — intentionally fatal. A supervised restart recovers from the durable store. In-place retry is deliberately avoided (the store is the durability anchor).                                                      |
| Corrupt payload                                | `get` surfaces a borsh error via `anyhow`. Skip-vs-halt is an open question (leaning halt — no in-process indexer cross-checking here).                                                                                                        |

## Borrowed pattern: bots `BlockFetcher`

`bots/types/src/block_fetcher.rs` already solves the hard part and we lift it:
the background task + `AbortOnDrop` + bounded channel + `recv()` interface, the
ordered-output discipline (send `block, block+1, …` in sequence, resume from
the last height actually received), and the **concurrent-batch shape** — issue
N fetches at once, commit the results in submission order via `join_all`,
discard from the first anomaly and re-issue from the last committed height.
Adapted: the unit of a fetch is a `/block/full/range` stride (up to 20 blocks)
rather than a single height, we emit the full `Block` (not `block.info`), and
we add the `from/to` bound so the task terminates. We drop the adaptive ramp
(`loops = min(i+1, max_loops)`): it exists to follow the tip, where most probed
heights don't exist yet — a bounded gap sits entirely below the live tip, so a
fixed `parallelism` suffices.

## Configuration

The deployment-specific fields — `store_path`, the sentinel `live_url`, and the
fetcher kind — are wired through the CLI's `remote.*` config section. The
**tuning** structs below still use their Rust `Default`s; mapping them onto
`remote.*` TOML keys is a future task.

`RemoteBlockSourceConfig` — `pubsub_buffer_size` (broadcast capacity),
`coordinator_buffer` (the coordinator's input channel), `heal_poll_interval`
(the healer's periodic re-check), `reorder_grace` (delay after a discontinuity
signal, to absorb an out-of-order delivery before fetching), `reconnect_backoff`
(between live re-subscribes).

`SentinelFetcherConfig` — `range_size` (heights requested per `/block/full/range`
call, clamped to the gap and capped at `MAX_BLOCK_RANGE` = 20), `parallelism`
(concurrent range calls per batch — the only tuning knob also wired through the
TOML, as `[block_source.fetcher] parallelism`, env-overridable as
`BLOCK_SOURCE__FETCHER__PARALLELISM`), `timeout` (per-request RPC timeout),
`channel_capacity` (fetch-ahead backlog — the dominant backfill-RAM knob),
`retry_backoff`.

Already wired through `remote.*`: `live_url` (the sentinel's base `httpd` URL,
from which `HttpdClient::new` derives both the `full_block` subscription and the
`/block/full/range` endpoint — it feeds the live tail and the fetcher's client
alike) and `store_path` — the **local directory** for the source's RocksDB.
Unlike the projections' Postgres, the raw
store is a private on-disk store owned by this source; nothing else reads it
(the query service reaches blocks through the same in-process `source.get`), so
each indexer host owns its own store. The blocks-CF tuning is currently
hardcoded (see [CF tuning](#cf-tuning-blocks)); exposing it under `remote.*` is a
future knob.

## End goal: B2-layered fetcher

_Not built now; recorded so this version's decisions don't close the door._

The end-state replaces `SentinelBlockFetcher` with an `ArchiveBlockFetcher` that
pulls cold history from B2 and recent blocks from a sentinel — same
`BlockFetcher` trait, same `spawn(from, to)`. Because the source already drives
the fetcher **per gap**, the layered fetcher routes each gap by height: old →
B2 chunks, recent → sentinel hot window. Nothing in the store, coordinator,
subscriber, or projections changes. The B2 side (chunk format, zstd dictionary,
manifests, the overlap invariant between the hot window and the chunk cadence)
is specified in `~/specs/left-curve/indexer-storage-architecture.md`.

**Storage cost** (raw, zstd; the store compresses on disk):

| Period              | Approx. |
| ------------------- | ------- |
| Today (~22M blocks) | ~210 GB |
| 1 year              | ~600 GB |
| 5 years             | ~2 TB   |

**Optional GC** (when storage becomes a concern): when all registered
projections are past height `H`, drop blocks below `H` (a RocksDB range delete,
or a per-epoch column family dropped whole). New projections deployed after a GC
fetch the missing blocks transparently via the fetcher → B2. This is the one
place the `get(h) = None` invariant interacts with GC: a GC'd height must be
re-fetchable, never reported as permanently absent.

## Open questions

- **Subscriber wire protocol / combined block endpoint** — _resolved._ The live
  tail is the node's `full_block` subscription and the backfill is
  `GET /block/full/range`; each delivers the whole `BlockData` in one call, so the
  two-call `query_block` + `query_block_outcome` assembly is gone (see
  [Payload delivery](#payload-delivery-one-call-via-full_block)).
- **Fetcher error model** — structured errors (e.g.
  `NotAvailable { earliest }`, needed once B2 lands) vs. plain 404s for the
  sentinel-only version.
- **Blocking I/O** — the RocksDB calls run directly inside the async store
  methods. Fine for the coordinator's serial puts and point gets; under heavy
  load they could move to `spawn_blocking`.
- **Schema version on the payload** — include a `u16 schema_version` from day one
  to avoid painful migrations of the on-disk format.
- **Corrupt-payload policy** — skip + alert vs. halt. Leaning halt, but wants a
  config knob.
- **Configurable CF tuning** — surface the blocks-CF knobs under `remote.*` once
  there are real block sizes to tune against.
- **Parallel gap filling** — fill multiple gaps concurrently for throughput?
  Deferred; sequential lowest-first is simpler and projection processing, not
  fetch, is usually the bottleneck.
