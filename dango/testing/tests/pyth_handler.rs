use {
    dango_primitives::{Coins, HashExt, NonEmpty, QuerierWrapper, ResultExt, btree_map},
    dango_proposal_preparer::{PythHandler, QueryPythId},
    dango_pyth_client::PythClientTrait,
    dango_pyth_types::{Channel, constants::LAZER_ENDPOINTS_TEST},
    dango_testing::setup_test,
    dango_types::{
        constants::perp_btc,
        oracle::{ExecuteMsg, InstantiateMsg, PriceConfig, PriceSource},
    },
    std::{thread::sleep, time::Duration},
};

#[tokio::test]
async fn handler() {
    let (mut suite, mut accounts, codes, contracts, _) = setup_test(Default::default());

    // Oracle from the setup_test has some PythIds already uploaded.
    let oracle = contracts.oracle;

    // Create an empty oracle (without any PythIds) to test the handler correctly
    // close the streaming.
    let empty_oracle = suite
        .instantiate(
            &mut accounts.owner,
            codes.oracle.to_bytes().sha2_256(),
            &InstantiateMsg {
                price_sources: btree_map! {},
                trusted_signers: btree_map! {},
            },
            "salt",
            None,
            None,
            Coins::new(),
        )
        .await
        .should_succeed()
        .address;

    let price_source = btree_map!(
        perp_btc::DENOM.clone() => PriceConfig::Single(PriceSource {
            id: 1,
            channel: Channel::RealTime,
        }),
    );

    suite
        .execute(
            &mut accounts.owner,
            oracle,
            &ExecuteMsg::RegisterPriceSources(price_source),
            Coins::new(),
        )
        .await
        .should_succeed();

    let querier = QuerierWrapper::new(&suite);
    let handler =
        PythHandler::new_with_cache(NonEmpty::new_unchecked(LAZER_ENDPOINTS_TEST), "lazer_token");

    // Start the handler with oracle.
    handler.update_stream(querier, oracle).unwrap();

    // Assert the handler is working.
    check_handler_works(&handler, 3);

    // Update the handler with the empty oracle.
    handler.update_stream(querier, empty_oracle).unwrap();

    // Remove possible data.
    handler.fetch_latest_price_update();

    // Assert the streaming is closed.
    for _ in 0..3 {
        assert!(handler.fetch_latest_price_update().is_none());
        sleep(Duration::from_millis(500));
    }

    // Update the handler with oracle.
    handler.update_stream(querier, oracle).unwrap();

    // Assert the handler is working.
    check_handler_works(&handler, 3);
}

// Check the handler returns data correctly.
fn check_handler_works<P>(handler: &PythHandler<P>, data_wanted: usize)
where
    P: PythClientTrait + QueryPythId,
{
    // Assert the streaming is working.
    let mut received_data = 0;
    let mut previous_data = None;

    for _ in 0..data_wanted * 5 {
        let data = handler.fetch_latest_price_update();

        // Check if we received some data.
        if data.is_some() {
            received_data += 1;

            if let Some(previous) = previous_data {
                assert_ne!(data, previous, "Data should change over time");
            }

            previous_data = Some(data);

            // Now that we have read the data, the next iteration should be empty
            // since the handler didn't have time to fetch new data.
            assert!(handler.fetch_latest_price_update().is_none());

            // We have met the data wanted, we can stop.
            if received_data >= data_wanted {
                return;
            }
        }

        sleep(Duration::from_millis(500));
    }

    panic!("Expected to receive at least {data_wanted} data, but received only {received_data}");
}
