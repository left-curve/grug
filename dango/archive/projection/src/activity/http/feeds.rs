//! The activity feeds' **database layer** — the functions that run the eight
//! documented access paths (see `DESIGN.md` § Access paths) and return a page of
//! rows with their cheap columns. The raw-payload detail (`tx` / `outcome` /
//! `data`) is filled afterwards by [`super::hydrate`]; these functions touch
//! only Postgres, which keeps them testable without a block source.
//!
//! Eight feeds collapse into six functions by folding each "+ optional filter"
//! pair into one argument:
//!
//! | function | queries | filters |
//! |----------|---------|---------|
//! | `transactions_involving` | 1 | address (+ optional `role`, `kind`) |
//! | `events_by_type` | 2 | types (one or more) |
//! | `contract_events` | 3 / 4 | contract (+ optional `names`) |
//! | `events_involving` | 5 / 6 | address (+ optional `types`) |
//! | `contract_events_involving` | 7 / 8 | address + contract (+ optional `names`) |
//!
//! Each is newest-first, capped at [`MAX_LIMIT`](super::pagination::MAX_LIMIT),
//! and keyset-paginated. The SQL is hand-written to match the access paths
//! exactly (`DISTINCT ON` to collapse an event's participant rows, a
//! row-comparison keyset, an in-index `IN (…)` name list, the union of involved
//! ∪ sender for query 1) and parameterized through [`Binder`] so no argument is
//! ever interpolated raw. It references the tables by their real names —
//! `activity_transactions`, `activity_events`, `activity_event_data` — never the
//! bare `events` / `transactions`. The `feeds_execute_against_postgres`
//! integration test in [`super::super`] runs every feed against a real Postgres
//! so the SQL shape can never regress unnoticed.

use {
    super::types::{
        AddressRole, Event, EventRow, Transaction, UnitKind, event_from_row, transaction_from_model,
    },
    crate::{
        activity::{entity::transactions, event_type::EventType},
        metrics::timed_query,
    },
    dango_archive_httpd::{ApiError, Binder, Page, PageInfo, decode_after, page_limit, paginate},
    dango_primitives::{Addr, Hash256},
    sea_orm::{
        ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr,
        EntityTrait, FromQueryResult, QueryFilter, QueryOrder, Statement, TransactionTrait,
    },
    serde::{Deserialize, Serialize},
};

// ---- keyset cursors (the activity feeds' ordering tuples) ----

/// The keyset of an event feed: the event position, compared all-DESC.
#[derive(Serialize, Deserialize, Clone, Copy)]
struct EventCursor {
    block_height: i64,
    category: i16,
    category_index: i32,
    event_index: i32,
}

/// The keyset of `transactionsInvolving`: the unit position.
#[derive(Serialize, Deserialize, Clone, Copy)]
struct UnitCursor {
    block_height: i64,
    kind: i16,
    idx: i32,
}

/// The event-position ordering, newest-first — shared by every event feed so
/// the index serves it as a backward scan (no sort). The `DISTINCT ON` feeds
/// append `, address DESC` (the tiebreaker that picks each event's
/// representative row): it must be **DESC** too, so the whole `ORDER BY` is the
/// backward scan verbatim — `address ASC` would force an Incremental Sort
/// (verified via `EXPLAIN`; the address-led feeds need no tiebreaker, so they
/// stop here).
const POS_DESC: &str = "block_height DESC, category DESC, category_index DESC, event_index DESC";

/// Every transaction with a given content hash, newest-first — the un-paginated
/// lookup behind the detail view. The hash is **not unique** (see `DESIGN.md` §
/// Identity): identical tx bytes can be re-included in a later block, so one hash
/// may map to several units. Returns every match ordered by position DESC; empty
/// if none. Cron units carry no hash, so this only ever resolves transactions.
/// Served by the partial `(hash)` index.
///
/// **Intentionally un-capped.** A hash collision is only reachable by
/// re-submitting *byte-identical* tx bytes, so in practice a hash maps to a
/// handful of units. If a deployment ever observes *many* units sharing one
/// hash, add a `LIMIT` here — the partial `(hash)` index already serves it as a
/// bounded backward scan.
pub(crate) async fn transactions_by_hash(
    db: &DatabaseConnection,
    hash: Hash256,
) -> Result<Vec<Transaction>, ApiError> {
    let txn = read_txn(db).await?;
    let rows = timed_query(
        "transactions_by_hash",
        transactions::Entity::find()
            .filter(transactions::Column::Hash.eq(hash.as_ref().to_vec()))
            .order_by_desc(transactions::Column::BlockHeight)
            .order_by_desc(transactions::Column::Idx)
            .all(&txn),
    )
    .await?;
    txn.commit().await?;
    rows.into_iter().map(transaction_from_model).collect()
}

