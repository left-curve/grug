use {
    crate::{MockClient, OracleTestEntry, TestSuite},
    byteorder::LE,
    dango_app::{AppError, Db, Indexer, ProposalPreparer, Vm},
    dango_db_memory::MemDb,
    dango_math::Fraction,
    dango_order_book::UsdPrice,
    dango_primitives::{
        Addr, Binary, BroadcastClient, ByteArray, Coins, Denom, Message, NonEmpty, ResultExt,
        Signer, Timestamp,
    },
    dango_pyth_types::{Channel, LeEcdsaMessage, MarketSession, PythId},
    dango_types::{
        oracle::{self, PriceConfig, PriceSource},
        perps,
    },
    dango_vm_rust::RustVm,
    k256::ecdsa::SigningKey,
    pyth_lazer_protocol::{
        ChannelId, Price as LazerPrice, PriceFeedId,
        api::MarketSession as LazerMarketSession,
        payload::{PayloadData, PayloadFeedData, PayloadPropertyValue},
        time::TimestampUs,
    },
    std::collections::BTreeMap,
};

const MOCK_SIGNER_SECRET: [u8; 32] = [0x42; 32];

struct MockPythSigner {
    signing_key: SigningKey,
}

impl MockPythSigner {
    fn new() -> Self {
        let signing_key =
            SigningKey::from_bytes(&MOCK_SIGNER_SECRET.into()).expect("valid secret key");
        Self { signing_key }
    }

    fn sign_prices(
        &self,
        feeds: &[(PythId, UsdPrice, MarketSession)],
        timestamp: Timestamp,
    ) -> LeEcdsaMessage {
        let timestamp_us = timestamp_to_micros(timestamp);

        let payload = PayloadData {
            timestamp_us: TimestampUs::from_micros(timestamp_us),
            channel_id: ChannelId::REAL_TIME,
            feeds: feeds
                .iter()
                .map(|&(id, price, market_session)| {
                    let raw = price.into_inner().numerator().0;
                    let mantissa: i64 = raw.try_into().expect("price mantissa overflows i64");

                    PayloadFeedData {
                        feed_id: PriceFeedId(id),
                        properties: vec![
                            PayloadPropertyValue::Price(Some(
                                LazerPrice::from_mantissa(mantissa).expect("zero price"),
                            )),
                            PayloadPropertyValue::Exponent(-6),
                            PayloadPropertyValue::MarketSession(match market_session {
                                MarketSession::Regular => LazerMarketSession::Regular,
                                MarketSession::Other => LazerMarketSession::Closed,
                            }),
                        ],
                    }
                })
                .collect(),
        };

        let mut payload_bytes = Vec::new();
        payload
            .serialize::<LE>(&mut payload_bytes)
            .expect("payload serialization");

        let hash = dango_crypto::keccak256(&payload_bytes);
        let (sig, recovery_id) = self.signing_key.sign_prehash_recoverable(&hash);

        LeEcdsaMessage {
            payload: Binary::from_inner(payload_bytes),
            signature: ByteArray::from_inner(sig.to_bytes().into()),
            recovery_id: recovery_id.to_byte(),
        }
    }
}

pub fn mock_pyth_trusted_signer() -> Binary {
    let sk = SigningKey::from_bytes(&MOCK_SIGNER_SECRET.into()).expect("valid secret key");
    let pk: ByteArray<33> = sk
        .verifying_key()
        .to_sec1_point(true)
        .to_bytes()
        .as_ref()
        .try_into()
        .unwrap();
    Binary::from_inner(pk.to_vec())
}

fn timestamp_to_micros(t: Timestamp) -> u64 {
    let micros = t.into_nanos() / 1_000;
    micros.min(u64::MAX as u128) as u64
}

fn build_oracle_messages(
    oracle: Addr,
    perps: Addr,
    do_feed_prices: Option<(&[(PythId, UsdPrice, MarketSession)], Timestamp)>,
    do_refresh_index_prices: bool,
    do_refresh_vault_orders: bool,
) -> Vec<Message> {
    let mut msgs = Vec::new();

    if let Some((feeds, timestamp)) = do_feed_prices {
        let signer = MockPythSigner::new();
        let message = signer.sign_prices(feeds, timestamp);

        msgs.push(
            Message::execute(
                oracle,
                &oracle::ExecuteMsg::FeedPrices(NonEmpty::new_unchecked(vec![message])),
                Coins::new(),
            )
            .unwrap(),
        );
    }

    if do_refresh_index_prices {
        msgs.push(
            Message::execute(
                perps,
                &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshIndexPrices {}),
                Coins::new(),
            )
            .unwrap(),
        );
    }

    if do_refresh_vault_orders {
        msgs.push(
            Message::execute(
                perps,
                &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::RefreshVaultOrders {}),
                Coins::new(),
            )
            .unwrap(),
        );
    }

    msgs
}

fn build_register_price_sources(
    entries: &BTreeMap<Denom, OracleTestEntry>,
) -> BTreeMap<Denom, PriceConfig> {
    entries
        .iter()
        .map(|(denom, e)| {
            (
                denom.clone(),
                PriceConfig::Single(PriceSource {
                    id: e.pyth_id,
                    channel: Channel::RealTime,
                }),
            )
        })
        .collect()
}

// ---- TestSuite methods ----

