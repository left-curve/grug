//! The `/events` read scope — the three event feeds, routed by `#[get]`
//! attribute macros.
//!
//! - `GET /events` — events filtered by `type` (a comma-separated list) and/or
//!   `involved` (an address); **at least one** is required, since an unfiltered
//!   feed has no index anchor (it would be a full-table scan + sort → 400).
//! - `GET /events/contract` — the contract events (the event sub-type
//!   contracts emit) of one emitting contract, named by the mandatory
//!   `contract` query argument and optionally narrowed by `user` (a
//!   participant address) and `names` (a comma-separated list). The mandatory
//!   `contract` keeps every reachable combination index-anchored.
//! - `GET /events/perps` — the perps shortcut: exactly `/events/contract`
//!   with the contract pre-bound to the deployment's perps address (injected
//!   at construction, resolved by the cli from the node's `app_config`), so
//!   the dominant consumer never has to carry the address around. Same
//!   optional `user` / `names`, same feeds. Mounted only when an address was
//!   injected.
//!
//! Each handler parses its arguments (a malformed address / type / cursor is a
//! 400), runs the matching feed in [`feeds`](super::super::feeds), hydrates the
//! page's `data` from the shared block source ([`hydrate`](super::super::hydrate)),
//! and answers with the JSON page. The shared read handles come from actix app
//! data (`web::Data`), injected by the httpd; the perps anchor rides as
//! scope-local app data, attached exactly when the shortcut is mounted.

use {
    super::super::{feeds, hydrate, types::Event},
    crate::activity::event_type::EventType,
    actix_web::{HttpResponse, Scope, get, web},
    dango_archive_block_source::BlockSource,
    dango_archive_httpd::{ApiError, Page},
    dango_primitives::Addr,
    sea_orm::DatabaseConnection,
    serde::Deserialize,
    std::sync::Arc,
    utoipa::{IntoParams, OpenApi},
};

/// The injected perps contract's address — the pre-bound anchor of the
/// `/events/perps` shortcut, carried as scope-local app data.
#[derive(Clone, Copy)]
struct PerpsContract(Addr);

/// The `/events` scope: the by-type / involved feed, the contract-events
/// feed, and (when a perps address was injected) the `/events/perps` shortcut.
pub(crate) fn services(perps_contract: Option<Addr>) -> Vec<Scope> {
    let mut scope = web::scope("/events")
        .service(events)
        .service(contract_events);
    // Without an injected address the shortcut has no anchor, so the route is
    // simply not mounted (404) — the explicit `/events/contract` form still
    // serves everything.
    if let Some(contract) = perps_contract {
        scope = scope
            .app_data(web::Data::new(PerpsContract(contract)))
            .service(perps_events);
    }
    vec![scope]
}

/// The scope's OpenAPI fragment — the docs counterpart of [`services`], with
/// the same conditional: `/events/perps` is documented exactly when it is
/// mounted.
pub(crate) fn api_doc(perps_mounted: bool) -> utoipa::openapi::OpenApi {
    #[derive(OpenApi)]
    #[openapi(paths(events, contract_events))]
    struct Doc;

    #[derive(OpenApi)]
    #[openapi(paths(perps_events))]
    struct PerpsDoc;

    let mut doc = Doc::openapi();
    if perps_mounted {
        doc.merge(PerpsDoc::openapi());
    }
    doc
}

/// `/events` arguments — `type` (comma-separated list) and/or `involved`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct EventsQuery {
    /// Comma-separated list of event types (`transfer`, `contract_event`, …).
    /// Required when `involved` is absent.
    #[serde(rename = "type")]
    #[param(value_type = Option<String>)]
    ty: Option<String>,
    /// Participant address (`0x` hex): only events the address is a party to.
    #[param(value_type = Option<String>)]
    involved: Option<Addr>,
    /// Page size (max 50; default 50).
    first: Option<i32>,
    /// Opaque cursor of the previous page (`pageInfo.endCursor`).
    after: Option<String>,
}

/// `/events/contract` arguments — the mandatory emitting `contract`, plus the
/// same optional narrowing `/events/perps` takes.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct ContractEventsQuery {
    /// Emitting contract address (`0x` hex). Required — the feed's index
    /// anchor.
    #[param(value_type = String)]
    contract: Addr,

    /// Participant address (`0x` hex): only the contract's events this address
    /// is a party to.
    #[param(value_type = Option<String>)]
    user: Option<Addr>,

    /// Comma-separated list of contract-event names (`order_filled`, …).
    names: Option<String>,

    /// Page size (max 50; default 50).
    first: Option<i32>,

    /// Opaque cursor of the previous page (`pageInfo.endCursor`).
    after: Option<String>,
}

/// `/events/perps` arguments — the same narrowing as `/events/contract`,
/// minus `contract` itself (pre-bound to the injected perps address).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct PerpsEventsQuery {
    /// Participant address (`0x` hex): only the perps contract's events this
    /// address is a party to.
    #[param(value_type = Option<String>)]
    user: Option<Addr>,

    /// Comma-separated list of contract-event names (`order_filled`, …).
    names: Option<String>,

    /// Page size (max 50; default 50).
    first: Option<i32>,

    /// Opaque cursor of the previous page (`pageInfo.endCursor`).
    after: Option<String>,
}