/// Query 1 — transactions (and cronjobs) **involving** an address: by default
/// the unit's `sender` *or* a party to one of the unit's events. The two sides
/// live in different tables (`transactions.sender` and the `events`
/// participation rows); they are unioned, deduped, and the newest N kept. `role`
/// narrows to one side (`sender` / `participant`); omitted ⇒ either. Cron units
/// (which have no sender) are included by default; `kind` narrows to
/// transactions or cronjobs only.
pub(crate) async fn transactions_involving(
    db: &DatabaseConnection,
    address: Addr,
    role: Option<AddressRole>,
    kind: Option<UnitKind>,
    first: Option<i32>,
    after: Option<String>,
) -> Result<Page<Transaction>, ApiError> {
    let limit = page_limit(first);
    let after = decode_after::<UnitCursor>(after)?;
    let fetch = limit + 1;

    // Which sides of the union to scan. `role` picks one; cron has no sender, so
    // a cron restriction drops the sender side regardless.
    let want_sender =
        !matches!(role, Some(AddressRole::Participant)) && !matches!(kind, Some(UnitKind::Cron));
    let want_participant = !matches!(role, Some(AddressRole::Sender));

    let mut binder = Binder::new();
    let address_ph = binder.bind(address.as_ref().to_vec());
    // The unit keyset is bound once and reused by whichever sides are scanned.
    let keyset = after.map(|c| {
        (
            binder.bind(c.block_height),
            binder.bind(c.kind),
            binder.bind(c.idx),
        )
    });

    let mut sides: Vec<String> = Vec::new();

    // Involved side: the distinct units X is a party to, from `events`.
    if want_participant {
        let kind_filter = match kind {
            Some(k) => format!(" AND category = {}", binder.bind(k.code())),
            None => String::new(),
        };
        let keyset_clause = match &keyset {
            Some((h, k, i)) => {
                format!(" AND (block_height, category, category_index) < ({h}, {k}, {i})")
            },
            None => String::new(),
        };
        sides.push(format!(
            "SELECT DISTINCT block_height, category, category_index FROM activity_events \
             WHERE address = {address_ph}{kind_filter}{keyset_clause} \
             ORDER BY block_height DESC, category DESC, category_index DESC LIMIT {fetch}"
        ));
    }

    // Sender side: the units X sent, from `transactions`. Only transactions carry
    // a sender (cron rows are NULL), so every row here is `kind = Tx` — a
    // constant. The `ORDER BY` therefore drops `kind` and matches the `(sender,
    // block_height, idx)` index verbatim (a clean backward scan); the keyset
    // still compares the full unit position so it stays consistent with the
    // cron-bearing involved side and the outer ordering.
    if want_sender {
        let keyset_clause = match &keyset {
            Some((h, k, i)) => format!(" AND (block_height, kind, idx) < ({h}, {k}, {i})"),
            None => String::new(),
        };
        sides.push(format!(
            "SELECT block_height, kind AS category, idx AS category_index FROM activity_transactions \
             WHERE sender = {address_ph}{keyset_clause} \
             ORDER BY block_height DESC, idx DESC LIMIT {fetch}"
        ));
    }

    // No side matches this (role, kind) combo — e.g. `sender` on cron-only.
    // Nothing can match, so skip the database and return an empty page.
    let rows = if sides.is_empty() {
        Vec::new()
    } else {
        // Merge (dedup is the UNION), join the unit rows, take the newest N. Each
        // arm carries its own `ORDER BY` / `LIMIT`, so it MUST be parenthesised —
        // Postgres rejects `SELECT … LIMIT n UNION …` without parens (syntax
        // error). A lone arm is just `(SELECT …)`.
        let units = sides
            .iter()
            .map(|side| format!("({side})"))
            .collect::<Vec<_>>()
            .join(" UNION ");
        let sql = format!(
            "WITH unit AS ({units}) \
             SELECT t.* FROM unit u JOIN activity_transactions t \
             ON t.block_height = u.block_height AND t.kind = u.category AND t.idx = u.category_index \
             ORDER BY t.block_height DESC, t.kind DESC, t.idx DESC LIMIT {fetch}"
        );
        let stmt = Statement::from_sql_and_values(DbBackend::Postgres, sql, binder.into_values());
        let txn = read_txn(db).await?;
        let rows = timed_query(
            "transactions_involving",
            transactions::Entity::find().from_raw_sql(stmt).all(&txn),
        )
        .await?;
        txn.commit().await?;
        rows
    };

    paginate(
        rows,
        limit,
        |m| UnitCursor {
            block_height: m.block_height,
            kind: m.kind,
            idx: m.idx,
        },
        transaction_from_model,
    )
}

