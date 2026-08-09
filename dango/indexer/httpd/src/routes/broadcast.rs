use {
    crate::{context::FullContext, request_ip::RequesterIp},
    actix_web::{Error, HttpRequest, HttpResponse, error::ErrorInternalServerError, post, web},
    dango_primitives::Tx,
};

/// `POST /broadcast` — submit a signed `Tx` to the mempool. Returns a mempool
/// receipt (`BroadcastTxOutcome`), not block inclusion; a mempool-rejected tx
/// still returns `200` (its `check_tx.result` is an `Err`). Only a transport
/// failure to the consensus client returns `500`. Mirrors the GraphQL
/// `broadcastTxSync` mutation; both go through `crate::broadcast::broadcast_tx`.
#[utoipa::path(
    post,
    path = "/broadcast",
    tag = "chain",
    summary = "Broadcast a signed transaction",
    description = "Submit a signed `Tx` to the mempool. Returns a mempool \
                   receipt (`BroadcastTxOutcome`), not block inclusion; a \
                   mempool-rejected tx still returns `200` with its \
                   `check_tx.result` an `Err`. Mirrors the GraphQL \
                   `broadcastTxSync` mutation.",
    request_body(
        content = serde_json::Value,
        description = "A signed `Tx`",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "The mempool receipt (`BroadcastTxOutcome`)", body = serde_json::Value),
        (status = 500, description = "Transport failure to the consensus client"),
    ),
)]
#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
#[post("/broadcast")]
pub async fn broadcast(
    body: web::Json<Tx>,
    app_ctx: web::Data<FullContext>,
    req: HttpRequest,
) -> Result<HttpResponse, Error> {
    let details = RequesterIp::from_request(&req).into_http_request_details();

    let outcome = crate::broadcast::broadcast_tx(&app_ctx, &details, body.into_inner())
        .await
        .map_err(ErrorInternalServerError)?;

    Ok(HttpResponse::Ok().json(outcome))
}
