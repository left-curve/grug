use {
    actix_codec::Framed,
    actix_http::{Request, ws},
    actix_service::IntoServiceFactory,
    actix_test::{Client, TestServer, read_body},
    actix_web::{
        App,
        body::MessageBody,
        dev::{AppConfig, ServiceFactory, ServiceRequest, ServiceResponse},
        middleware::{Compress, Logger},
        test::try_call_service,
        web::{self, ServiceConfig},
    },
    anyhow::{anyhow, bail, ensure},
    awc::BoxedSocket,
    core::str,
    dango_indexer_httpd::{
        context::FullContext, graphql::build_full_schema, routes::graphql::GraphqlRequestTimeout,
        server::config_app, subscription_limiter::SubscriptionLimiter,
    },
    futures_util::{sink::SinkExt, stream::StreamExt},
    sea_orm::sqlx::types::uuid,
    serde::{Deserialize, Serialize, de::DeserializeOwned},
    serde_json::json,
    std::{
        collections::HashMap,
        ops::{Deref, DerefMut},
        time::Instant,
    },
    tokio::time::{Duration, timeout},
};

const DEFAULT_TIMEOUT_SECONDS: u64 = 5;

#[derive(Clone, Serialize, Debug)]
pub struct GraphQLCustomRequest<'a> {
    pub name: &'a str,
    pub query: &'a str,
    pub variables: serde_json::Map<String, serde_json::Value>,
}