/// Query 2 — events of one or more **types**, newest-first.
///
/// A single type is the clean `idx_type` backward scan (no sort). Several types
/// are a `UNION ALL` of per-type scans — each one its own index-anchored,
/// `DISTINCT ON` + `LIMIT`-bounded scan — merged and re-limited; an event has
/// exactly one type, so the branches never overlap. This keeps the cost at
/// `O(types · N)` (a bounded merge over at most `types · fetch` rows), never a
/// full-type top-N sort — the trap `event_type = ANY(...)` would fall into on
/// Postgres before it can return array-key results in index order.
pub(crate) async fn events_by_type(
    db: &DatabaseConnection,
    types: Vec<EventType>,
    first: Option<i32>,
    after: Option<String>,
) -> Result<Page<Event>, ApiError> {
    let limit = page_limit(first);
    let after = decode_after::<EventCursor>(after)?;
    let fetch = limit + 1;

    if types.is_empty() {
        // Defensive: the handler requires a type when there is no address anchor.
        return Ok(Page {
            items: Vec::new(),
            page_info: PageInfo {
                has_next_page: false,
                end_cursor: None,
            },
        });
    }

    let mut binder = Binder::new();
    // Bound once, shared by every per-type branch.
    let keyset = event_keyset(&mut binder, &after);

    let sql = match types.as_slice() {
        // One type: the clean single backward index scan.
        [ty] => single_type_scan(&mut binder, *ty, &keyset, fetch),
        // Several: each its own index-anchored, LIMIT-bounded scan, `UNION ALL`
        // (no cross-branch duplicates), then a bounded merge.
        many => {
            let branches = many
                .iter()
                .map(|ty| format!("({})", single_type_scan(&mut binder, *ty, &keyset, fetch)))
                .collect::<Vec<_>>()
                .join(" UNION ALL ");
            format!("SELECT * FROM ({branches}) u ORDER BY {POS_DESC} LIMIT {fetch}")
        },
    };
    run_event_feed(db, "events_by_type", sql, binder, limit).await
}

/// One type's `DISTINCT ON` backward index scan — a `UNION ALL` branch of
/// [`events_by_type`], or the whole query when there is a single type. `keyset`
/// is the shared `AND (position) < ($cursor)` clause (bound once).
fn single_type_scan(binder: &mut Binder, ty: EventType, keyset: &str, fetch: u64) -> String {
    let type_ph = binder.bind(ty.code());
    format!(
        "SELECT DISTINCT ON (block_height, category, category_index, event_index) * \
         FROM activity_events WHERE event_type = {type_ph}{keyset} \
         ORDER BY {POS_DESC}, address DESC LIMIT {fetch}"
    )
}

/// Queries 3 / 4 — contract events emitted by a contract, optionally narrowed to
/// a set of event names (a single value or a list). Serves both
/// `/events/contract` and the `/events/perps` shortcut (the same call with the
/// injected perps address).
pub(crate) async fn contract_events(
    db: &DatabaseConnection,
    contract: Addr,
    names: Option<Vec<String>>,
    first: Option<i32>,
    after: Option<String>,
) -> Result<Page<Event>, ApiError> {
    let limit = page_limit(first);
    let after = decode_after::<EventCursor>(after)?;
    let fetch = limit + 1;

    let mut binder = Binder::new();
    let contract_ph = binder.bind(contract.as_ref().to_vec());
    let names = name_filter(&mut binder, &names);
    let keyset = event_keyset(&mut binder, &after);
    let sql = format!(
        "SELECT DISTINCT ON (block_height, category, category_index, event_index) * \
         FROM activity_events WHERE contract = {contract_ph}{names}{keyset} \
         ORDER BY {POS_DESC}, address DESC LIMIT {fetch}"
    );
    run_event_feed(db, "contract_events", sql, binder, limit).await
}

