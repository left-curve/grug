# `LocalBlockSource` — V1 design

Concrete implementation of the [`BlockSource`](../DESIGN.md#blocksource) trait
for the V1 deployment: the archive runs on the same host as the
dango node and reuses the data the node already persists.

> See [DESIGN.md](../DESIGN.md) for the shared contract (`BlockSource`,
> `Projection`, app loop). This document covers only V1-specific concretions.

## Why this impl

The dango node, through its in-process indexer (`indexer/sql/`,
`indexer/clickhouse/`), already writes every finalized block to disk and
exposes a GraphQL `full_block` subscription that streams each new block with its
full payload. V1 piggy-backs on both — the subscription for the live tail, the
on-disk cache files for catch-up reads — no duplicate storage, no new wire
protocol.

## Inputs the source relies on

1. **GraphQL `full_block` subscription** at `dango-httpd` — streams each newly
   indexed block as a complete `BlockData` (block + outcome) in one event. This
   is the live tail; the source reads nothing from disk to serve it.
2. **Cache files on disk** — written by the node's `dango-indexer-cache` crate at:

   ```plain
   <dir>/blocks/<last-3-digits-of-height-reversed>/<height>.borsh[.xz]
   ```

   Each file is a borsh-serialized
   `CacheFile { data: BlockAndBlockOutcomeWithHttpDetails { block, block_outcome, http_request_details }, .. }`,
   optionally lzma-compressed (`.xz`). Used only for **catch-up** (`get(h)`), not
   the live tail. The archive drops `http_request_details` and keeps
   only `block` + `outcome` — the shape of `BlockData` (the node's `FullBlock`).

Both inputs come from the same dango node process. If the node is down,
both are down — single failure domain, no consistency issues to worry about.

## Live tail — the `full_block` subscription

The live half of the source opens a WebSocket to `dango-httpd` and subscribes to
`full_block`. Each event carries the whole `BlockData`, so the source decodes it,
advances the frontier, and broadcasts — **no disk read on the live path**. This
is the _same_ `subscribe_full_blocks` call the `RemoteBlockSource` uses; only the
base URL differs (in-process `dango-httpd` here, a remote sentinel there), so the
two sources share one live-tail implementation (`HttpdClient`).

Why a GraphQL subscription over polling or inotify:

- **API contractual** — depends on the GraphQL schema (stable, versioned)
  instead of the on-disk file layout.
- **Reconnection** is solved by the WS client wrapped in the source's own
  resume loop (see [Frontier](#frontier--from-the-live-feed-not-a-boot-query)).
- **"Live from tip"** semantics come for free — `subscribe_full_blocks()`
  starts at the chain head and moves forward.
- **No file enumeration** to discover what appeared.
- **Shared with V2** — the same subscription transfers to a remote sentinel
  unchanged; only the base URL differs. What differs downstream is that V2 also
  owns a store and a coordinator, where V1 broadcasts directly. See
  [remote-block-source.md](./remote-block-source.md).

The subscription carries the full `Block + BlockOutcome`, so the live path never
touches disk. The cache files matter only for catch-up `get(h)` — a projection
reaching for a height below the live tip.

## Catch-up — disk read on demand

When a projection lags behind the frontier, it calls `source.get(h)`. In V1
that's a single-file read off the node's cache:

```plain
path = cache_path.block_path(h)        // <dir>/blocks/<…>/<h>.borsh[.xz]
if !CacheFile::exists(path) → Ok(None)  // "not yet", not an error
CacheFile::load_from_disk_async(path)
return BlockData { block, outcome }     // drops http_request_details
```

`CacheFile` handles both compressed (`.xz`) and uncompressed files
transparently. A missing file maps to `Ok(None)` — the `get(h) == None` is
"not yet" contract — so a projection that has outrun the node's on-disk writes
simply retries.

No batching in V1: the projection loop reads one block at a time. A future
optimization could expose `get_range(from, max_count)` if profiling shows
disk read overhead matters; for now, page cache hides it.

## Frontier — from the live feed, not a boot query

The source holds `frontier: AtomicU64` in memory, mutated only by the `run()`
task; reads are lock-free.

**At boot** there is no separate query. `run()` opens the subscription at the
live tip (`subscribe_full_blocks()`) and the **first block that arrives sets
the baseline** — the frontier jumps straight to it. No walk of the on-disk
blocks, no `latest_block_height` round-trip; the feed itself tells the source
where the tip is.

**At runtime** each delivered block is checked against the frontier:

- `height <= frontier` — already seen (a re-delivery after a reconnect); skip.
- `height == frontier + 1` — advance the frontier and broadcast.
- `height > frontier + 1` while `frontier != 0` — a jump (a dropped live event,
  or a reconnect at a higher tip). The frontier advances **straight to `height`**
  and broadcasts it; the skipped heights are **not** a hole to replay — they are
  already on the node's disk, so the projection loop pulls them via `get`. (The
  first block, with `frontier` still 0, is the baseline.)

The subscription **always opens at the live tip** (`subscribe_full_blocks`, no
`since`). Unlike `RemoteBlockSource` there is no store to heal from — but there
needs not be: every height at or below the frontier is reachable through `get`
(the co-located node holds every finalized block on disk), so a reconnect simply
re-baselines the frontier at the new tip and the projection loop fills the skipped
range from disk. Resuming at `frontier + 1` instead would fail against the node's
~100-block ring ("resync required") after any non-trivial downtime — a wedge with
no recovery here, since V1 has no healer.

### Why not query the indexer for the boot frontier

An earlier design queried `dango-httpd` for the latest indexed height at boot.
With the `full_block` feed carrying the payload, that round-trip buys nothing:
the first event delivers the height _and_ the block, so the source baselines and
broadcasts in one step. Dropping the query also removes an implicit ordering
dependency on a separate GraphQL query field being wired up, and a class of
"frontier is 1–2 blocks ahead of / behind the feed" skew.

## Internal architecture (informal)

```plain
                  ┌──────────────────────────────────┐
                  │ LocalBlockSource                 │
                  │                                  │
   dango-httpd ──→│  WS sub task (full_block):       │
   (full_block    │    recv BlockData                │
    subscription) │      → advance frontier          │
                  │      → broadcast Arc<BlockData>  │
                  │                                  │
                  │  get(h) call:                    │
                  │    → read file from disk         │
                  │    → return BlockData            │
                  │                                  │
                  │  subscribe():                    │
                  │    → broadcast::Receiver         │
                  └──────────────────────────────────┘
```

One internal task (the subscription loop). `get` is synchronous from the
source's point of view — no shared state beyond the frontier atomic and the
broadcast sender.

## Failure modes

| Failure                                | Behavior                                                                                                                                                                                                                                       |
| -------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| WS connection drops                    | The source reconnects with backoff and resubscribes at the live tip; the heights produced during the outage are read from disk by the projection loop's `get` catch-up. The frontier stalls until the feed is back, then jumps to the new tip. |
| `dango-httpd` down but node up         | Same as a WS drop — the source retries. Files on disk keep growing; the projections keep draining them via `get` regardless of the frontier, and the frontier catches up once the feed returns.                                                |
| Gap in the live feed (a dropped event) | A delivered height beyond `frontier + 1` advances the frontier straight to it and broadcasts; the skipped heights are pulled from disk via `get`, never replayed over the (small-ring) subscription.                                           |
| Cache file missing for a `get(h)`      | Maps to `Ok(None)` — "not yet"; a projection that outran the node's on-disk writes retries.                                                                                                                                                    |
| Cache file present but corrupt         | `CacheFile::load` returns a borsh error, bubbled up via `anyhow`. Operational decision (skip vs halt) deferred — TBD.                                                                                                                          |

## Coupling with `dango-indexer-cache`

The source depends on the concrete shape of `CacheFile` and
`BlockAndBlockOutcomeWithHttpDetails` in the `dango-indexer-cache` crate. Any
layout change in that crate is a breaking change for the archive.

Since both crates live in the same monorepo, this is a compile-time
invariant: a refactor in `dango-indexer-cache` either updates the source or
fails to build.

## Open questions

- Should we expose `get_range(from, max_count)` on `BlockSource` to let
  projections in catch-up batch disk reads? Marginal until profiled.
- Behavior on corrupt-file detection: skip + alert vs halt. Probably skip
  in V1 (the in-process indexer would also catch it) but needs a config knob.
- Reconnection backoff is a fixed `RECONNECT_BACKOFF` (5 s) today; surfacing
  it as config is a later knob.
- `dango-httpd` startup ordering: if the archive starts before
  `dango-httpd` is reachable, the source should retry rather than fail
  hard. Standard pattern, but make sure it's exercised in tests.