impl<'a> GraphQLCustomRequest<'a> {
    /// Creates a GraphQLCustomRequest from a typed QueryBody.
    ///
    /// The `field_name` should be the GraphQL field name in the subscription/query
    /// response (e.g., "accounts" for SubscribeAccounts, "trades" for SubscribeTrades).
    pub fn from_query_body<V: Serialize>(
        body: graphql_client::QueryBody<V>,
        field_name: &'a str,
    ) -> Self {
        let variables = serde_json::to_value(&body.variables)
            .expect("Failed to serialize variables")
            .as_object()
            .cloned()
            .unwrap_or_default();

        Self {
            name: field_name,
            query: body.query,
            variables,
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct GraphQLResponse {
    pub data: HashMap<String, serde_json::Value>,
    pub errors: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize, Debug)]
pub struct GraphQLSubscriptionResponse {
    pub id: String,
    pub payload: GraphQLResponse,
}

#[derive(Deserialize, Debug)]
pub struct GraphQLCustomResponse<R> {
    pub data: R,
    pub errors: Option<Vec<serde_json::Value>>,
}

impl<R> Deref for GraphQLCustomResponse<R> {
    type Target = R;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<R> DerefMut for GraphQLCustomResponse<R> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

pub fn build_app_service(
    app_ctx: FullContext,
) -> App<
    impl ServiceFactory<
        ServiceRequest,
        Response = ServiceResponse<impl MessageBody>,
        Config = (),
        InitError = (),
        Error = actix_web::Error,
    >,
> {
    let graphql_schema = build_full_schema(app_ctx.clone());

    build_actix_app(app_ctx, graphql_schema)
}

pub async fn call_batch_graphql<R, A, S, B>(
    app: A,
    requests_body: Vec<GraphQLCustomRequest<'_>>,
) -> anyhow::Result<Vec<GraphQLCustomResponse<R>>>
where
    R: DeserializeOwned,
    A: IntoServiceFactory<S, Request>,
    S: ServiceFactory<
            Request,
            Config = AppConfig,
            Response = ServiceResponse<B>,
            Error = actix_web::Error,
        >,
    S::InitError: std::fmt::Debug,
    B: MessageBody,
{
    let app = actix_web::test::init_service(app).await;

    // When I need to debug the request body
    // println!(
    //     "request_body: {}",
    //     serde_json::to_string_pretty(&requests_body).unwrap()
    // );

    let request = actix_web::test::TestRequest::post()
        .uri("/graphql")
        .set_json(&requests_body)
        .to_request();

    let graphql_response = actix_web::test::call_and_read_body(&app, request).await;

    // When I need to debug the response
    // println!("text response: \n{graphql_response:#?}");

    let graphql_responses: Vec<GraphQLResponse> = serde_json::from_slice(&graphql_response)
        .inspect_err(|err| {
            println!("Failed to parse GraphQL response: {err}");

            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&graphql_response) {
                println!("json: {json:#?}");
            } else {
                println!("graphql_response: {graphql_response:#?}");
            }
        })?;

    // When I need to debug the response
    // println!("GraphQLResponses: {:#?}", graphql_responses);

    graphql_responses
        .into_iter()
        .enumerate()
        .map(|(index, mut graphql_response)| {
            if let Some(data) = graphql_response.data.remove(requests_body[index].name) {
                Ok(GraphQLCustomResponse {
                    data: serde_json::from_value(data)?,
                    errors: graphql_response.errors,
                })
            } else {
                bail!("can't find {} in response", requests_body[index].name)
            }
        })
        .collect::<Result<Vec<_>, _>>()
}

pub async fn call_graphql<R, A, S, B>(
    app: A,
    request_body: GraphQLCustomRequest<'_>,
) -> anyhow::Result<GraphQLCustomResponse<R>>
where
    R: DeserializeOwned,
    A: IntoServiceFactory<S, Request>,
    S: ServiceFactory<
            Request,
            Config = AppConfig,
            Response = ServiceResponse<B>,
            Error = actix_web::Error,
        >,
    S::InitError: std::fmt::Debug,
    B: MessageBody,
{
    call_batch_graphql(app, vec![request_body])
        .await
        .and_then(|mut responses| responses.pop().ok_or_else(|| anyhow!("no response found")))
}

/// Helper function to make typed GraphQL queries in tests.
///
/// This takes a typed `QueryBody<V>` from graphql_client and returns
/// a typed `Response<R>`.
pub async fn call_graphql_query<V, R, A, S, B>(
    app: A,
    query_body: graphql_client::QueryBody<V>,
) -> anyhow::Result<graphql_client::Response<R>>
where
    V: Serialize,
    R: DeserializeOwned,
    A: IntoServiceFactory<S, Request>,
    S: ServiceFactory<
            Request,
            Config = AppConfig,
            Response = ServiceResponse<B>,
            Error = actix_web::Error,
        >,
    S::InitError: std::fmt::Debug,
    B: MessageBody,
{
    let app = actix_web::test::init_service(app).await;

    let request = actix_web::test::TestRequest::post()
        .uri("/graphql")
        .set_json(&query_body)
        .to_request();

    let response = actix_web::test::call_and_read_body(&app, request).await;
    let response: graphql_client::Response<R> = serde_json::from_slice(&response)?;

    Ok(response)
}

/// Convenience wrapper that builds the app from a `FullContext` and makes
/// a typed GraphQL query. Reduces boilerplate in tests that don't need
/// custom app configuration.
pub async fn call_graphql_query_with_context<V, R>(
    context: FullContext,
    query_body: graphql_client::QueryBody<V>,
) -> anyhow::Result<graphql_client::Response<R>>
where
    V: Serialize,
    R: DeserializeOwned,
{
    let app = build_app_service(context);
    call_graphql_query(app, query_body).await
}

/// Convenience wrapper that builds the app from a `FullContext` and makes a
/// REST `POST` request with a JSON body, deserializing the JSON response. Used
/// by tests for the `/query`, `/simulate`, and `/broadcast` endpoints.
pub async fn call_rest_post_with_context<B, R>(
    context: FullContext,
    uri: &str,
    body: &B,
    headers: &[(&str, &str)],
) -> anyhow::Result<R>
where
    B: Serialize,
    R: DeserializeOwned,
{
    let app = build_app_service(context);
    let app = actix_web::test::init_service(app).await;

    let mut request = actix_web::test::TestRequest::post()
        .uri(uri)
        .peer_addr("127.0.0.1:12345".parse().expect("valid socket address"));
    for (name, value) in headers {
        request = request.insert_header((*name, *value));
    }
    let request = request.set_json(body).to_request();

    let response = actix_web::test::call_and_read_body(&app, request).await;

    Ok(serde_json::from_slice(&response)?)
}

/// Helper function to make typed batched GraphQL queries in tests.
///
/// This takes a vector of typed `QueryBody<V>` from graphql_client and returns
/// a vector of typed `Response<R>`.
pub async fn call_batch_graphql_query<V, R, A, S, B>(
    app: A,
    query_bodies: Vec<graphql_client::QueryBody<V>>,
) -> anyhow::Result<Vec<graphql_client::Response<R>>>
where
    V: Serialize,
    R: DeserializeOwned,
    A: IntoServiceFactory<S, Request>,
    S: ServiceFactory<
            Request,
            Config = AppConfig,
            Response = ServiceResponse<B>,
            Error = actix_web::Error,
        >,
    S::InitError: std::fmt::Debug,
    B: MessageBody,
{
    let app = actix_web::test::init_service(app).await;

    let request = actix_web::test::TestRequest::post()
        .uri("/graphql")
        .set_json(&query_bodies)
        .to_request();

    let response = actix_web::test::call_and_read_body(&app, request).await;
    let responses: Vec<graphql_client::Response<R>> = serde_json::from_slice(&response)?;

    Ok(responses)
}

pub async fn call_api<R>(
    app: App<
        impl ServiceFactory<
            ServiceRequest,
            Response = ServiceResponse<impl MessageBody>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        > + 'static,
    >,
    uri: &str,
) -> anyhow::Result<R>
where
    R: DeserializeOwned,
{
    let app = actix_web::test::init_service(app).await;

    let request = actix_web::test::TestRequest::get().uri(uri).to_request();

    let res = try_call_service(&app, request)
        .await
        .map_err(|err| anyhow!("failed to call service: {err:?}"))?;

    let text_response = read_body(res).await;

    // When I need to debug the response
    // println!("text response: \n{:#?}", str::from_utf8(&text_response)?);

    Ok(serde_json::from_slice(&text_response)?)
}

pub async fn call_api_post<R, B>(
    app: App<
        impl ServiceFactory<
            ServiceRequest,
            Response = ServiceResponse<impl MessageBody>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        > + 'static,
    >,
    uri: &str,
    body: &B,
) -> anyhow::Result<R>
where
    R: DeserializeOwned,
    B: Serialize,
{
    let app = actix_web::test::init_service(app).await;

    let request = actix_web::test::TestRequest::post()
        .uri(uri)
        .set_json(body)
        .to_request();

    let res = try_call_service(&app, request)
        .await
        .map_err(|err| anyhow!("failed to call service: {err:?}"))?;

    let text_response = read_body(res).await;

    Ok(serde_json::from_slice(&text_response)?)
}

pub async fn call_api_with_headers<R>(
    app: App<
        impl ServiceFactory<
            ServiceRequest,
            Response = ServiceResponse<impl MessageBody>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        > + 'static,
    >,
    uri: &str,
    headers: &[(&str, &str)],
) -> anyhow::Result<R>
where
    R: DeserializeOwned,
{
    let app = actix_web::test::init_service(app).await;

    let mut request = actix_web::test::TestRequest::get()
        .uri(uri)
        .peer_addr("127.0.0.1:12345".parse().expect("valid socket address"));
    for (name, value) in headers {
        request = request.insert_header((*name, *value));
    }
    let request = request.to_request();

    let res = try_call_service(&app, request)
        .await
        .map_err(|err| anyhow!("failed to call service: {err:?}"))?;

    let text_response = read_body(res).await;

    Ok(serde_json::from_slice(&text_response)?)
}

pub async fn call_graphql_with_headers<R, A, S, B>(
    app: A,
    request_body: GraphQLCustomRequest<'_>,
    headers: &[(&str, &str)],
) -> anyhow::Result<GraphQLCustomResponse<R>>
where
    R: DeserializeOwned,
    A: IntoServiceFactory<S, Request>,
    S: ServiceFactory<
            Request,
            Config = AppConfig,
            Response = ServiceResponse<B>,
            Error = actix_web::Error,
        >,
    S::InitError: std::fmt::Debug,
    B: MessageBody,
{
    let app = actix_web::test::init_service(app).await;
    let field_name = request_body.name;

    let mut request = actix_web::test::TestRequest::post()
        .uri("/graphql")
        .peer_addr("127.0.0.1:12345".parse().expect("valid socket address"));
    for (name, value) in headers {
        request = request.insert_header((*name, *value));
    }
    let request = request.set_json(&request_body).to_request();

    let graphql_response = actix_web::test::call_and_read_body(&app, request).await;
    let mut graphql_response: GraphQLResponse = serde_json::from_slice(&graphql_response)?;

    let data = graphql_response
        .data
        .remove(field_name)
        .ok_or_else(|| anyhow!("can't find {field_name} in response"))?;

    Ok(GraphQLCustomResponse {
        data: serde_json::from_value(data)?,
        errors: graphql_response.errors,
    })
}

/// Calls a GraphQL subscription and returns a stream
pub async fn call_ws_graphql_stream<C, F, A, B>(
    context: C,
    app_builder: F,
    request_body: GraphQLCustomRequest<'_>,
) -> anyhow::Result<(
    TestServer,
    awc::ClientResponse,
    Framed<BoxedSocket, ws::Codec>,
)>
where
    C: Clone + Send + Sync + 'static,
    F: Fn(C) -> App<A> + Clone + Send + Sync + 'static,
    A: ServiceFactory<
            ServiceRequest,
            Response = ServiceResponse<B>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        > + 'static,
    B: MessageBody + 'static,
{
    let srv = actix_test::start(move || app_builder(context.clone()));

    let (ws, mut framed) = Client::new()
        .ws(srv.url("/graphql"))
        .header("sec-websocket-protocol", "graphql-transport-ws")
        .connect()
        .await
        .map_err(|e| anyhow!("failed to connect to websocket 1: {e}"))?;

    framed
        .send(ws::Message::Text(
            json!({"type": "connection_init", "payload": {}})
                .to_string()
                .into(),
        ))
        .await?;

    let res = timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECONDS), framed.next()).await;

