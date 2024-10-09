use {
    criterion::{criterion_group, criterion_main, BatchSize, Criterion},
    dango_testing::setup_benchmark,
    dango_types::{
        account::single,
        account_factory::{self, AccountParams, Salt},
        amm::{self, FeeRate, PoolParams, XykParams},
    },
    grug::{
        btree_map, Addr, Coins, HashExt, JsonSerExt, Message, ResultExt, Signer, Tx, Udec128,
        UniqueVec,
    },
    grug_db_disk::TempDataDir,
    rand::{distributions::Alphanumeric, Rng},
    std::time::Duration,
};

/// Measure how many token transfers can be processed in a second.
///
/// We do this by making a single block that contains 100 transactions, each tx
/// containing one `Message::Transfer`.
fn sends(c: &mut Criterion) {
    c.bench_function("send", |b| {
        b.iter_batched(
            || {
                // Create a random folder for this iteration.
                let random_string = rand::thread_rng()
                    .sample_iter(&Alphanumeric)
                    .take(7)
                    .map(char::from)
                    .collect::<String>();
                let dir = TempDataDir::new(&format!("__dango_benchmark_sends_{random_string}"));
                let (mut suite, mut accounts, codes, contracts) =
                    setup_benchmark(&dir, 100).unwrap();

                // Deploy 200 accounts.
                // The first 100 will be senders; the second 100 will be receivers.
                // For convenience, all accounts are owned by the owner.
                let msgs = (0..200)
                    .map(|i| {
                        Message::execute(
                            contracts.account_factory,
                            &account_factory::ExecuteMsg::RegisterAccount {
                                params: AccountParams::Spot(single::Params {
                                    owner: accounts.owner.username.clone(),
                                }),
                            },
                            if i < 100 {
                                Coins::one("uusdc", 100_000_000).unwrap()
                            } else {
                                Coins::new()
                            },
                        )
                        .unwrap()
                    })
                    .collect::<Vec<_>>();

                // In experience, this costs ~34M gas.
                suite
                    .send_messages_with_gas(&mut accounts.owner, 50_000_000, msgs)
                    .unwrap()
                    .result
                    .should_succeed();

                // Make a block that contains 100 transactions.
                // The i-th transaction is the i-th sender sending coins to the i-receiver.
                let txs = (0..100)
                    .map(|i| {
                        // Predict the sender address.
                        // During genesis we created 3 accounts, so offset i by 3.
                        let sender = Addr::compute(
                            contracts.account_factory,
                            codes.account_spot.hash256(),
                            Salt { index: i + 3 }.into_bytes().as_slice(),
                        );

                        // Predict the receiver address.
                        let receiver = Addr::compute(
                            contracts.account_factory,
                            codes.account_spot.hash256(),
                            Salt { index: i + 103 }.into_bytes().as_slice(),
                        );

                        // Sign the transaction.
                        let msg = Message::Transfer {
                            to: receiver,
                            coins: Coins::one("uusdc", 123).unwrap(),
                        };

                        let (data, credential) = accounts
                            .owner
                            .sign_transaction_with_sequence(vec![msg.clone()], &suite.chain_id, 0)
                            .unwrap();

                        Tx {
                            sender,
                            gas_limit: 2_000_000,
                            msgs: vec![msg],
                            data: data.to_json_value().unwrap(),
                            credential: credential.to_json_value().unwrap(),
                        }
                    })
                    .collect::<Vec<_>>();

                // Note: `dir` must be passed to the routine, so that it's alive
                // until the end of this iteration.
                (dir, suite, txs)
            },
            |(_dir, mut suite, txs)| {
                suite
                    .make_block(txs)
                    .unwrap()
                    .tx_outcomes
                    .into_iter()
                    .all(|outcome| outcome.result.is_ok());
            },
            BatchSize::SmallInput,
        );
    });
}

fn swaps(c: &mut Criterion) {
    c.bench_function("swap", |b| {
        b.iter_batched(
            || {
                // Create a random folder for this iteration.
                let random_string = rand::thread_rng()
                    .sample_iter(&Alphanumeric)
                    .take(7)
                    .map(char::from)
                    .collect::<String>();
                let dir = TempDataDir::new(&format!("__dango_benchmark_swaps_{random_string}"));
                let (mut suite, mut accounts, _, contracts) = setup_benchmark(&dir, 100).unwrap();

                // Create an ATOM-USDC pool.
                suite
                    .execute_with_gas(
                        &mut accounts.owner,
                        5_000_000,
                        contracts.amm,
                        &amm::ExecuteMsg::CreatePool(PoolParams::Xyk(XykParams {
                            liquidity_fee_rate: FeeRate::new_unchecked(Udec128::new_bps(30)),
                        })),
                        Coins::try_from(btree_map! {
                            "uatom" => 100_000_000,
                            "uusdc" => 400_000_000,
                        })
                        .unwrap(),
                    )
                    .unwrap();

                // Create and sign 100 transactions, each containing a swap.
                let txs = (0..100)
                    .map(|_| {
                        accounts
                            .owner
                            .sign_transaction(
                                vec![Message::execute(
                                    contracts.amm,
                                    &amm::ExecuteMsg::Swap {
                                        route: UniqueVec::new_unchecked(vec![1]),
                                        minimum_output: None,
                                    },
                                    Coins::one("uusdc", 100).unwrap(),
                                )
                                .unwrap()],
                                &suite.chain_id,
                                50_000_000,
                            )
                            .unwrap()
                    })
                    .collect::<Vec<_>>();

                (dir, suite, txs)
            },
            |(_dir, mut suite, txs)| {
                suite
                    .make_block(txs)
                    .unwrap()
                    .tx_outcomes
                    .into_iter()
                    .all(|outcome| outcome.result.is_ok());
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group! {
    name    = tps_measurement;
    config  = Criterion::default().measurement_time(Duration::from_secs(90)).sample_size(100);
    targets = sends, swaps
}

criterion_main!(tps_measurement);