/// Queries 5 / 6 — events **involving** an address, optionally narrowed to a set
/// of types. One row per event (X appears once per event), so no `DISTINCT`.
///
/// The query is anchored on `address` (the PK gives position order within an
/// address), so the type set is just a residual `event_type IN (…)` filter — no
/// sort whatever the list length, and bounded by the address's own event count.
pub(crate) async fn events_involving(
    db: &DatabaseConnection,
    address: Addr,
    types: Vec<EventType>,
    first: Option<i32>,
    after: Option<String>,
) -> Result<Page<Event>, ApiError> {
    let limit = page_limit(first);
    let after = decode_after::<EventCursor>(after)?;
    let fetch = limit + 1;

    let mut binder = Binder::new();
    let address_ph = binder.bind(address.as_ref().to_vec());
    let type_filter = if types.is_empty() {
        String::new()
    } else {
        let placeholders = types
            .iter()
            .map(|ty| binder.bind(ty.code()))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" AND event_type IN ({placeholders})")
    };
    let keyset = event_keyset(&mut binder, &after);
    let sql = format!(
        "SELECT * FROM activity_events WHERE address = {address_ph}{type_filter}{keyset} \
         ORDER BY {POS_DESC} LIMIT {fetch}"
    );
    run_event_feed(db, "events_involving", sql, binder, limit).await
}

/// Queries 7 / 8 — contract events of a contract **involving** an address,
/// optionally narrowed to a set of event names. Like [`contract_events`], also
/// serves the `/events/perps` shortcut.
pub(crate) async fn contract_events_involving(
    db: &DatabaseConnection,
    address: Addr,
    contract: Addr,
    names: Option<Vec<String>>,
    first: Option<i32>,
    after: Option<String>,
) -> Result<Page<Event>, ApiError> {
    let limit = page_limit(first);
    let after = decode_after::<EventCursor>(after)?;
    let fetch = limit + 1;

    let mut binder = Binder::new();
    let address_ph = binder.bind(address.as_ref().to_vec());
    let contract_ph = binder.bind(contract.as_ref().to_vec());
    let names = name_filter(&mut binder, &names);
    let keyset = event_keyset(&mut binder, &after);
    let sql = format!(
        "SELECT * FROM activity_events \
         WHERE address = {address_ph} AND contract = {contract_ph}{names}{keyset} \
         ORDER BY {POS_DESC} LIMIT {fetch}"
    );
    run_event_feed(db, "contract_events_involving", sql, binder, limit).await
}

// ---- shared feed plumbing ----

/// Per-statement timeout for the read feeds, set via `SET LOCAL` so it is scoped
/// to the feed's own transaction. Ingest and migrations share the pool but not
/// this path, so they are never capped (a blanket role/pool timeout would also
/// cut short a long index-building migration). A generous ceiling: the heaviest
/// legitimate feed — a 50-row page — returns well under two seconds, so this
/// only ever fires on a pathological plan, freeing the backend instead of
/// letting a runaway query pin a pool connection (a client disconnect does not
/// cancel a query already executing). Postgres-only, like every feed statement.
const READ_TIMEOUT_STATEMENT: &str = "SET LOCAL statement_timeout = '15s'";

/// Begin a read transaction with [`READ_TIMEOUT_STATEMENT`] applied. The caller
/// runs its query against the returned transaction and commits it; on any error
/// (a timeout included) the dropped transaction rolls back — harmless for a
/// read.
async fn read_txn(db: &DatabaseConnection) -> Result<DatabaseTransaction, DbErr> {
    let txn = db.begin().await?;
    txn.execute_unprepared(READ_TIMEOUT_STATEMENT).await?;
    Ok(txn)
}

/// Wrap a feed's event query so each row also carries its stored payload: the
/// inner query (already selected, ordered, and limited over `events`) becomes a
/// subquery, and `data` is pulled per row by a **correlated** lookup on
/// `event_data`'s primary key. A scalar subquery (not a join) guarantees a point
/// index probe per row no matter how large `event_data` grows — a plain `LEFT
/// JOIN` lets the planner pick a hash join that sequentially scans the whole
/// `event_data`. The inner `LIMIT` blocks subquery flattening, so the inner's
/// backward-scan (no-sort) plan is left untouched; `data` is `NULL` for
/// non-priority events not stored there (then hydrated from the block).
fn with_event_data(inner: &str) -> String {
    format!(
        "SELECT sub.*, \
         ( SELECT ed.data FROM activity_event_data ed \
           WHERE ed.block_height = sub.block_height AND ed.category = sub.category \
           AND ed.category_index = sub.category_index AND ed.event_index = sub.event_index ) AS data \
         FROM ({inner}) sub \
         ORDER BY sub.block_height DESC, sub.category DESC, sub.category_index DESC, \
         sub.event_index DESC"
    )
}

