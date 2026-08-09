use {
    crate::{context::MinimalContext, graphql::query::core::CoreQuery},
    actix_web::{Error, HttpResponse, error::ErrorBadRequest, post, web},
    dango_primitives::Query,
};

/// `POST /query` — run a read-only query against the latest finalized state.
/// The body is a raw `Query` object; the response is the raw `QueryResponse`
/// (no GraphQL envelope). Mirrors the GraphQL `queryApp` query.
#[utoipa::path(
    post,
    path = "/query",
    tag = "chain",
    summary = "Raw state query",
    description = "Run a read-only query against the latest finalized state. \
                   The body is a raw grug `Query` object; the response is the \
                   raw `QueryResponse` (no GraphQL envelope). Mirrors the \
                   GraphQL `queryApp` query.",
    request_body(
        content = serde_json::Value,
        description = "A grug `Query` object, e.g. `{\"app_config\": {}}`",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "The raw `QueryResponse`", body = serde_json::Value),
        (status = 400, description = "Malformed body or failed query"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[post("/query")]
pub async fn query(
    body: web::Json<Query>,
    app_ctx: web::Data<MinimalContext>,
) -> Result<HttpResponse, Error> {
    let response = CoreQuery::_query_app(&app_ctx, body.into_inner())
        .await
        .map_err(|e| ErrorBadRequest(e.message))?
        .response;

    Ok(HttpResponse::Ok().json(response))
}