    // Wait for connection_ack
    match res {
        Ok(Some(Ok(ws::Frame::Text(text)))) => {
            ensure!(
                text == json!({ "type": "connection_ack" }).to_string(),
                "unexpected connection response: {text:?}"
            );
        },
        Ok(Some(Err(e))) => return Err(e.into()),
        Ok(None) => bail!("connection closed unexpectedly"),
        Ok(_) => bail!("unexpected message type"),
        Err(_) => bail!("connection timed out"),
    }

    let request_id = uuid::Uuid::new_v4();
    let request_body_json = json!({
        "id": request_id,
        "type": "subscribe",
        "payload": request_body
    });

    framed
        .send(ws::Message::Text(request_body_json.to_string().into()))
        .await?;

    Ok((srv, ws, framed))
}

/// Calls a GraphQL subscription and returns the first response.
pub async fn call_ws_graphql<F, A, B, R>(
    context: FullContext,
    app_builder: F,
    request_body: GraphQLCustomRequest<'_>,
) -> anyhow::Result<GraphQLCustomResponse<R>>
where
    R: DeserializeOwned,
    F: Fn(FullContext) -> App<A> + Clone + Send + Sync + 'static,
    A: ServiceFactory<
            ServiceRequest,
            Response = ServiceResponse<B>,
            Config = (),
            InitError = (),
            Error = actix_web::Error,
        > + 'static,
    B: MessageBody + 'static,
{
    let name = request_body.name;
    let (_srv, _ws, mut framed) =
        call_ws_graphql_stream(context, app_builder, request_body).await?;
    let response = parse_graphql_subscription_response(&mut framed, name).await?;

    Ok(response)
}