/// Run an event feed's statement and shape the rows into a page. Every feed's
/// inner query is wrapped by [`with_event_data`] and mapped to an [`EventRow`]
/// (the event columns plus the correlated priority payload), so all share one
/// cursor / node mapping.
async fn run_event_feed(
    db: &DatabaseConnection,
    query: &'static str,
    sql: String,
    binder: Binder,
    limit: u64,
) -> Result<Page<Event>, ApiError> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        with_event_data(&sql),
        binder.into_values(),
    );
    let txn = read_txn(db).await?;
    let rows = timed_query(query, EventRow::find_by_statement(stmt).all(&txn)).await?;
    txn.commit().await?;
    paginate(
        rows,
        limit,
        |m| EventCursor {
            block_height: m.block_height,
            category: m.category,
            category_index: m.category_index,
            event_index: m.event_index,
        },
        event_from_row,
    )
}

/// The keyset predicate for an event feed: `AND (position) < ($cursor)` (a
/// row-comparison the index serves directly), or empty for the first page.
fn event_keyset(binder: &mut Binder, after: &Option<EventCursor>) -> String {
    match after {
        Some(c) => format!(
            " AND (block_height, category, category_index, event_index) < ({}, {}, {}, {})",
            binder.bind(c.block_height),
            binder.bind(c.category),
            binder.bind(c.category_index),
            binder.bind(c.event_index),
        ),
        None => String::new(),
    }
}

/// The optional contract-event-name filter: `AND contract_event_name IN (…)`
/// bound in-index, or empty when no (non-empty) name list was given.
fn name_filter(binder: &mut Binder, names: &Option<Vec<String>>) -> String {
    match names {
        Some(list) if !list.is_empty() => {
            let placeholders = list
                .iter()
                .map(|name| binder.bind(name.clone()))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" AND contract_event_name IN ({placeholders})")
        },
        _ => String::new(),
    }
}

// ---- Postgres feed integration test ----
//
// The eight feeds are **hand-written** SQL run through a `Statement` tagged
// `DbBackend::Postgres` — `DISTINCT ON`, a `WITH … UNION` of two arms each
// carrying their own `ORDER BY … LIMIT`, row-comparison keysets, and a
// correlated `activity_event_data` sub-select. None of that executes in the
// SQLite round-trip test in `super::super` (which drives the *typed* sea-orm
// builder) — so a SQL-shape bug (a wrong table name, an unparenthesised `UNION`
// arm) compiles clean and only fails at runtime against Postgres. This test
// closes that gap: it runs **every feed** against a real Postgres and asserts
// the rows, so the class of bug that needs a live engine to surface is caught
// in CI.
//
// The feeds touch only Postgres (the eager `tx` / `outcome` / `data` hydration
// is a separate, source-driven step), so no block source is needed: every
// assertion below is on the cheap columns, plus the *priority* event payload
// the feed decodes straight from the joined `event_data` blob.
//
// Skipped (not failed) when no Postgres is reachable, so `cargo test` stays
// green on a bare machine; CI's backend job always provides one.
#[cfg(test)]
mod tests {
    use {
        super::{
            super::types::{AddressRole, Event, Transaction, UnitKind},
            contract_events, contract_events_involving, events_by_type, events_involving,
            transactions_by_hash, transactions_involving,
        },
        crate::activity::{
            compress_event,
            entity::{event_data, events, transactions},
            event_type::EventType,
            migrations,
        },
        dango_archive_httpd::Page,
        dango_primitives::{Addr, FlatCategory, FlatEvent, FlatEvtBackrun, Hash256},
        sea_orm::{ActiveValue::Set, ConnectionTrait, EntityTrait},
    };

    /// The `block_height`s of a page's transactions, in order.
    fn tx_heights(page: &Page<Transaction>) -> Vec<u64> {
        page.items.iter().map(|t| t.block_height).collect()
    }

    /// The `block_height`s of a page's events, in order.
    fn ev_heights(page: &Page<Event>) -> Vec<u64> {
        page.items.iter().map(|e| e.block_height).collect()
    }

    /// One `activity_transactions` row.
    fn tx_row(
        height: i64,
        idx: i32,
        kind: i16,
        hash: Option<Vec<u8>>,
        sender: Option<Vec<u8>>,
        success: bool,
    ) -> transactions::ActiveModel {
        transactions::ActiveModel {
            block_height: Set(height),
            idx: Set(idx),
            kind: Set(kind),
            hash: Set(hash),
            sender: Set(sender),
            success: Set(success),
            timestamp: Set(0),
        }
    }