#[utoipa::path(
    get,
    path = "/events",
    tag = "events",
    summary = "Events by type and/or involved address",
    description = "Events filtered by `type` (a comma-separated list) and/or \
                   `involved` (a participant address). **At least one is \
                   required** — an unfiltered feed has no index anchor. \
                   Newest-first, keyset-paginated.",
    params(EventsQuery),
    responses(
        (status = 200, description = "One page of events, newest-first", body = Page<Event>),
        (status = 400, description = "Neither `type` nor `involved` given, or a malformed argument / cursor"),
    ),
)]
#[get("")]
async fn events(
    db: web::Data<DatabaseConnection>,
    source: web::Data<Arc<dyn BlockSource>>,
    query: web::Query<EventsQuery>,
) -> Result<HttpResponse, ApiError> {
    let q = query.into_inner();
    let types = parse_types(q.ty)?;

    let mut page = match q.involved {
        // Address-anchored: the optional type set is a residual filter.
        Some(address) => feeds::events_involving(&db, address, types, q.first, q.after).await?,
        // No address anchor — the type set must anchor the query, so it is
        // required (an unfiltered feed would be a full-table scan + sort).
        None => {
            if types.is_empty() {
                return Err(ApiError::bad_request(
                    "`/events` requires at least one of `type` or `involved`",
                ));
            }
            feeds::events_by_type(&db, types, q.first, q.after).await?
        },
    };
    hydrate::hydrate_events(source.get_ref(), &mut page.items).await?;
    Ok(HttpResponse::Ok().json(page))
}

#[utoipa::path(
    get,
    path = "/events/contract",
    tag = "events",
    summary = "Contract events of an emitting contract",
    description = "The contract events — the event sub-type contracts emit — \
                   of one emitting contract (the mandatory `contract`), \
                   optionally narrowed to a participant (`user`) and/or a set \
                   of event names (`names`). Newest-first, keyset-paginated.",
    params(ContractEventsQuery),
    responses(
        (status = 200, description = "One page of events, newest-first", body = Page<Event>),
        (status = 400, description = "Missing or malformed `contract`, or a malformed argument / cursor"),
    ),
)]
#[get("/contract")]
async fn contract_events(
    db: web::Data<DatabaseConnection>,
    source: web::Data<Arc<dyn BlockSource>>,
    query: web::Query<ContractEventsQuery>,
) -> Result<HttpResponse, ApiError> {
    let q = query.into_inner();
    serve_contract_events(
        &db,
        source.get_ref(),
        q.contract,
        q.user,
        q.names,
        q.first,
        q.after,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/events/perps",
    tag = "events",
    summary = "Contract events of the perps contract",
    description = "Shortcut: exactly `/events/contract` with the contract \
                   pre-bound to the deployment's perps address (resolved from \
                   the node's `app_config` at startup). Same optional `user` / \
                   `names` filters, same feeds. Mounted only when the \
                   deployment resolved a perps address.",
    params(PerpsEventsQuery),
    responses(
        (status = 200, description = "One page of the perps contract's events, newest-first",
         body = Page<Event>),
        (status = 400, description = "Malformed argument or cursor"),
    ),
)]
#[get("/perps")]
async fn perps_events(
    db: web::Data<DatabaseConnection>,
    source: web::Data<Arc<dyn BlockSource>>,
    perps: web::Data<PerpsContract>,
    query: web::Query<PerpsEventsQuery>,
) -> Result<HttpResponse, ApiError> {
    let q = query.into_inner();
    serve_contract_events(
        &db,
        source.get_ref(),
        perps.0,
        q.user,
        q.names,
        q.first,
        q.after,
    )
    .await
}

/// Run the contract-events feeds for `contract` and answer with the hydrated
/// page — the shared body of `/events/contract` and the `/events/perps`
/// shortcut, which only differ in where the contract address comes from.
async fn serve_contract_events(
    db: &DatabaseConnection,
    source: &Arc<dyn BlockSource>,
    contract: Addr,
    user: Option<Addr>,
    names: Option<String>,
    first: Option<i32>,
    after: Option<String>,
) -> Result<HttpResponse, ApiError> {
    let names = parse_names(names);

    let mut page = match user {
        Some(address) => {
            feeds::contract_events_involving(db, address, contract, names, first, after).await?
        },
        None => feeds::contract_events(db, contract, names, first, after).await?,
    };
    hydrate::hydrate_events(source, &mut page.items).await?;
    Ok(HttpResponse::Ok().json(page))
}

// ---- argument parsing ----

/// Parse a comma-separated `type` argument into a **set** of [`EventType`]s; an
/// absent or empty argument yields no types. An unknown value is a 400.
///
/// Duplicates are dropped (first-seen order kept): a repeated type would
/// double-count an event in `events_by_type`'s `UNION ALL`, and a type set is
/// idempotent anyway.
fn parse_types(raw: Option<String>) -> Result<Vec<EventType>, ApiError> {
    let Some(list) = raw else {
        return Ok(Vec::new());
    };
    let mut types = list
        .split(',')
        .filter(|name| !name.is_empty())
        .map(parse_event_type)
        .collect::<Result<Vec<_>, _>>()?;

    let mut seen = std::collections::HashSet::new();
    types.retain(|ty| seen.insert(*ty));
    Ok(types)
}

/// Parse one [`EventType`] from its snake_case spelling (`transfer`,
/// `contract_event`, …); an unknown value is a 400.
fn parse_event_type(text: &str) -> Result<EventType, ApiError> {
    use serde::de::{
        IntoDeserializer,
        value::{Error as DeError, StrDeserializer},
    };
    let de: StrDeserializer<DeError> = text.into_deserializer();
    EventType::deserialize(de)
        .map_err(|_: DeError| ApiError::bad_request(format!("unknown event type: {text}")))
}

/// Split a comma-separated `names` argument into a list, dropping empties; an
/// absent argument means "no name filter".
fn parse_names(raw: Option<String>) -> Option<Vec<String>> {
    raw.map(|list| {
        list.split(',')
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect()
    })
}
