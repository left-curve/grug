//! The Postgres connection the committer (cursors + projection tables) and the
//! read API share.

use {
    crate::config::PostgresConfig,
    anyhow::Context,
    sea_orm::{ConnectOptions, Database, DatabaseConnection},
};

/// Open the Postgres pool from config. The pool is `Clone` and shared: the
/// committer commits domain writes + cursor through it, and the read API runs
/// its table queries on it.
pub async fn connect(cfg: &PostgresConfig) -> anyhow::Result<DatabaseConnection> {
    let mut opt = ConnectOptions::new(cfg.url.clone());
    opt.max_connections(cfg.max_connections);
    Database::connect(opt)
        .await
        .context("failed to connect to Postgres")
}
