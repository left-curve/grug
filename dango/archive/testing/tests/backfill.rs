//! Production-path backfill → live → full query coverage.
//!
//! Drive the mock node to 100 blocks BEFORE starting the indexer, so the
//! `RemoteBlockSource` connects mid-history exactly like a production cold
//! start. The live tail (`since=None`) streams only blocks *newer* than the
//! tip it observes, so the fetcher must backfill the whole history below through
//! the REST `/block/full/range` API. The chain must keep producing for the live
//! tail to observe a tip at all — so we pump blocks until the backfill catches
//! up (the "poi continua a farne fare" part), then exercise every REST feed.
//!
//! Assertions survive two unknowns: the genesis block-height offset (counts are
//! of *broadcast* txs, not absolute heights) and gas mechanics. The test preset
//! runs with a zero gas-fee rate, so no fee transfers fire and event volumes are
//! exact — one `Transfer` + two bank contract-events (`sent`/`received`) per
//! transfer — each cross-checked against its per-name totals and printed as a
//! diagnostic. `user4` is a recipient that never sends, keeping its per-address
//! assertions gas-neutral.

use {
    dango_archive_testing::{PendingEnv, broadcast_transfer},
    dango_primitives::{Addr, FlatCategory},
    sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement},
    std::time::Duration,
};

/// Bulk transfers before the indexer starts (+1 marker = 100 pre-blocks).
const PRE_BULK: usize = 99;
const WAIT: Duration = Duration::from_secs(90);

