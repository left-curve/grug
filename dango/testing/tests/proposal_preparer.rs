use {
    dango_db_memory::MemDb,
    dango_primitives::{
        Addr, Binary, Coins, Denom, Duration as CoreDuration, NonEmpty, QuerierExt, ResultExt,
        btree_map,
    },
    dango_proposal_preparer::{ProposalPreparer, QueryPythId},
    dango_pyth_client::{PythClientCache, PythClientTrait},
    dango_pyth_types::{
        Channel, FixedRate, PythLazerSubscriptionDetails,
        constants::{BTC_USD_ID, LAZER_ENDPOINTS_TEST, LAZER_TRUSTED_SIGNER},
    },
    dango_testing::{TestSuite, setup_test},
    dango_types::{
        constants::perp_btc,
        oracle::{
            ExecuteMsg, PriceConfig, PriceSource, QueryPriceRequest, QueryPriceSourcesRequest,
        },
    },
    dango_vm_rust::RustVm,
    std::{
        str::FromStr,
        thread::{self, sleep},
        time::Duration,
    },
};

const NOT_USED_ID_LAZER: PythLazerSubscriptionDetails = PythLazerSubscriptionDetails {
    id: 9,
    channel: Channel::FixedRate(FixedRate::RATE_200_MS),
};

#[tokio::test]
async fn proposal_pyth() {
    // Ensure there are all cache file for the PythIds in oracle and also for
    // the NOT_USED_ID and retrieve them if not presents. This is needed since
    // the PythPPHandler create a thread to get the data from Pyth and if the
    // cache files are not present the thread will not wait for client to retrieve
    // and save them. The test will end before the client is able to finish.
    {
        let (suite, _, _, contracts, _) = setup_test(Default::default());

        // Retrieve all PythIds from the oracle.
        let mut pyth_ids = suite
            .query_wasm_smart(
                contracts.oracle,
                QueryPriceSourcesRequest {
                    start_after: None,
                    limit: None,
                },
            )
            .should_succeed()
            .into_values()
            .flat_map(|config| config.feeds())
            .collect::<Vec<_>>();

        // Create cache for ids if not present.
        pyth_ids.push(NOT_USED_ID_LAZER);

        // Ensure to have the cache files for all the ids.
        PythClientCache::new(NonEmpty::new_unchecked(LAZER_ENDPOINTS_TEST), "lazer_token")
            .unwrap()
            .load_or_retrieve_data(NonEmpty::new_unchecked(pyth_ids));
    }

    let (mut suite, mut accounts, _, contracts, _) = setup_test(Default::default());

    let current_time = suite.block.timestamp;

    let oracle = contracts.oracle;

    let price_source = btree_map!(
        perp_btc::DENOM.clone() => PriceConfig::Single(PriceSource { id: BTC_USD_ID.id, channel: BTC_USD_ID.channel })
    );

    let pubkey = Binary::from_str(LAZER_TRUSTED_SIGNER).unwrap();

    suite
        .execute(
            &mut accounts.owner,
            oracle,
            &ExecuteMsg::RegisterTrustedSigner {
                public_key: pubkey,
                expires_at: current_time + CoreDuration::from_minutes(10),
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.owner,
            oracle,
            &ExecuteMsg::RegisterPriceSources(price_source),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Assert the price of perp_btc exists.
    sleep(Duration::from_secs(2));
    assert_price_exists(&mut suite, contracts.oracle, perp_btc::DENOM.clone()).await;

    // Retrieve the prices and sequences.
    let prices1 = suite
        .query_wasm_smart(
            oracle,
            QueryPriceRequest {
                denom: perp_btc::DENOM.clone(),
            },
        )
        .should_succeed();

    // Await some time and assert that the timestamps are updated.
    sleep(Duration::from_secs(1));

    suite.make_empty_block().await;

    let prices2 = suite
        .query_wasm_smart(
            oracle,
            QueryPriceRequest {
                denom: perp_btc::DENOM.clone(),
            },
        )
        .should_succeed();

    // Assert that the prices have been updated.
    //
    // This means either the timestamp is newer, or the timestamp is equal but
    // the sequence is newer.
    assert_ne!(
        prices1.timestamp, prices2.timestamp,
        "The price timestamp should be updated"
    );

    // Push a new PythId to oracle to verify that the handler update the
    // ids correctly.
    {
        let test_denom = Denom::from_str("test").unwrap();

        // Verify the denom does not exist in the oracle.
        suite
            .query_wasm_smart(
                contracts.oracle,
                QueryPriceRequest {
                    denom: test_denom.clone(),
                },
            )
            .should_fail_with_error("data not found");

        // Verify the NOT_USED_ID is not in the oracle.
        suite
            .query_wasm_smart(
                contracts.oracle,
                QueryPriceSourcesRequest {
                    start_after: None,
                    limit: Some(u32::MAX),
                },
            )
            .should_succeed()
            .values()
            .flat_map(|config| config.feeds())
            .for_each(|source| {
                assert_ne!(source.id, NOT_USED_ID_LAZER.id);
            });

        // Push NOT_USED_ID to the oracle.
        suite
            .execute(
                &mut accounts.owner,
                contracts.oracle,
                &ExecuteMsg::RegisterPriceSources(btree_map! {
                    test_denom.clone() => PriceConfig::Single(PriceSource {
                        id: NOT_USED_ID_LAZER.id,
                        channel: NOT_USED_ID_LAZER.channel,
                    }),
                }),
                Coins::new(),
            )
            .await
            .should_succeed();

        // Verify that the price exists.
        sleep(Duration::from_secs(1));
        assert_price_exists(&mut suite, oracle, test_denom).await;
    }
}

async fn assert_price_exists<P>(
    suite: &mut TestSuite<MemDb, RustVm, ProposalPreparer<P>>,
    oracle: Addr,
    denom: Denom,
) where
    P: PythClientTrait + QueryPythId + Send + 'static,
    P::Error: std::fmt::Debug,
{
    // Trigger a few blocks to be sure the PP has time to update the prices.
    let mut price = None;

    for _ in 0..10 {
        thread::sleep(Duration::from_millis(200));

        let txs = suite.make_empty_block().await.block_outcome.tx_outcomes;

        // Ensure all tx passed.
        for tx in txs {
            tx.should_succeed();
        }

        if let Ok(p) = suite.query_wasm_smart(
            oracle,
            QueryPriceRequest {
                denom: denom.clone(),
            },
        ) {
            price = Some(p);
            break;
        }
    }

    assert!(price.is_some(), "Unable to retrieve price from oracle");
}
