//! GraphQL operation smoke tests.
//!
//! For each generated query and subscription type, spin up a mock dango-httpd
//! server, send the operation with sample variables, and assert the response
//! roundtrips through the generated `ResponseData` deserializer. These tests
//! don't depend on any specific on-chain state — they verify that the .graphql
//! operation files, the schema, and the codegen are mutually consistent. Richer
//! end-to-end coverage with realistic on-chain scenarios lives in
//! `dango/testing/tests/httpd/`.

use {
    assertor::*,
    dango_genesis::GenesisOption,
    dango_sdk::{
        QueryApp, Simulate, SubscribeAccounts, SubscribeBlock, SubscribeEventByAddresses,
        SubscribeEvents, SubscribeMessages, SubscribePerpsCandles, SubscribePerpsTrades,
        SubscribeQueryApp, SubscribeQueryStatus, SubscribeTransactions, SubscribeTransfers,
        WsClient, query_app, simulate, subscribe_accounts, subscribe_block,
        subscribe_event_by_addresses, subscribe_events, subscribe_messages,
        subscribe_perps_candles, subscribe_perps_trades, subscribe_query_app,
        subscribe_query_status, subscribe_transactions, subscribe_transfers,
    },
    dango_testing::{
        BlockCreation, Preset, TestOption, mock_httpd_run_with_port_sender,
        mock_httpd_wait_for_server_ready,
    },
    futures::StreamExt,
    graphql_client::{GraphQLQuery, Response},
    serde_json::json,
    std::{sync::mpsc, time::Duration},
};

const MAX_RETRIES: u32 = 3;
const TIMEOUT_SECS: u64 = 5;

/// Spawn a mock dango-httpd in a separate thread and return the bound port
/// once the server is responsive.
async fn spawn_mock_server() -> anyhow::Result<u16> {
    let (port_tx, port_rx) = mpsc::channel::<u16>();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let _ = mock_httpd_run_with_port_sender(
                BlockCreation::OnBroadcast,
                None,
                TestOption::default(),
                GenesisOption::preset_test(),
                None,
                port_tx,
            )
            .await;
        });
    });

    let port = port_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("mock server never reported a port"))?;

    mock_httpd_wait_for_server_ready(port).await?;

    Ok(port)
}

// -----------------------------------------------------------------------------
// Query smoke tests
// -----------------------------------------------------------------------------

macro_rules! query_smoke_test {
    ($test_name:ident, $query_type:ty, $variables:expr) => {
        #[tokio::test]
        async fn $test_name() -> anyhow::Result<()> {
            let port = spawn_mock_server().await?;
            let url = format!("http://localhost:{port}/graphql");

            let result = reqwest::Client::builder()
                .build()?
                .post(url)
                .json(&<$query_type>::build_query($variables))
                .send()
                .await?
                .json::<Response<<$query_type as GraphQLQuery>::ResponseData>>()
                .await;

            assert_that!(result).is_ok();

            Ok(())
        }
    };
}

query_smoke_test!(
    test_query_app,
    QueryApp,
    query_app::Variables {
        request: json!({"config":{}}),
        height: None,
    }
);

query_smoke_test!(
    test_simulate,
    Simulate,
    simulate::Variables {
        tx: json!({
            "data": {
                "chain_id": "dev-1",
                "nonce": 1,
                "username": "owner"
            },
            "msgs": [
                {
                    "transfer": {
                        "0x01bba610cbbfe9df0c99b8862f3ad41b2f646553": {
                            "hyp/all/btc": "100"
                        }
                    }
                }
            ],
            "sender": "0x33361de42571d6aa20c37daa6da4b5ab67bfaad9"
        }),
    }
);

// -----------------------------------------------------------------------------
// Subscription smoke tests
// -----------------------------------------------------------------------------

