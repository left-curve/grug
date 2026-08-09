//! The Postgres test-database helper: it stands up a schema-isolated engine
//! (external `DATABASE_URL` or embedded) and runs the real committer + projection
//! migrations on it — the engine the hand-written feed SQL actually depends on.

use {
    dango_archive_projection::{ActivityProjection, Projection},
    dango_archive_testing::TestDb,
    sea_orm::{ConnectionTrait, DbBackend, Statement},
    std::sync::Arc,
};

/// Migrating the activity projection through the committer lands all of its
/// tables (and the cursor table) in the test's schema on a real Postgres.
#[tokio::test]
async fn migrations_apply_on_a_real_postgres() {
    let db = TestDb::setup().await.expect("set up the test database");

    let projections: Vec<Arc<dyn Projection>> = vec![Arc::new(ActivityProjection::default())];
    db.migrate(&projections).await.expect("run the migrations");

    // `to_regclass` resolves against the connection's `search_path` (this test's
    // schema) and is NULL when the relation is absent — so a `true` here proves
    // the committer + projection migrations really executed on Postgres.
    for table in [
        "activity_transactions",
        "activity_events",
        "activity_event_data",
        "projection_cursors",
    ] {
        let row = db
            .conn
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                format!("SELECT to_regclass('{table}') IS NOT NULL AS present"),
            ))
            .await
            .expect("query")
            .expect("one row");
        let present: bool = row.try_get("", "present").expect("present column");
        assert!(present, "table `{table}` should exist after migrate");
    }
}

/// Re-running the migrations is a no-op (the shared `seaql_migrations` history
/// skips already-applied versions), so boot is idempotent.
#[tokio::test]
async fn migrations_are_idempotent() {
    let db = TestDb::setup().await.expect("set up the test database");
    let projections: Vec<Arc<dyn Projection>> = vec![Arc::new(ActivityProjection::default())];

    db.migrate(&projections).await.expect("first migrate");
    db.migrate(&projections)
        .await
        .expect("second migrate is a no-op");
}
