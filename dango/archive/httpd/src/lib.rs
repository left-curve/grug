//! HTTP front door for the archive.
//!
//! A deliberately small actix-web service: given the shared read handles (the
//! Postgres pool and the [`BlockSource`](dango_archive_block_source::BlockSource))
//! plus the projections' route registrars, it injects the handles as actix app
//! data, mounts the core `GET /blocks/{height}`, `GET /blocks/latest` (the
//! block at the source's contiguous frontier), and `GET /up` routes, applies
//! each projection's feeds, and serves. The merged OpenAPI document (its own
//! core paths + every projection's `api_doc()` fragment) is served at
//! `GET /openapi.json`, with Swagger UI on `GET /docs/` and the base `GET /`
//! redirecting there.
//!
//! - [`serve`] returns the boxed, supervised server task.
//! - [`Configurator`] is the route-mounting closure the app builds from its
//!   projections' `services()` scopes and hands to [`serve`].
//!
//! It also exposes the **read-API building blocks** every projection's feeds
//! share — [`ApiError`], the [`Page`] / [`PageInfo`] envelope, [`paginate`],
//! [`page_limit`], the opaque-cursor codec ([`decode_after`]), and the SQL
//! [`Binder`] — so a projection writes only its own SQL + cursor shape, not the
//! pagination plumbing.
//!
//! The crate is projection-agnostic — it never names a projection; the app
//! builds the configurator and hands the [`App`] the [`serve`] task, which it
//! supervises like any other.
//!
//! [`App`]: dango_archive_app::App

mod config;
mod error;
mod metrics;
mod read;
mod server;

pub use {
    crate::metrics::init_metrics,
    config::HttpdConfig,
    // Re-exported from the shared types crate (they moved there so clients can
    // deserialize the same envelope the handlers serialize); the projections
    // keep importing them from here.
    dango_archive_types::{Page, PageInfo},
    error::ApiError,
    read::{Binder, decode_after, page_limit, paginate},
    server::{Configurator, serve},
};