macro_rules! subscription_smoke_test {
    ($test_name:ident, $sub_type:ty, $variables:expr, $expect:expr, $require_data:expr) => {
        #[tokio::test]
        async fn $test_name() -> anyhow::Result<()> {
            let port = spawn_mock_server().await?;
            let ws_url = format!("ws://localhost:{port}/graphql");
            let client = WsClient::new(&ws_url)?;
            let mut stream = client.subscribe::<$sub_type>($variables).await?;

            let mut retries = 0;
            let require_data: bool = $require_data;

            loop {
                let result =
                    tokio::time::timeout(Duration::from_secs(TIMEOUT_SECS), stream.next()).await;

                match result {
                    Ok(Some(Ok(response))) => {
                        if let Some(errors) = &response.errors {
                            if !errors.is_empty() {
                                if require_data {
                                    panic!("GraphQL errors: {errors:?}");
                                } else {
                                    break;
                                }
                            }
                        }

                        if let Some(data) = response.data {
                            assert!(($expect)(data), "Response data did not match expected");
                        } else if require_data {
                            panic!("Expected data in response but got None");
                        }

                        break;
                    },
                    Ok(Some(Err(e))) => {
                        panic!("Subscription error: {e}");
                    },
                    Ok(None) => {
                        panic!("Subscription stream ended unexpectedly");
                    },
                    Err(_) => {
                        retries += 1;
                        if retries >= MAX_RETRIES {
                            if require_data {
                                panic!("Subscription timed out after {MAX_RETRIES} retries");
                            } else {
                                break;
                            }
                        }
                    },
                }
            }

            Ok(())
        }
    };
}

subscription_smoke_test!(
    test_subscribe_block,
    SubscribeBlock,
    subscribe_block::Variables,
    |data: subscribe_block::ResponseData| { data.block.block_height >= 0 },
    false
);

subscription_smoke_test!(
    test_subscribe_accounts,
    SubscribeAccounts,
    subscribe_accounts::Variables::default(),
    |_data: subscribe_accounts::ResponseData| { true },
    false
);

subscription_smoke_test!(
    test_subscribe_transfers,
    SubscribeTransfers,
    subscribe_transfers::Variables::default(),
    |_data: subscribe_transfers::ResponseData| { true },
    false
);

subscription_smoke_test!(
    test_subscribe_transactions,
    SubscribeTransactions,
    subscribe_transactions::Variables::default(),
    |_data: subscribe_transactions::ResponseData| { true },
    false
);

subscription_smoke_test!(
    test_subscribe_messages,
    SubscribeMessages,
    subscribe_messages::Variables::default(),
    |_data: subscribe_messages::ResponseData| { true },
    false
);

subscription_smoke_test!(
    test_subscribe_events,
    SubscribeEvents,
    subscribe_events::Variables::default(),
    |_data: subscribe_events::ResponseData| { true },
    false
);

subscription_smoke_test!(
    test_subscribe_event_by_addresses,
    SubscribeEventByAddresses,
    subscribe_event_by_addresses::Variables {
        addresses: vec!["0x0000000000000000000000000000000000000000".to_string()],
        since_block_height: None,
    },
    |_data: subscribe_event_by_addresses::ResponseData| { true },
    false
);

subscription_smoke_test!(
    test_subscribe_perps_candles,
    SubscribePerpsCandles,
    subscribe_perps_candles::Variables {
        pair_id: "perp/ethusd".to_string(),
        interval: subscribe_perps_candles::CandleInterval::ONE_MINUTE,
        later_than: None,
    },
    |_data: subscribe_perps_candles::ResponseData| { true },
    false
);

subscription_smoke_test!(
    test_subscribe_perps_trades,
    SubscribePerpsTrades,
    subscribe_perps_trades::Variables {
        pair_id: "perp/ethusd".to_string(),
    },
    |data: subscribe_perps_trades::ResponseData| { data.perps_trades.pair_id == "perp/ethusd" },
    false
);

subscription_smoke_test!(
    test_subscribe_query_app,
    SubscribeQueryApp,
    subscribe_query_app::Variables {
        request: json!({"config":{}}),
        block_interval: 10,
    },
    |data: subscribe_query_app::ResponseData| { data.query_app.block_height >= 0 },
    true
);

subscription_smoke_test!(
    test_subscribe_query_status,
    SubscribeQueryStatus,
    subscribe_query_status::Variables { block_interval: 10 },
    |data: subscribe_query_status::ResponseData| {
        !data.query_status.chain_id.is_empty() && data.query_status.block.block_height >= 0
    },
    true
);