#[tokio::test(flavor = "multi_thread")]
async fn backfill_then_live_then_query_coverage() {
    let mut pending = PendingEnv::setup().await.expect("set up mock node + db");

    // Addresses read by reference (no partial move of `pending.accounts`).
    let user1 = pending.accounts.user1.address.inner().to_string();
    let user3 = pending.accounts.user3.address.inner().to_string();
    let user4 = pending.accounts.user4.address.inner().to_string();
    let addr_user2 = *pending.accounts.user2.address.inner();
    let addr_user4 = *pending.accounts.user4.address.inner();
    let bank = pending.contracts.bank.to_string();

    // ---- produce 100 blocks BEFORE the indexer ----
    // Block 1: the MARKER, user3 -> user4. A discriminator that appears nowhere
    // else and is the lowest block — only the fetcher can reach it.
    let marker_hash = broadcast_transfer(
        &pending.client,
        &mut pending.accounts.user3,
        addr_user4,
        777,
    )
    .await
    .expect("broadcast the marker");
    // Blocks 2..=100: bulk user1 -> user2.
    let mut last_pre_hash = marker_hash;
    for _ in 0..PRE_BULK {
        last_pre_hash = broadcast_transfer(
            &pending.client,
            &mut pending.accounts.user1,
            addr_user2,
            100,
        )
        .await
        .expect("broadcast a bulk transfer");
    }

    // ---- start the indexer; it connects at the tip and backfills below ----
    let mut env = pending.start_indexer().await.expect("start the indexer");

    // Keep producing — the live tail (`since=None`) only advances on NEW blocks,
    // and the healer backfills everything below the tip the live tail observes.
    // Pump user1 -> user2 blocks until the last pre-indexer block is caught up:
    // that means the fetcher backfilled the whole history below the tip (none of
    // the 100 pre-blocks could have arrived live — they predate the
    // subscription). Track the live count so the assertions stay exact.
    let last_pre = last_pre_hash.to_string();
    let deadline = std::time::Instant::now() + WAIT;
    let mut live_hashes = Vec::new();
    loop {
        live_hashes.push(
            broadcast_transfer(&env.client, &mut env.accounts.user1, addr_user2, 100)
                .await
                .expect("broadcast a live transfer"),
        );
        if env.is_tx_indexed(&last_pre).await.expect("index check") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "backfill did not catch up within {WAIT:?}",
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let live_count = live_hashes.len();
    let total = 100 + live_count;

    // The live tail caught up to the latest block it produced — every live block
    // above the backfilled history is indexed too.
    let last_live = live_hashes.last().unwrap().to_string();
    env.wait_for_tx_indexed(&last_live, WAIT)
        .await
        .expect("live tail to reach the latest block");

    // The marker (block 1) is indexed — concrete proof the fetcher reached the
    // very bottom of the history (the live tail never saw blocks <= the tip).
    let marker = env
        .wait_for_tx_indexed(&marker_hash.to_string(), WAIT)
        .await
        .expect("fetcher to reach the marker (bottom) block");
    let marker_node = &marker[0];
    let marker_height = marker_node["blockHeight"].as_u64().expect("marker height");
    assert_eq!(marker_node["sender"].as_str().unwrap(), user3);
    assert!(marker_node["success"].as_bool().unwrap());

    // Precondition for the exact `count(*)` below: the window must stay below
    // the first perps cronjob. The preset schedules it every minute and blocks
    // are 250 ms apart, so the first cron unit lands at ~block 240; from there
    // every block adds a `kind=Cron` row that would inflate the count. The live
    // pump is deadline-bounded, not block-bounded, so assert zero cron units
    // explicitly — a degraded run that pumps past block 240 then fails here with
    // a clear cause instead of a confusing off-by-N count mismatch.
    assert_eq!(
        count(
            env.conn(),
            &format!(
                "SELECT count(*) AS n FROM activity_transactions WHERE kind = {}",
                FlatCategory::Cron as i16
            ),
        )
        .await,
        0,
        "no cron units expected below ~block 240 (saw {total} blocks)",
    );

    // Complete, contiguous catch-up: one tx per block, no gaps, no crons, and
    // the marker sits at the very bottom of the range.
    assert_eq!(
        count(
            env.conn(),
            "SELECT count(*) AS n FROM activity_transactions"
        )
        .await,
        total as i64,
        "every broadcast block is indexed exactly once",
    );
    assert_eq!(
        count(
            env.conn(),
            "SELECT count(DISTINCT block_height) AS n FROM activity_transactions",
        )
        .await,
        total as i64,
        "exactly one tx per block",
    );
    assert_eq!(
        count(
            env.conn(),
            "SELECT max(block_height) - min(block_height) + 1 AS n FROM activity_transactions",
        )
        .await,
        total as i64,
        "the indexed block range is contiguous (no gaps)",
    );
    assert_eq!(
        marker_height,
        count(
            env.conn(),
            "SELECT min(block_height) AS n FROM activity_transactions"
        )
        .await as u64,
        "the marker is the lowest indexed block",
    );
    eprintln!("pumped {live_count} live blocks; {total} blocks indexed total");

    // ===== query coverage =====

    // ---- transactionsInvolving ----
    // user1 sent every bulk + live transfer, and nothing else.
    let sent = env
        .collect_heights(&format!("/transactions/involving/{user1}?role=sender"))
        .await
        .unwrap();
    assert_eq!(
        sent.len(),
        PRE_BULK + live_count,
        "user1 sent every non-marker transfer"
    );
    assert!(
        strictly_descending(&sent),
        "transaction feeds are newest-first"
    );

    // user3 sent only the marker.
    let s3 = env
        .collect_heights(&format!("/transactions/involving/{user3}?role=sender"))
        .await
        .unwrap();
    assert_eq!(s3, vec![marker_height], "user3 sent only the marker");

    // user4 sent nothing.
    let s4 = env
        .collect_heights(&format!("/transactions/involving/{user4}?role=sender"))
        .await
        .unwrap();
    assert!(s4.is_empty(), "user4 never sent a tx");

    // user4 participates only in the marker (as recipient).
    let p4 = env
        .collect_heights(&format!("/transactions/involving/{user4}?role=participant"))
        .await
        .unwrap();
    assert_eq!(
        p4,
        vec![marker_height],
        "user4 participates only in the marker"
    );

    // kind: cron → none in this window (the exclusion case).
    let crons = env
        .collect_heights(&format!("/transactions/involving/{user1}?kind=cron"))
        .await
        .unwrap();
    assert!(crons.is_empty(), "no cron units in this window");

    // kind: transaction → all of user1's units.
    let txs = env
        .collect_heights(&format!("/transactions/involving/{user1}?kind=transaction"))
        .await
        .unwrap();
    assert_eq!(txs.len(), PRE_BULK + live_count);

    // explicit first-page pagination shape.
    let page = env
        .get(&format!(
            "/transactions/involving/{user1}?role=sender&first=50"
        ))
        .await
        .unwrap();
    assert_eq!(
        page["items"].as_array().unwrap().len(),
        50,
        "a page is capped at 50",
    );
    assert!(page["pageInfo"]["hasNextPage"].as_bool().unwrap());

    // ---- transactionsByHash ----
    let by_hash = env
        .get(&format!("/transactions/{marker_hash}"))
        .await
        .unwrap();
    assert_eq!(by_hash.as_array().unwrap().len(), 1);
    // an unknown hash maps to nothing.
    let unknown_hash = "0".repeat(64);
    let none = env
        .get(&format!("/transactions/{unknown_hash}"))
        .await
        .unwrap();
    assert!(none.as_array().unwrap().is_empty());

    // ---- eventsByType ----
    let transfers = env.collect_heights("/events?type=transfer").await.unwrap();
    let contract_events = env
        .collect_heights("/events?type=contract_event")
        .await
        .unwrap();
    eprintln!(
        "eventsByType: TRANSFER={} CONTRACT_EVENT={}",
        transfers.len(),
        contract_events.len()
    );
    assert_eq!(
        transfers.len(),
        total,
        "exactly one Transfer event per transfer"
    );
    assert!(non_increasing(&transfers), "event feeds are newest-first");
    assert_eq!(
        contract_events.len(),
        2 * total,
        "exactly the bank `sent` + `received` per transfer",
    );
    assert!(non_increasing(&contract_events));

    // Multiple types in one call (the UNION-ALL path) — every transfer + every
    // bank contract-event, merged newest-first.
    let multi = env
        .collect_heights("/events?type=transfer,contract_event")
        .await
        .unwrap();
    assert_eq!(
        multi.len(),
        3 * total,
        "every Transfer + bank sent/received, across both types",
    );
    assert!(non_increasing(&multi));

    // Guardrail: `/events` with neither `type` nor `involved` has no index
    // anchor, so it is rejected (a 400) — never run as a full-table scan.
    assert!(
        env.get_opt("/events").await.is_err(),
        "/events without type or involved must be rejected",
    );

    // ---- eventsInvolving ----
    // user4 is a party to the marker's Transfer + bank `sent` + bank `received`.
    let inv4 = env
        .collect_heights(&format!("/events?involved={user4}"))
        .await
        .unwrap();
    assert_eq!(
        inv4,
        vec![marker_height; 3],
        "user4 is a party to exactly the marker's three events",
    );
    let inv4_transfer = env
        .collect_heights(&format!("/events?involved={user4}&type=transfer"))
        .await
        .unwrap();
    assert_eq!(inv4_transfer, vec![marker_height]);
    let inv4_contract = env
        .collect_heights(&format!("/events?involved={user4}&type=contract_event"))
        .await
        .unwrap();
    assert_eq!(
        inv4_contract,
        vec![marker_height; 2],
        "the marker's `sent` + `received`"
    );

    // ---- contractEvents (bank) ----
    let sent_events = env
        .collect_heights(&format!("/events/contract?contract={bank}&names=sent"))
        .await
        .unwrap();
    let received_events = env
        .collect_heights(&format!("/events/contract?contract={bank}&names=received"))
        .await
        .unwrap();
    eprintln!(
        "bank contractEvents: sent={} received={}",
        sent_events.len(),
        received_events.len()
    );
    assert_eq!(
        sent_events.len(),
        total,
        "exactly one bank `sent` per transfer"
    );
    assert_eq!(
        received_events.len(),
        total,
        "exactly one bank `received` per transfer"
    );
    // an unknown contract has no events.
    let unknown_contract = Addr::try_from(vec![0xCDu8; 20]).unwrap().to_string();
    let no_events = env
        .collect_heights(&format!("/events/contract?contract={unknown_contract}"))
        .await
        .unwrap();
    assert!(no_events.is_empty(), "an unknown contract has no events");

    // ---- contractEventsInvolving ----
    let ci4 = env
        .collect_heights(&format!("/events/contract?contract={bank}&user={user4}"))
        .await
        .unwrap();
    assert_eq!(
        ci4,
        vec![marker_height; 2],
        "user4's bank sent + received in the marker"
    );
    let ci4_received = env
        .collect_heights(&format!(
            "/events/contract?contract={bank}&user={user4}&names=received"
        ))
        .await
        .unwrap();
    assert_eq!(ci4_received, vec![marker_height]);
    let ci4_sent = env
        .collect_heights(&format!(
            "/events/contract?contract={bank}&user={user4}&names=sent"
        ))
        .await
        .unwrap();
    assert_eq!(ci4_sent, vec![marker_height]);

    // ---- perpsEvents (the injected-anchor shortcut) ----
    // The harness anchors the shortcut on the bank (see
    // `PendingEnv::start_indexer`), so every `/events/contract?contract={bank}`
    // read above must be reproducible on `/events/perps` verbatim — same feeds,
    // the contract argument pre-bound instead of caller-supplied.
    let pe_all = env.collect_heights("/events/perps").await.unwrap();
    assert_eq!(
        pe_all.len(),
        2 * total,
        "the unfiltered shortcut serves the anchor's whole feed (sent + received)",
    );
    assert!(non_increasing(&pe_all));
    let pe_sent = env
        .collect_heights("/events/perps?names=sent")
        .await
        .unwrap();
    assert_eq!(
        pe_sent, sent_events,
        "the shortcut's `names` filter matches the explicit route's",
    );
    let pe4 = env
        .collect_heights(&format!("/events/perps?user={user4}"))
        .await
        .unwrap();
    assert_eq!(
        pe4, ci4,
        "the shortcut's `user` filter matches the explicit route's",
    );
    let pe4_received = env
        .collect_heights(&format!("/events/perps?user={user4}&names=received"))
        .await
        .unwrap();
    assert_eq!(
        pe4_received, ci4_received,
        "the shortcut's combined `user` + `names` matches the explicit route's",
    );

    // ===== eager payload hydration — the read paths back to the store =====

    // GET /blocks/{height}: the full `{ block, outcome }` read straight
    // from the RocksDB store the fetcher populated, for the marker's own height.
    let block = env.get(&format!("/blocks/{marker_height}")).await.unwrap();
    assert_eq!(
        block["block"]["info"]["height"].as_u64(),
        Some(marker_height),
        "GET /blocks/{{height}} returns the stored block at that height",
    );
    assert!(
        block["outcome"].is_object(),
        "...with its execution outcome"
    );
    // a height the source does not hold → 404.
    let absent = env
        .get_opt(&format!("/blocks/{}", (total + 1000) as u64))
        .await
        .unwrap();
    assert!(absent.is_none(), "an un-ingested height is a 404");

    // GET /blocks/latest: the block at the contiguous frontier. The projection
    // has caught up to the last live block and the mock chain only produces on
    // broadcast, so the frontier — which always leads the projection — must sit
    // exactly at the tip: the marker (the lowest block) plus the `total`
    // contiguous blocks asserted above.
    let latest = env.get("/blocks/latest").await.unwrap();
    let latest_height = latest["block"]["info"]["height"]
        .as_u64()
        .expect("latest height");
    assert_eq!(
        latest_height,
        marker_height + total as u64 - 1,
        "GET /blocks/latest is the contiguous frontier (== the tip once caught up)",
    );
    assert!(
        latest["outcome"].is_object(),
        "...with the same {{ block, outcome }} shape as /blocks/{{height}}"
    );

    // ===== the API docs =====

    // The merged OpenAPI spec documents the core block routes and every
    // activity feed — including /events/perps, since this harness injects an
    // anchor (see `PendingEnv::start_indexer`).
    let spec = env.get("/openapi.json").await.unwrap();
    for path in [
        "/blocks/{height}",
        "/blocks/latest",
        "/up",
        "/transactions/{hash}",
        "/transactions/involving/{address}",
        "/events",
        "/events/contract",
        "/events/perps",
    ] {
        assert!(
            spec["paths"].get(path).is_some(),
            "the OpenAPI spec should document {path}",
        );
    }

    // The base path lands on Swagger UI (via the /docs/ redirect, which the
    // client follows).
    let docs = env.get_text("/").await.unwrap();
    assert!(
        docs.contains("swagger-ui"),
        "GET / should land on the Swagger UI page",
    );

    // Transaction.tx / Transaction.outcome: hydrated eagerly from the unit's
    // block (→ source.get → RocksDB).
    let hydrated = env
        .get(&format!("/transactions/{marker_hash}"))
        .await
        .unwrap();
    let unit = &hydrated[0];
    assert!(
        unit["tx"].is_object(),
        "tx is hydrated from the block store"
    );
    assert!(
        unit["outcome"]["transaction"].is_object(),
        "outcome is hydrated from the block store",
    );

    // Event.data: the priority Transfer payload, served from the inline
    // event_data join (decompressed) without a block load.
    let with_data = env
        .get(&format!("/events?involved={user4}&type=transfer&first=1"))
        .await
        .unwrap();
    let payload = &with_data["items"][0]["data"];
    assert!(
        !payload.is_null(),
        "the transfer event payload round-trips from event_data"
    );
}

/// Run a SQL query that returns a single `bigint` column aliased `n`.
async fn count(conn: &DatabaseConnection, sql: &str) -> i64 {
    let row = conn
        .query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .expect("query")
        .expect("one row");
    row.try_get::<i64>("", "n").expect("column n")
}

fn strictly_descending(v: &[u64]) -> bool {
    v.windows(2).all(|w| w[0] > w[1])
}

fn non_increasing(v: &[u64]) -> bool {
    v.windows(2).all(|w| w[0] >= w[1])
}