    /// One `activity_events` row (an event × one participant).
    #[allow(clippy::too_many_arguments)]
    fn ev_row(
        address: Vec<u8>,
        height: i64,
        category: i16,
        category_index: i32,
        event_index: i32,
        event_type: i16,
        contract: Option<Vec<u8>>,
        name: Option<&str>,
    ) -> events::ActiveModel {
        events::ActiveModel {
            address: Set(address),
            block_height: Set(height),
            category: Set(category),
            category_index: Set(category_index),
            event_index: Set(event_index),
            event_type: Set(event_type),
            contract: Set(contract),
            contract_event_name: Set(name.map(str::to_string)),
        }
    }

    /// Connect to the `DATABASE_URL` Postgres (default the repo's local
    /// `grug_test`), carve out a throwaway schema for this test, and return a
    /// handle whose `search_path` is scoped to it. `None` — skip — when no
    /// Postgres is reachable.
    async fn pg_test_schema() -> Option<(sea_orm::DatabaseConnection, String)> {
        use sea_orm::{ConnectOptions, Database};

        let base_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres@localhost/grug_test".to_string());
        let schema = format!("hist_activity_it_{}", uuid::Uuid::new_v4().simple());

        let mut opt = ConnectOptions::new(base_url);
        opt.set_schema_search_path(schema.clone())
            .max_connections(1)
            .connect_timeout(std::time::Duration::from_secs(2))
            .acquire_timeout(std::time::Duration::from_secs(2))
            .sqlx_logging(false);

        let db = match Database::connect(opt).await {
            Ok(db) => db,
            Err(err) => {
                eprintln!("skipping Postgres feed test — no reachable DATABASE_URL ({err})");
                return None;
            },
        };
        db.execute_unprepared(&format!("CREATE SCHEMA IF NOT EXISTS \"{schema}\""))
            .await
            .expect("create throwaway schema");
        Some((db, schema))
    }

