use {
    crate::{context::FullContext, request_ip::RequesterIp},
    actix_web::{
        Error, HttpRequest, HttpResponse, Responder, error::ErrorInternalServerError, get, web,
    },
    async_graphql::futures_util::TryFutureExt,
    chrono::{Duration, Utc},
    dango_indexer_sql::entity,
    dango_primitives::{BlockInfo, GIT_COMMIT},
    sea_orm::{EntityTrait, Order, QueryOrder},
    std::env::var,
};

/// `GET /` — the base path lands on the API docs. Liveness lives on `/up`.
#[get("/")]
pub async fn index() -> impl Responder {
    HttpResponse::Found()
        .insert_header(("location", "/docs/"))
        .finish()
}

#[utoipa::path(
    get,
    path = "/requester-ip",
    tag = "meta",
    summary = "Echo the caller's IP as the server resolves it",
    responses(
        (status = 200, description = "The remote / peer address and forwarding headers as \
                                      seen by the server", body = serde_json::Value),
    ),
)]
#[get("/requester-ip")]
pub async fn requester_ip(req: HttpRequest) -> impl Responder {
    HttpResponse::Ok().json(RequesterIp::from_request(&req))
}

#[derive(serde::Serialize)]
pub struct UpResponse<'a> {
    pub block: BlockInfo,
    pub is_running: bool,
    pub git_commit: &'a str,
    pub indexed_block_height: Option<u64>,
    pub chain_id: &'a str,
    pub hostname: &'a str,
}

#[utoipa::path(
    get,
    path = "/up",
    tag = "meta",
    summary = "Liveness and indexing status",
    description = "Proves the chain is answering queries and the indexer \
                   database is reachable. `is_running` is whether the latest \
                   finalized block is younger than 30 seconds; \
                   `indexed_block_height` is the highest block the indexer has \
                   written (`null` when none yet).",
    responses(
        (status = 200, description = "Chain and indexer status", body = serde_json::Value,
         example = json!({
             "block": {"height": 42, "timestamp": "2026-01-01T00:00:00Z", "hash": "..."},
             "is_running": true,
             "git_commit": "abc123",
             "indexed_block_height": 42,
             "chain_id": "dango-1",
             "hostname": "node-1"
         })),
        (status = 500, description = "The chain query or the indexer database read failed"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/up")]
pub async fn up(app_ctx: web::Data<FullContext>) -> Result<impl Responder, Error> {
    // This ensures that the chain is working
    let block = app_ctx
        .base
        .dango_app
        .last_finalized_block()
        .map_err(ErrorInternalServerError)
        .await?;

    let is_running =
        block.timestamp.to_naive_date_time() >= (Utc::now().naive_utc() - Duration::seconds(30));

    // This ensures than the database is up
    let indexed_block_height = entity::blocks::Entity::find()
        .order_by(entity::blocks::Column::BlockHeight, Order::Desc)
        .one(&app_ctx.db)
        .await
        .map_err(ErrorInternalServerError)?
        .map(|b| b.block_height as u64);

    Ok(HttpResponse::Ok().json(UpResponse {
        block,
        is_running,
        indexed_block_height,
        git_commit: GIT_COMMIT,
        chain_id: var("CHAIN_ID").unwrap_or_default().as_str(),
        hostname: var("HOSTNAME").unwrap_or_default().as_str(),
    }))
}

#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[get("/sentry-raise")]
pub async fn sentry_raise() -> Result<impl Responder, Error> {
    sentry::capture_message("Capturing a message before a crash", sentry::Level::Info);

    let err = "NaN".parse::<usize>().unwrap_err();
    sentry::capture_error(&err);

    Ok(HttpResponse::Ok().body("Sending a sentry crash"))
}

/// Custom 404 handler that serves an HTML page from
/// `{static_files_path}/404.html` when configured, falling back to a plain
/// text response otherwise.
pub async fn not_found_handler(app_ctx: web::Data<FullContext>) -> HttpResponse {
    let static_files_path = app_ctx.static_files_path.as_deref();

    if let Some(static_files_path) = static_files_path {
        let file_path = format!("{static_files_path}/404.html");
        if let Ok(html_content) = std::fs::read_to_string(&file_path) {
            return HttpResponse::NotFound()
                .content_type("text/html; charset=utf-8")
                .body(html_content);
        }
    }

    HttpResponse::NotFound()
        .content_type("text/plain; charset=utf-8")
        .body("404 Not Found")
}
