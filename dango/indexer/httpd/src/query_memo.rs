//! A per-block memo for the `/ws` `query` subscription.
//!
//! Chain state only changes at block boundaries, so a query's result is fully
//! determined by the pair (block, query). When many subscribers hold the same
//! standing query, every tick of a block would otherwise execute the query —
//! including contract code, for `wasm_smart` — once per subscriber. The memo
//! collapses that: the first asker for a (trigger height, query) pair runs the
//! query, and every concurrent or later asker for the same pair awaits the
//! same shared future.
//!
//! Two invariants keep the memo correct in the face of the commit/trigger
//! race (a query triggered by block `N` executes against *latest* state, which
//! may already be `N + 1`):
//!
//! - The generation advances on **trigger** heights only, which are monotonic
//!   because the block ring is appended in strict height order. A response's
//!   own reported height is opaque payload and never feeds back into the
//!   memo's bookkeeping.
//! - A response is always at least as new as its trigger key, because the ring
//!   is appended after commit. A hit can therefore never serve state *older*
//!   than the block that triggered it. The one path that could violate this —
//!   a subscriber still processing trigger `N` after another subscriber has
//!   advanced the generation to `N + 1` — bypasses the memo instead (see
//!   [`QueryMemo::query_at`]).

#[cfg(feature = "metrics")]
use metrics::{counter, describe_counter};
use {
    crate::context::MinimalContext,
    dango_primitives::{BorshSerExt, Query, QueryResponse},
    futures_util::{
        FutureExt,
        future::{BoxFuture, Shared},
    },
    serde::Serialize,
    std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    },
};

/// Bound on distinct queries memoized within one block. Beyond it, further
/// distinct queries run uncached rather than grow the map — the memo is an
/// optimization, never a requirement.
const MEMO_MAX_ENTRIES: usize = 256;

/// One query-subscription frame: the response and the block height it was
/// served at. Serialized as a `query` channel data frame's `data` payload.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryFrame {
    pub block_height: u64,
    pub response: QueryResponse,
}

/// The shared, cloneable outcome of one query execution. `String` errors
/// because [`Shared`] requires the output to be `Clone`, which the app's
/// error type is not; errors are memoized like successes, since a
/// deterministic failure repeats identically until state changes.
type SharedQuery = Shared<BoxFuture<'static, Result<Arc<QueryFrame>, String>>>;

/// Coalesces identical [`Query`] executions triggered by the same block. See
/// the module docs for the invariants.
#[derive(Default)]
pub struct QueryMemo {
    /// Locked only to look up or insert an entry — never across an `.await`.
    inner: Mutex<MemoInner>,
}

#[derive(Default)]
struct MemoInner {
    /// The trigger height the current entries belong to. A lookup at a newer
    /// height clears the map — that is the entire eviction policy, since old
    /// heights never recur.
    height: u64,

    /// Live or completed executions for this generation, keyed by the query's
    /// canonical Borsh bytes.
    entries: HashMap<Vec<u8>, SharedQuery>,
}