    #[tokio::test]
    async fn feeds_execute_against_postgres() {
        let Some((db, schema_name)) = pg_test_schema().await else {
            return;
        };

        // Apply the three migrations (tables + partial / fillfactor indexes)
        // into the throwaway schema.
        let manager = sea_orm_migration::SchemaManager::new(&db);
        for migration in migrations::migrations() {
            migration.up(&manager).await.expect("migration up");
        }

        // ---- fixture: addresses, hashes, codes ----
        let a = Addr::try_from(vec![0x0Au8; 20]).unwrap(); // the subject address
        let b = Addr::try_from(vec![0x0Bu8; 20]).unwrap(); // another sender
        let c = Addr::try_from(vec![0x0Cu8; 20]).unwrap(); // a co-participant
        let gateway = Addr::try_from(vec![0xAAu8; 20]).unwrap();
        let perps = Addr::try_from(vec![0xBBu8; 20]).unwrap();
        let h1 = Hash256::try_from(vec![0x11u8; 32]).unwrap(); // shared by two units
        let h2 = Hash256::try_from(vec![0x22u8; 32]).unwrap();
        let hx = Hash256::try_from(vec![0xFFu8; 32]).unwrap(); // matches nothing

        let tx = FlatCategory::Tx as i16;
        let cron = FlatCategory::Cron as i16;
        let ce = EventType::ContractEvent.code();
        let transfer = EventType::Transfer.code();

        // ---- seed units ----
        // #1 h100 tx: A is the SENDER.
        // #2 h90  tx: B sent it; A only PARTICIPATES (via an event).
        // #3 h80  cron: A participates; cron has no sender.
        // #4 h70  tx: same hash as #1 but a different block & sender — A is
        //         neither sender nor participant, so only `transactions_by_hash`
        //         (not `transactions_involving(A)`) sees it.
        transactions::Entity::insert_many([
            tx_row(
                100,
                0,
                tx,
                Some(h1.as_ref().to_vec()),
                Some(a.as_ref().to_vec()),
                true,
            ),
            tx_row(
                90,
                0,
                tx,
                Some(h2.as_ref().to_vec()),
                Some(b.as_ref().to_vec()),
                true,
            ),
            tx_row(80, 0, cron, None, None, true),
            tx_row(
                70,
                0,
                tx,
                Some(h1.as_ref().to_vec()),
                Some(b.as_ref().to_vec()),
                false,
            ),
        ])
        .exec_without_returning(&db)
        .await
        .unwrap();

        // ---- seed events (one row per event × participant) ----
        events::Entity::insert_many([
            // gateway "bridge" in unit #2, participant A (h90, eidx 2).
            ev_row(
                a.as_ref().to_vec(),
                90,
                tx,
                0,
                2,
                ce,
                Some(gateway.as_ref().to_vec()),
                Some("bridge"),
            ),
            // perps "order_filled" in unit #1, participants B and C (NOT A) —
            // two rows the DISTINCT-ON feeds must collapse to one event.
            ev_row(
                b.as_ref().to_vec(),
                100,
                tx,
                0,
                5,
                ce,
                Some(perps.as_ref().to_vec()),
                Some("order_filled"),
            ),
            ev_row(
                c.as_ref().to_vec(),
                100,
                tx,
                0,
                5,
                ce,
                Some(perps.as_ref().to_vec()),
                Some("order_filled"),
            ),
            // a Transfer in the cron unit #3, participant A (h80, eidx 0).
            ev_row(a.as_ref().to_vec(), 80, cron, 0, 0, transfer, None, None),
        ])
        .exec_without_returning(&db)
        .await
        .unwrap();

        // ---- seed the priority payload for the gateway event ----
        let payload = FlatEvent::Backrun(FlatEvtBackrun { sender: a });
        event_data::Entity::insert(event_data::ActiveModel {
            block_height: Set(90),
            category: Set(tx),
            category_index: Set(0),
            event_index: Set(2),
            data: Set(compress_event(&payload).unwrap()),
        })
        .exec_without_returning(&db)
        .await
        .unwrap();

        let addr_a = a;
        let addr_b = b;

        // ===== query 1: transactions_involving (the involved ∪ sender UNION) =====

        // Default (role omitted): the union of A-as-sender (#1) and
        // A-as-participant (#2 tx, #3 cron), newest-first, deduped. This is the
        // exact path the unparenthesised-`UNION` bug broke at runtime.
        let page = transactions_involving(&db, addr_a, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(tx_heights(&page), vec![100, 90, 80]);
        // The h80 unit is the cronjob.
        assert!(matches!(page.items[2].kind, UnitKind::Cron));

        // role: Sender → only the sender side (#1).
        let page = transactions_involving(&db, addr_a, Some(AddressRole::Sender), None, None, None)
            .await
            .unwrap();
        assert_eq!(tx_heights(&page), vec![100]);

        // role: Participant → only the involved side (#2, #3).
        let page = transactions_involving(
            &db,
            addr_a,
            Some(AddressRole::Participant),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(tx_heights(&page), vec![90, 80]);

        // kind: Cron → the sender side is dropped (cron has no sender); only the
        // cron unit #3.
        let page = transactions_involving(&db, addr_a, None, Some(UnitKind::Cron), None, None)
            .await
            .unwrap();
        assert_eq!(tx_heights(&page), vec![80]);

        // kind: Transaction → sender side (#1) + the tx participant side (#2).
        let page =
            transactions_involving(&db, addr_a, None, Some(UnitKind::Transaction), None, None)
                .await
                .unwrap();
        assert_eq!(tx_heights(&page), vec![100, 90]);

        // role: Sender + kind: Cron → no side can match; an empty page, no query.
        let page = transactions_involving(
            &db,
            addr_a,
            Some(AddressRole::Sender),
            Some(UnitKind::Cron),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(tx_heights(&page), Vec::<u64>::new());

        // Keyset pagination across the UNION: page the default feed 2 + 1.
        let page = transactions_involving(&db, addr_a, None, None, Some(2), None)
            .await
            .unwrap();
        assert_eq!(tx_heights(&page), vec![100, 90]);
        assert!(page.page_info.has_next_page);
        let cursor = page.page_info.end_cursor.clone().expect("endCursor");
        let page = transactions_involving(&db, addr_a, None, None, Some(2), Some(cursor))
            .await
            .unwrap();
        assert_eq!(tx_heights(&page), vec![80]);
        assert!(!page.page_info.has_next_page);

        // ===== transactions_by_hash (un-paginated; hash is not unique) =====

        // h1 maps to two units (#1 and #4), newest-first.
        let items = transactions_by_hash(&db, h1).await.unwrap();
        assert_eq!(
            items.iter().map(|t| t.block_height).collect::<Vec<_>>(),
            vec![100, 70]
        );
        // h2 maps to one.
        let items = transactions_by_hash(&db, h2).await.unwrap();
        assert_eq!(
            items.iter().map(|t| t.block_height).collect::<Vec<_>>(),
            vec![90]
        );
        // an unknown hash maps to none.
        let items = transactions_by_hash(&db, hx).await.unwrap();
        assert_eq!(
            items.iter().map(|t| t.block_height).collect::<Vec<_>>(),
            Vec::<u64>::new()
        );

        // ===== event feeds =====

        // by type — DISTINCT ON collapses the two order_filled participant rows
        // into one event; bridge + order_filled, newest-first.
        let page = events_by_type(&db, vec![EventType::ContractEvent], None, None)
            .await
            .unwrap();
        assert_eq!(ev_heights(&page), vec![100, 90]);

        let page = events_by_type(&db, vec![EventType::Transfer], None, None)
            .await
            .unwrap();
        assert_eq!(ev_heights(&page), vec![80]);

        // Multiple types — the `UNION ALL` branch: every event of either type,
        // merged newest-first (contract events at 100/90, the cron transfer at 80).
        let page = events_by_type(
            &db,
            vec![EventType::ContractEvent, EventType::Transfer],
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(ev_heights(&page), vec![100, 90, 80]);

        // involving an address (one row per event for A) — bridge + cron transfer.
        let page = events_involving(&db, addr_a, vec![], None, None)
            .await
            .unwrap();
        assert_eq!(ev_heights(&page), vec![90, 80]);

        let page = events_involving(&db, addr_a, vec![EventType::Transfer], None, None)
            .await
            .unwrap();
        assert_eq!(ev_heights(&page), vec![80]);

        // Multiple types — the residual `event_type IN (…)` filter: A's bridge
        // (ContractEvent, 90) + A's cron transfer (Transfer, 80).
        let page = events_involving(
            &db,
            addr_a,
            vec![EventType::Transfer, EventType::ContractEvent],
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(ev_heights(&page), vec![90, 80]);

        // by contract (DISTINCT ON), with and without a name filter.
        let page = contract_events(&db, gateway, None, None, None)
            .await
            .unwrap();
        assert_eq!(ev_heights(&page), vec![90]);
        let page = contract_events(&db, perps, None, None, None).await.unwrap();
        assert_eq!(ev_heights(&page), vec![100]);
        let page = contract_events(
            &db,
            perps,
            Some(vec!["order_filled".to_string()]),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(ev_heights(&page), vec![100]);
        let page = contract_events(
            &db,
            perps,
            Some(vec!["does_not_exist".to_string()]),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(ev_heights(&page), Vec::<u64>::new());

        // contract events involving an address.
        let page = contract_events_involving(&db, addr_a, gateway, None, None, None)
            .await
            .unwrap();
        assert_eq!(ev_heights(&page), vec![90]);
        let page = contract_events_involving(&db, addr_b, perps, None, None, None)
            .await
            .unwrap();
        assert_eq!(ev_heights(&page), vec![100]);
        // A did not participate in the perps event (only B and C).
        let page = contract_events_involving(&db, addr_a, perps, None, None, None)
            .await
            .unwrap();
        assert_eq!(ev_heights(&page), Vec::<u64>::new());

        // ===== event payload (the eager priority decode) =====

        // The gateway event is a priority type, so the feed pulls its blob
        // through the correlated `activity_event_data` sub-select and decodes it
        // inline — no block load. Round-trips the payload.
        let page = contract_events(&db, gateway, None, None, None)
            .await
            .unwrap();
        assert_eq!(ev_heights(&page), vec![90]);
        assert_eq!(
            page.items[0].data,
            Some(serde_json::to_value(&payload).unwrap()),
            "the priority payload should round-trip through the event_data join",
        );

        // ===== event-feed keyset pagination (the row-comparison cursor) =====

        let page = events_by_type(&db, vec![EventType::ContractEvent], Some(1), None)
            .await
            .unwrap();
        assert_eq!(ev_heights(&page), vec![100]);
        assert!(page.page_info.has_next_page);
        let cursor = page.page_info.end_cursor.clone().expect("endCursor");
        let page = events_by_type(&db, vec![EventType::ContractEvent], Some(1), Some(cursor))
            .await
            .unwrap();
        assert_eq!(ev_heights(&page), vec![90]);
        assert!(!page.page_info.has_next_page);

        // ---- teardown: drop the throwaway schema ----
        db.execute_unprepared(&format!("DROP SCHEMA IF EXISTS \"{schema_name}\" CASCADE"))
            .await
            .ok();
    }
}