/// Parses a GraphQL subscription response.
pub async fn parse_graphql_subscription_response<R>(
    framed: &mut Framed<BoxedSocket, ws::Codec>,
    name: &str,
) -> anyhow::Result<GraphQLCustomResponse<R>>
where
    R: DeserializeOwned,
{
    let start = Instant::now();

    loop {
        let res = timeout(
            Duration::from_secs(DEFAULT_TIMEOUT_SECONDS - start.elapsed().as_secs()),
            framed.next(),
        )
        .await;

        match res {
            Ok(Some(Ok(ws::Frame::Text(text)))) => {
                // When I need to debug the response
                // println!("text response: \n{}", str::from_utf8(&text)?);

                let mut graphql_response: GraphQLSubscriptionResponse =
                    serde_json::from_slice(&text).inspect_err(|err| {
                        println!("Failed to parse GraphQL subscription response: {err}");

                        println!(
                            "text response: \n{}",
                            str::from_utf8(&text).unwrap_or_default()
                        );
                    })?;

                // When I need to debug the response
                // println!("response: \n{graphql_response:#?}");

                if let Some(data) = graphql_response.payload.data.remove(name) {
                    return Ok(GraphQLCustomResponse {
                        data: serde_json::from_value(data)?,
                        errors: graphql_response.payload.errors,
                    });
                } else {
                    bail!("can't find {name} in response");
                }
            },
            Ok(Some(Ok(ws::Frame::Ping(ping)))) => {
                tracing::info!("Received ping for {name}");
                framed.send(ws::Message::Pong(ping)).await?;
                continue;
            },
            Ok(Some(Err(e))) => return Err(e.into()),
            Ok(None) => bail!("connection closed unexpectedly"),
            Ok(res) => bail!("unexpected message type: {res:?}"),
            Err(_) => bail!("timeout while waiting for response for {name}"),
        }
    }
}

