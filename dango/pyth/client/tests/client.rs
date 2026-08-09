pub mod common_function;

use {
    crate::common_function::test_stream,
    axum::{
        Router,
        extract::{
            State, WebSocketUpgrade,
            ws::{Message, WebSocket},
        },
        response::IntoResponse,
        routing::get,
    },
    dango_primitives::NonEmpty,
    dango_pyth_client::{PythClient, PythClientCache, PythClientTrait},
    dango_pyth_types::{
        PythLazerSubscriptionDetails,
        constants::{BTC_USD_ID, ETH_USD_ID, HYPE_USD_ID, LAZER_ENDPOINTS_TEST, SOL_USD_ID},
    },
    pyth_lazer_protocol::{
        api::{SubscriptionId, WsRequest},
        binary_update::BinaryWsUpdate,
        message::Message as PythMessage,
    },
    reqwest::StatusCode,
    std::{
        net::SocketAddr,
        sync::Arc,
        time::{Duration, Instant},
    },
    tokio::{select, sync::Mutex, time::sleep},
    tokio_stream::StreamExt,
    tracing::{error, info},
};

const TOKEN: &str = "insert_lazer_token_here";

#[ignore = "rely on network calls"]
#[tokio::test]
async fn test_lazer_stream() {
    let client = PythClient::new(NonEmpty::new_unchecked(LAZER_ENDPOINTS_TEST), TOKEN).unwrap();

    test_stream(
        client,
        vec![BTC_USD_ID, ETH_USD_ID],
        vec![SOL_USD_ID, HYPE_USD_ID],
    )
    .await;
}

