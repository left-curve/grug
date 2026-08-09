//! The `/transactions` read scope — the two transaction feeds, routed by
//! `#[get]` attribute macros.
//!
//! Each handler parses its path / query arguments (a malformed hash, address or
//! cursor is a 400), runs the matching feed in [`feeds`](super::super::feeds)
//! against the shared Postgres pool, hydrates the page's `tx` / `outcome` from
//! the shared block source ([`hydrate`](super::super::hydrate)), and answers with
//! the JSON page. The shared read handles come from actix app data (`web::Data`),
//! injected by the httpd.

use {
    super::super::{
        feeds, hydrate,
        types::{AddressRole, Transaction, UnitKind},
    },
    actix_web::{HttpResponse, Scope, get, web},
    dango_archive_block_source::BlockSource,
    dango_archive_httpd::{ApiError, Page},
    dango_primitives::{Addr, Hash256},
    sea_orm::DatabaseConnection,
    serde::Deserialize,
    std::sync::Arc,
    utoipa::{IntoParams, OpenApi},
};

/// The `/transactions` scope: the content-hash lookup and the "involving"
/// feed.
pub(crate) fn services() -> Scope {
    web::scope("/transactions")
        .service(by_hash)
        .service(involving)
}

/// The scope's OpenAPI fragment — the docs counterpart of [`services`].
pub(crate) fn api_doc() -> utoipa::openapi::OpenApi {
    #[derive(OpenApi)]
    #[openapi(paths(by_hash, involving))]
    struct Doc;
    Doc::openapi()
}

/// `transactionsInvolving` arguments.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct InvolvingQuery {
    /// How the address must relate to the unit: `sender` or `participant`.
    /// Omitted ⇒ either.
    role: Option<AddressRole>,
    /// Restrict to one kind of unit: `transaction` or `cron`. Omitted ⇒ both.
    kind: Option<UnitKind>,
    /// Page size (max 50; default 50).
    first: Option<i32>,
    /// Opaque cursor of the previous page (`pageInfo.endCursor`).
    after: Option<String>,
}

#[utoipa::path(
    get,
    path = "/transactions/{hash}",
    tag = "transactions",
    summary = "Transactions by content hash",
    description = "Every unit whose transaction bytes hash to `hash`, \
                   newest-first, un-paginated: the hash is **not** unique — \
                   byte-identical re-submissions can land in later blocks. Cron \
                   units carry no hash, so this only ever resolves transactions.",
    params(
        ("hash" = String, Path, description = "Transaction content hash (64 hex chars)"),
    ),
    responses(
        (status = 200, description = "Every matching unit, newest-first (empty if none)",
         body = Vec<Transaction>),
        (status = 400, description = "Malformed hash"),
    ),
)]
#[get("/{hash}")]
async fn by_hash(
    db: web::Data<DatabaseConnection>,
    source: web::Data<Arc<dyn BlockSource>>,
    hash: web::Path<Hash256>,
) -> Result<HttpResponse, ApiError> {
    let mut items = feeds::transactions_by_hash(&db, hash.into_inner()).await?;
    hydrate::hydrate_transactions(source.get_ref(), &mut items).await?;
    Ok(HttpResponse::Ok().json(items))
}

#[utoipa::path(
    get,
    path = "/transactions/involving/{address}",
    tag = "transactions",
    summary = "Transactions involving an address",
    description = "Units the address **sent** or **participated in** (a party \
                   to one of the unit's events) — the union by default, \
                   narrowed with `role` / `kind`. Newest-first, keyset-paginated.",
    params(
        ("address" = String, Path, description = "Account address (`0x` hex)"),
        InvolvingQuery,
    ),
    responses(
        (status = 200, description = "One page of units, newest-first",
         body = Page<Transaction>),
        (status = 400, description = "Malformed address, argument, or cursor"),
    ),
)]
#[get("/involving/{address}")]
async fn involving(
    db: web::Data<DatabaseConnection>,
    source: web::Data<Arc<dyn BlockSource>>,
    address: web::Path<Addr>,
    query: web::Query<InvolvingQuery>,
) -> Result<HttpResponse, ApiError> {
    let q = query.into_inner();
    let mut page =
        feeds::transactions_involving(&db, address.into_inner(), q.role, q.kind, q.first, q.after)
            .await?;
    hydrate::hydrate_transactions(source.get_ref(), &mut page.items).await?;
    Ok(HttpResponse::Ok().json(page))
}