impl QueryMemo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the response for `query` as triggered by block `height`,
    /// executing it at most once per (height, query) across all callers.
    pub async fn query_at(
        &self,
        height: u64,
        query: Query,
        ctx: MinimalContext,
    ) -> Result<Arc<QueryFrame>, String> {
        // How the lookup was answered: sharing an existing execution, starting
        // the execution others will share, or bypassing the memo entirely.
        enum Lookup {
            Hit(SharedQuery),
            Miss(SharedQuery),
            Bypass,
        }

        let lookup = match query.to_borsh_vec() {
            Ok(key) => {
                let mut inner = self.inner.lock().unwrap();

                if height > inner.height {
                    inner.entries.clear();
                    inner.height = height;
                }

                if height < inner.height {
                    // This caller is skewed behind the newest trigger. Its
                    // execution would read *current* state, so parking the
                    // result in the newer generation's map could serve, for
                    // that newer trigger, state older than the trigger itself.
                    // Bypass the memo: correctness over deduplication.
                    Lookup::Bypass
                } else if let Some(shared) = inner.entries.get(&key) {
                    Lookup::Hit(shared.clone())
                } else if inner.entries.len() >= MEMO_MAX_ENTRIES {
                    Lookup::Bypass
                } else {
                    // Insert before awaiting, so concurrent askers coalesce
                    // onto this execution rather than starting their own.
                    let shared = run_query(ctx.clone(), query.clone()).boxed().shared();
                    inner.entries.insert(key, shared.clone());
                    Lookup::Miss(shared)
                }
            },
            // A query that cannot be serialized cannot be keyed; run it
            // uncached. (Cannot actually happen for a `Query` that
            // deserialized off the wire.)
            Err(_) => Lookup::Bypass,
        };

        #[cfg(feature = "metrics")]
        counter!("ws.query_memo.lookups.total", "result" => match &lookup {
            Lookup::Hit(_) => "hit",
            Lookup::Miss(_) => "miss",
            Lookup::Bypass => "bypass",
        })
        .increment(1);

        match lookup {
            Lookup::Hit(shared) | Lookup::Miss(shared) => shared.await,
            Lookup::Bypass => run_query(ctx, query).await,
        }
    }
}

/// Register the memo's lookup counter. An idempotent, describe-only call,
/// invoked once at server startup alongside the other metric registrations.
#[cfg(feature = "metrics")]
pub fn init_query_memo_metrics() {
    describe_counter!(
        "ws.query_memo.lookups.total",
        "Query-memo lookups by result: hit (served from a shared execution), \
         miss (started the execution others share), bypass (ran uncached — \
         stale trigger, full map, or unkeyable query)"
    );
}

