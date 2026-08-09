#[cfg(feature = "metrics")]
use crate::metrics::GaugeGuard;
use {
    crate::{request_ip::RequesterIp, subscription_limiter::SubscriptionLimiter},
    actix_web::{HttpRequest, HttpResponse, Resource, web},
    async_graphql::{BatchResponse, Data, Response, Schema, ServerError},
    async_graphql_actix_web::{GraphQLBatchRequest, GraphQLResponse, GraphQLSubscription},
    std::time::Duration,
};

/// Per-request execution timeout for `graphql_index`, injected via `web::Data`.
#[derive(Clone, Copy)]
pub struct GraphqlRequestTimeout(pub Duration);

pub fn graphql_route<Q, M, S>() -> Resource
where
    Q: async_graphql::ObjectType + 'static,
    M: async_graphql::ObjectType + 'static,
    S: async_graphql::SubscriptionType + 'static,
{
    web::resource("/graphql")
        .route(web::post().to(graphql_index::<Q, M, S>))
        .route(
            web::get()
                .guard(actix_web::guard::Header("upgrade", "websocket"))
                .to(graphql_ws::<Q, M, S>),
        )
}

/// Doc-only stub carrying the OpenAPI path item for `POST /graphql` — never
/// mounted; the real route is the generic [`graphql_route`] resource, which
/// `#[utoipa::path]` cannot annotate.
#[utoipa::path(
    post,
    path = "/graphql",
    tag = "graphql",
    summary = "GraphQL endpoint (deprecated)",
    description = "**Deprecated — scheduled for removal.** Prefer the REST \
                   routes and `GET /ws`. Accepts a standard GraphQL-over-HTTP \
                   request or a batch of them; the same resource also serves \
                   legacy `graphql-ws` subscriptions on a GET upgrade \
                   (undocumented). No schema is documented here.",
    request_body(
        content = serde_json::Value,
        description = "A GraphQL request (`{\"query\": \"...\", \"variables\": {...}}`) \
                       or a batch of them",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "GraphQL response envelope (errors ride the envelope, \
                                      not the status)"),
    ),
)]
#[deprecated = "the GraphQL API is deprecated and will be removed"]
pub fn graphql_doc() {}

#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
pub async fn graphql_index<Q, M, S>(
    schema: web::Data<Schema<Q, M, S>>,
    timeout: web::Data<GraphqlRequestTimeout>,
    req: HttpRequest,
    gql_request: GraphQLBatchRequest,
) -> GraphQLResponse
where
    Q: async_graphql::ObjectType + 'static,
    M: async_graphql::ObjectType + 'static,
    S: async_graphql::SubscriptionType + 'static,
{
    let requester_ip = RequesterIp::from_request(&req);
    let details = requester_ip.clone().into_http_request_details();

    let request = gql_request.into_inner().data(details).data(requester_ip);

    // Bound non-subscription requests; subscriptions go through `graphql_ws`.
    let timeout_duration = timeout.0;
    match tokio::time::timeout(timeout_duration, schema.execute_batch(request)).await {
        Ok(response) => response.into(),
        Err(_) => {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                timeout_secs = timeout_duration.as_secs(),
                "graphql request timed out"
            );
            BatchResponse::Single(Response::from_errors(vec![ServerError::new(
                format!("request exceeded {}s timeout", timeout_duration.as_secs()),
                None,
            )]))
            .into()
        },
    }
}

#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
pub async fn graphql_ws<Q, M, S>(
    schema: web::Data<Schema<Q, M, S>>,
    req: HttpRequest,
    payload: web::Payload,
    global_limiter: web::Data<SubscriptionLimiter>,
) -> actix_web::Result<HttpResponse>
where
    Q: async_graphql::ObjectType + 'static,
    M: async_graphql::ObjectType + 'static,
    S: async_graphql::SubscriptionType + 'static,
{
    let mut subscription = GraphQLSubscription::new(Schema::clone(&*schema))
        .keepalive_timeout(Duration::from_secs(30));

    let mut data = Data::default();
    data.insert(global_limiter.new_connection());
    #[cfg(feature = "metrics")]
    data.insert(GaugeGuard::new(
        "graphql.websocket.connections.active",
        "graphql",
        "websocket",
    ));
    subscription = subscription.with_data(data);

    subscription.start(&req, payload)
}