// This is used to test the reconnection logic of the PythClientLazer.
// This test will:
// - Start 2 WS servers, one that will drop the connection after sending a few messages
//   and one that will keep the connection alive.
// - Create a PythClientLazer with both servers as endpoints.
// - Start a stream with the client.
// - Wait for a few seconds to allow the client to reconnect.
// - Ensure the client has reconnected multiple times;
// - Ensure there are some data in the stream.
#[ignore = "fails on CI; need to investigate; TODO"]
#[tokio::test(flavor = "multi_thread")]
async fn reconnection() {
    // Random port 15k - 16k.
    let port = rand::random_range(15000..16000);
    let port_alive = rand::random_range(15000..16000);

    // Run the mock ws server to keep connection alive.
    run_server(port_alive, true).await;

    // Run the mock ws server that drop connection.
    run_server(port, false).await;

    let mut client = PythClient::new(
        NonEmpty::new_unchecked(vec![
            format!("ws://0.0.0.0:{port}/ws"),
            format!("ws://0.0.0.0:{port_alive}/ws"),
        ]),
        "test",
    )
    .unwrap();

    // Start the stream.
    let mut stream = client
        .stream(NonEmpty::new_unchecked(vec![BTC_USD_ID]))
        .await
        .unwrap();

    // The server will send some data to the ws connection. What we want to test is
    // that the client will try to reconnect when the server close the connection.
    // This mean we only check how many times the server received a new connection
    // from the PythClientLazer.
    sleep(Duration::from_secs(8)).await;

    // Check how many reconnection attempts the server received.
    let reconnections_alive = reqwest::get(format!("http://0.0.0.0:{port_alive}/reconnections"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
        .trim()
        .parse::<usize>()
        .unwrap();

    let reconnections = reqwest::get(format!("http://0.0.0.0:{port}/reconnections"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
        .trim()
        .parse::<usize>()
        .unwrap();

    assert!(
        reconnections_alive >= 1,
        "Expected at least 1 reconnection attempt on the alive server"
    );

    assert!(
        reconnections >= 2,
        "Expected at least 2 reconnection attempts"
    );

    select! {
        _ = sleep(Duration::from_secs(1)) => {
            error!("Test timed out waiting for stream data");
            panic!( "Test timed out waiting for stream data");
        },
        data = stream.next() => {
            assert!(data.is_some(), "Expected some data from the stream")
        }
    }
}

#[derive(Clone)]
struct AppState {
    // The PythLazer client will send 4 connections during subscription.
    // This vector is used to filter out duplicate connections, in order to count
    // correctly the reconnection attempts.
    connections: Arc<Mutex<Vec<SubscriptionId>>>,
    // Number of reconnection attempts.
    reconnection_attempts: Arc<Mutex<usize>>,
    keep_connection_alive: bool,
}

async fn run_server(port: u32, keep_connection_alive: bool) {
    let state = AppState {
        connections: Arc::new(Mutex::new(vec![])),
        reconnection_attempts: Arc::new(Mutex::new(0)),
        keep_connection_alive,
    };

    let app = Router::new()
        .route("/reconnections", get(reconnection_attempts))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let addr: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
    info!("listening on {addr}");

    tokio::spawn(async move {
        axum::serve(tokio::net::TcpListener::bind(addr).await.unwrap(), app)
            .await
            .unwrap();
    });
}

// Return the number of reconnection attempts.
async fn reconnection_attempts(State(state): State<AppState>) -> impl IntoResponse {
    let reconnection_attempts = state.reconnection_attempts.lock().await;

    let attempts = *reconnection_attempts;
    (StatusCode::OK, attempts.to_string())
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let socket = Arc::new(Mutex::new(socket));

    // Read the first message to get the subscription details.
    let first = {
        let mut guard = socket.lock().await;
        guard.recv().await
    };

    // Read the subscription request.
    let (subscription_id, price_ids) = match first {
        Some(Ok(Message::Text(txt))) => match serde_json::from_str::<WsRequest>(&txt) {
            Ok(request) => match request {
                WsRequest::Subscribe(sub) => {
                    (sub.subscription_id, sub.params.price_feed_ids.clone())
                },
                _ => {
                    error!("received unexpected request {request:#?}");
                    return;
                },
            },
            Err(_) => todo!(),
        },
        _ => {
            error!("received unexpected msg {first:#?}");
            return;
        },
    };

    // Check if this is a reconnection attempts or just a duplicate connection.
    let mut print_info = true;
    {
        let mut connections = state.connections.lock().await;

        // Duplicate connection, do not count it.
        if connections.contains(&subscription_id) {
            // Reject any duplicate connection in case the server keep alive
            // in order to have a clean test.
            if state.keep_connection_alive {
                return;
            }
            print_info = false;
        } else {
            // New connection.
            info!("Client sent ids {price_ids:#?}");
            connections.push(subscription_id);

            // Increment the number of reconnection attempts.
            let mut reconnection_attempts = state.reconnection_attempts.lock().await;
            *reconnection_attempts += 1;
        }
    }

    // Start a client cache in order to read the data from file.
    let mut pyth_client_cache =
        match PythClientCache::new(NonEmpty::new_unchecked(LAZER_ENDPOINTS_TEST), TOKEN) {
            Ok(client) => client,
            Err(err) => {
                error!("Error creating PythClientLazerCache: {err}");
                return;
            },
        };

    let subscriptions = price_ids
        .unwrap()
        .into_iter()
        .map(|id| PythLazerSubscriptionDetails {
            id: id.0,
            channel: dango_pyth_types::Channel::RealTime,
        })
        .collect::<Vec<_>>();

    let mut stream = pyth_client_cache
        .stream(NonEmpty::new_unchecked(subscriptions))
        .await
        .unwrap();

    sleep(Duration::from_secs(1)).await;

    // Send some value;
    let send_socket = Arc::clone(&socket);
    let mut iteration = 0;

    // If the server is set to keep the connection alive, send basically all data we have.
    // Otherwise just send a few messages and close the connection.
    let max_iteration = if state.keep_connection_alive {
        300
    } else {
        3
    };

    // Read data from cache and send it to the client.
    while let Some(data) = stream.next().await {
        iteration += 1;
        if iteration >= max_iteration {
            if print_info {
                info!("server closing connection after {iteration} messages");
            }
            break;
        }

        let msg = data.first().cloned().unwrap();

        let send_msg = BinaryWsUpdate {
            subscription_id,
            messages: vec![PythMessage::LeEcdsa(msg.into())],
        };

        let mut buf = vec![];
        send_msg.serialize(&mut buf).unwrap();

        if print_info {
            info!("Sending data to the client {iteration}");
        }
        send_socket
            .lock()
            .await
            .send(Message::binary(buf))
            .await
            .unwrap()
    }

    // Check if we need to keep the connection alive.
    if state.keep_connection_alive {
        // Keep the connection alive for a bit.
        sleep(Duration::from_secs(100)).await;
    }

    // Remove the connection.
    let mut connections = state.connections.lock().await;
    connections.retain(|id| *id != subscription_id);
    if print_info {
        info!(
            "Connection closed, remaining connections: {:#?}",
            *connections
        );
    }
}

#[ignore = "manual performance test"]
#[tokio::test]
async fn test_performance() {
    // Load the .env file
    dotenvy::dotenv().ok();

    let key = std::env::var("PYTH__ACCESS_TOKEN").expect("env var not set");

    let mut client = PythClient::new(NonEmpty::new_unchecked(LAZER_ENDPOINTS_TEST), key).unwrap();
    let mut stream = client
        .stream(NonEmpty::new_unchecked(vec![BTC_USD_ID]))
        .await
        .unwrap();

    let mut last = Instant::now();
    let mut counter = 0;

    // Average over 10 seconds
    let avg_seconds = 10;

    loop {
        // Wait for the next batch from your stream
        if let Some(price_update) = stream.next().await {
            // Increment the counter for how many updates you've received
            counter += price_update.len();

            let elapsed = last.elapsed();

            if elapsed >= Duration::from_secs(avg_seconds) {
                info!("updates/sec = {}", counter / avg_seconds as usize);

                // Reset
                counter = 0;
                last = Instant::now();
            }
        }
    }
}