/// Execute the query against the app and shape the outcome into the shared
/// frame/error form. The frame reports the height the app actually served —
/// which can exceed the trigger height, if a newer block committed between the
/// trigger and the execution.
async fn run_query(ctx: MinimalContext, query: Query) -> Result<Arc<QueryFrame>, String> {
    ctx.dango_app
        .query_app(query)
        .await
        .map(|(response, block_height)| {
            Arc::new(QueryFrame {
                block_height,
                response,
            })
        })
        .map_err(|err| err.to_string())
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::traits::QueryApp,
        async_trait::async_trait,
        dango_app::{AppError, AppResult},
        dango_primitives::{BlockInfo, Hash256, QueryCodeRequest, TxOutcome, UnsignedTx},
        futures_util::join,
        std::sync::atomic::{AtomicUsize, Ordering},
    };

    /// A `QueryApp` that counts `query_app` executions and answers every query
    /// with `WasmRaw(None)` at a fixed height (or a fixed error).
    struct MockApp {
        calls: AtomicUsize,
        height: u64,
        fail: bool,
    }

    impl MockApp {
        fn context(height: u64, fail: bool) -> (MinimalContext, Arc<Self>) {
            let app = Arc::new(Self {
                calls: AtomicUsize::new(0),
                height,
                fail,
            });

            (MinimalContext::new(app.clone()), app)
        }
    }

    #[async_trait]
    impl QueryApp for MockApp {
        async fn query_app(&self, _raw_req: Query) -> AppResult<(QueryResponse, u64)> {
            self.calls.fetch_add(1, Ordering::SeqCst);

            if self.fail {
                return Err(AppError::vm("boom".to_string()));
            }

            Ok((QueryResponse::WasmRaw(None), self.height))
        }

        async fn simulate(&self, _unsigned_tx: UnsignedTx) -> AppResult<TxOutcome> {
            unimplemented!("not exercised by the memo");
        }

        async fn chain_id(&self) -> AppResult<String> {
            unimplemented!("not exercised by the memo");
        }

        async fn last_finalized_block(&self) -> AppResult<BlockInfo> {
            unimplemented!("not exercised by the memo");
        }
    }

    /// A distinct query per `seed`, for exercising the per-query keying.
    fn query(seed: u8) -> Query {
        Query::Code(QueryCodeRequest {
            hash: Hash256::from_inner([seed; 32]),
        })
    }

    #[test]
    fn coalesces_identical_queries_at_same_height() {
        let (ctx, app) = MockApp::context(5, false);
        let memo = QueryMemo::new();

        let (a, b, c) = futures::executor::block_on(async {
            join!(
                memo.query_at(5, query(1), ctx.clone()),
                memo.query_at(5, query(1), ctx.clone()),
                memo.query_at(5, query(1), ctx.clone()),
            )
        });

        assert_eq!(app.calls.load(Ordering::SeqCst), 1);

        // All askers share the very same response allocation.
        let (a, b, c) = (a.unwrap(), b.unwrap(), c.unwrap());
        assert!(Arc::ptr_eq(&a, &b));
        assert!(Arc::ptr_eq(&b, &c));
        assert_eq!(a.block_height, 5);
    }

    #[test]
    fn distinct_queries_run_separately_and_both_cache() {
        let (ctx, app) = MockApp::context(5, false);
        let memo = QueryMemo::new();

        futures::executor::block_on(async {
            memo.query_at(5, query(1), ctx.clone()).await.unwrap();
            memo.query_at(5, query(2), ctx.clone()).await.unwrap();
            // Repeats hit the cache.
            memo.query_at(5, query(1), ctx.clone()).await.unwrap();
            memo.query_at(5, query(2), ctx.clone()).await.unwrap();
        });

        assert_eq!(app.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn newer_height_clears_and_reruns() {
        let (ctx, app) = MockApp::context(5, false);
        let memo = QueryMemo::new();

        futures::executor::block_on(async {
            memo.query_at(5, query(1), ctx.clone()).await.unwrap();
            memo.query_at(6, query(1), ctx.clone()).await.unwrap();
        });

        assert_eq!(app.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn stale_trigger_bypasses_memo_without_polluting_it() {
        let (ctx, app) = MockApp::context(6, false);
        let memo = QueryMemo::new();

        futures::executor::block_on(async {
            // Generation advances to 6.
            memo.query_at(6, query(1), ctx.clone()).await.unwrap();

            // A caller skewed behind (trigger 5) runs uncached: it neither
            // reads the generation-6 entry nor inserts one of its own.
            memo.query_at(5, query(2), ctx.clone()).await.unwrap();
            memo.query_at(5, query(2), ctx.clone()).await.unwrap();

            // The generation-6 entry is untouched and still hits.
            memo.query_at(6, query(1), ctx.clone()).await.unwrap();
        });

        assert_eq!(app.calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn cap_falls_back_to_uncached() {
        let (ctx, app) = MockApp::context(5, false);
        let memo = QueryMemo::new();

        futures::executor::block_on(async {
            // Fill the map to the cap, with multi-byte seeds that can never
            // collide with the repeated-single-byte seeds `query()` produces.
            for i in 0..MEMO_MAX_ENTRIES {
                let mut bytes = [0u8; 32];
                bytes[0] = (i / 256) as u8;
                bytes[1] = (i % 256) as u8;
                bytes[2] = 0xff;
                memo.query_at(
                    5,
                    Query::Code(QueryCodeRequest {
                        hash: Hash256::from_inner(bytes),
                    }),
                    ctx.clone(),
                )
                .await
                .unwrap();
            }
            assert_eq!(app.calls.load(Ordering::SeqCst), MEMO_MAX_ENTRIES);

            // Beyond the cap, a distinct query runs uncached — twice for two
            // asks — while an already-cached query still hits.
            memo.query_at(5, query(1), ctx.clone()).await.unwrap();
            memo.query_at(5, query(1), ctx.clone()).await.unwrap();
            assert_eq!(app.calls.load(Ordering::SeqCst), MEMO_MAX_ENTRIES + 2);
        });
    }

    #[test]
    fn errors_are_memoized() {
        let (ctx, app) = MockApp::context(5, true);
        let memo = QueryMemo::new();

        let (a, b) = futures::executor::block_on(async {
            join!(
                memo.query_at(5, query(1), ctx.clone()),
                memo.query_at(5, query(1), ctx.clone()),
            )
        });

        assert_eq!(app.calls.load(Ordering::SeqCst), 1);
        assert_eq!(a.unwrap_err(), b.unwrap_err());
    }
}