impl<PP, ID> TestSuite<MemDb, RustVm, PP, ID>
where
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<<PP as ProposalPreparer>::Error>,
{
    /// Execute an arbitrary combination of oracle and perps maintenance actions.
    pub async fn do_oracle_actions(
        &mut self,
        owner: &mut (dyn Signer + Send + Sync),
        do_register_price_sources: Option<BTreeMap<Denom, OracleTestEntry>>,
        do_feed_prices: Option<(&[(PythId, UsdPrice, MarketSession)], Timestamp)>,
        do_refresh_index_prices: bool,
        do_refresh_vault_orders: bool,
    ) {
        if let Some(entries) = &do_register_price_sources {
            let price_sources = build_register_price_sources(entries);

            self.execute(
                owner,
                self.contracts.oracle,
                &oracle::ExecuteMsg::RegisterPriceSources(price_sources),
                Coins::new(),
            )
            .await
            .should_succeed();
        }

        let msgs = build_oracle_messages(
            self.contracts.oracle,
            self.contracts.perps,
            do_feed_prices,
            do_refresh_index_prices,
            do_refresh_vault_orders,
        );

        if let Ok(msgs) = NonEmpty::new(msgs) {
            self.send_messages(owner, msgs).await.should_succeed();
        }
    }

    /// Register price sources, feed initial prices, and refresh index prices
    /// and vault orders.
    pub async fn seed_oracle_prices(
        &mut self,
        owner: &mut (dyn Signer + Send + Sync),
        entries: BTreeMap<Denom, OracleTestEntry>,
    ) {
        let feeds: Vec<_> = entries
            .values()
            .map(|e| (e.pyth_id, e.humanized_price, MarketSession::Regular))
            .collect();

        self.do_oracle_actions(
            owner,
            Some(entries),
            Some((&feeds, Timestamp::MAX)),
            true,
            true,
        )
        .await;
    }

    /// Feed prices and refresh index prices and vault orders.
    pub async fn feed_oracle_prices(
        &mut self,
        owner: &mut (dyn Signer + Send + Sync),
        feeds: &[(PythId, UsdPrice, MarketSession)],
        timestamp: Option<Timestamp>,
    ) {
        self.do_oracle_actions(
            owner,
            None,
            Some((feeds, timestamp.unwrap_or(Timestamp::MAX))),
            true,
            true,
        )
        .await;
    }
}

// ---- MockClient methods ----

const MOCK_CLIENT_GAS_LIMIT: u64 = 20_000_000;

impl<DB, VM, PP, ID> MockClient<DB, VM, PP, ID>
where
    DB: Db + Send + Sync + 'static,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer + Send + Sync + 'static,
    ID: Indexer + Send + Sync + 'static,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    /// Execute an arbitrary combination of oracle and perps maintenance actions.
    pub async fn do_oracle_actions(
        &self,
        owner: &mut (dyn Signer + Send + Sync),
        do_register_price_sources: Option<BTreeMap<Denom, OracleTestEntry>>,
        do_feed_prices: Option<(&[(PythId, UsdPrice, MarketSession)], Timestamp)>,
        do_refresh_index_prices: bool,
        do_refresh_vault_orders: bool,
    ) {
        let chain_id = self.chain_id().await;
        let contracts = self.suite().await.contracts.clone();

        if let Some(entries) = &do_register_price_sources {
            let price_sources = build_register_price_sources(entries);

            let tx = owner
                .sign_transaction(
                    NonEmpty::new_unchecked(vec![
                        Message::execute(
                            contracts.oracle,
                            &oracle::ExecuteMsg::RegisterPriceSources(price_sources),
                            Coins::new(),
                        )
                        .unwrap(),
                    ]),
                    &chain_id,
                    MOCK_CLIENT_GAS_LIMIT,
                )
                .unwrap();

            self.broadcast_tx(tx)
                .await
                .unwrap()
                .check_tx
                .should_succeed();
        }

        let msgs = build_oracle_messages(
            contracts.oracle,
            contracts.perps,
            do_feed_prices,
            do_refresh_index_prices,
            do_refresh_vault_orders,
        );

        if let Ok(msgs) = NonEmpty::new(msgs) {
            let tx = owner
                .sign_transaction(msgs, &chain_id, MOCK_CLIENT_GAS_LIMIT)
                .unwrap();

            self.broadcast_tx(tx)
                .await
                .unwrap()
                .check_tx
                .should_succeed();
        }
    }

    /// Register price sources, feed initial prices, and refresh index prices
    /// and vault orders.
    pub async fn seed_oracle_prices(
        &self,
        owner: &mut (dyn Signer + Send + Sync),
        entries: BTreeMap<Denom, OracleTestEntry>,
    ) {
        let feeds: Vec<_> = entries
            .values()
            .map(|e| (e.pyth_id, e.humanized_price, MarketSession::Regular))
            .collect();

        self.do_oracle_actions(
            owner,
            Some(entries),
            Some((&feeds, Timestamp::MAX)),
            true,
            true,
        )
        .await;
    }

    /// Feed prices and refresh index prices and vault orders.
    pub async fn feed_oracle_prices(
        &self,
        owner: &mut (dyn Signer + Send + Sync),
        feeds: &[(PythId, UsdPrice, MarketSession)],
        timestamp: Option<Timestamp>,
    ) {
        self.do_oracle_actions(
            owner,
            None,
            Some((feeds, timestamp.unwrap_or(Timestamp::MAX))),
            true,
            true,
        )
        .await;
    }
}