pub fn build_actix_app<G>(
    app_ctx: FullContext,
    graphql_schema: G,
) -> App<
    impl ServiceFactory<
        ServiceRequest,
        Response = ServiceResponse<impl MessageBody>,
        Config = (),
        InitError = (),
        Error = actix_web::Error,
    >,
>
where
    G: Clone + 'static,
{
    build_actix_app_with_config(app_ctx, graphql_schema, |app_ctx, graphql_schema| {
        config_app(app_ctx, graphql_schema, 128 * 1024)
    })
}

/// Builds an Actix app with a custom config function. Used for Dango to have
/// a different GraphQL executor and custom routes to use that executor.
///
/// I tried really hard to use async-graphql + generics but couldn't get it to
/// work. I'm not sure that's doable.
///
/// See <https://github.com/async-graphql/async-graphql/discussions/1630>.
pub fn build_actix_app_with_config<F, G>(
    app_ctx: FullContext,
    graphql_schema: G,
    config_app: F,
) -> App<
    impl ServiceFactory<
        ServiceRequest,
        Response = ServiceResponse<impl MessageBody>,
        Config = (),
        InitError = (),
        Error = actix_web::Error,
    >,
>
where
    G: Clone + 'static,
    F: FnOnce(FullContext, G) -> Box<dyn Fn(&mut ServiceConfig)>,
{
    let app = App::new()
        .app_data(web::Data::new(SubscriptionLimiter::new(10, 5000)))
        .app_data(web::Data::new(GraphqlRequestTimeout(Duration::from_secs(
            30,
        ))))
        .wrap(Logger::default())
        .wrap(Compress::default());

    app.configure(config_app(app_ctx, graphql_schema))
}
