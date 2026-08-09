//! The activity projection's **REST read surface** — the layer that turns the
//! three storage tables into queryable feeds over plain HTTP:
//!
//! - [`feeds`] — the database layer: the functions running the documented access
//!   paths (see `DESIGN.md` § Access paths), returning a page of cheap columns;
//! - [`hydrate`] — eager raw-payload hydration (`tx` / `outcome` / `data`) from
//!   the unit's block, batched per page;
//! - [`services`] — the actix layer: one module per resource (`transaction`,
//!   `events`), each grouping its `#[get]`-routed handlers in a `web::scope`,
//!   plus [`scopes`](services::scopes) which gathers them for the app to mount;
//! - [`types`] — the type surface: the `Transaction` / `Event` objects and the
//!   `UnitKind` / `AddressRole` enums.
//!
//! The generic read plumbing — `ApiError`, the `Page` / `PageInfo` envelope,
//! `paginate`, `page_limit`, the opaque-cursor codec, and the SQL `Binder` —
//! lives in the `httpd` crate, shared across projections; this module only adds
//! the activity's own SQL, types, and cursor shapes.
//!
//! Only [`scopes`](services::scopes) and its docs counterpart
//! [`api_doc`](services::api_doc) escape the module; everything else is
//! plumbing the app and the httpd never name. The handlers reach the shared
//! Postgres pool and block source through actix app data, injected when the
//! httpd builds the server.

mod feeds;
mod hydrate;
mod services;
mod types;

pub(crate) use services::{api_doc, scopes};
