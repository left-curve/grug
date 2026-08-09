use {
    crate::{
        Addr, Binary, BlockInfo, Code, CodeStatus, Coin, Config, ContractInfo, Denom,
        GenericResult, GenericResultExt, Hash256, HashExt, Json, JsonSerExt, MockStorage,
        NextUpgrade, Order, PastUpgrade, Querier, Query, QueryResponse, QueryStatusResponse,
        StdError, StdResult, Storage,
    },
    dango_backtrace::BacktracedError,
    dango_math::{NumberConst, Uint128},
    serde::Serialize,
    std::collections::BTreeMap,
};

/// A function that handles Wasm smart queries.
type SmartQueryHandler = Box<dyn Fn(Addr, Json) -> Result<Json, BacktracedError<String>>>;

// ------------------------------- mock querier --------------------------------

/// A mock implementation of the [`Querier`](crate::Querier) trait for testing
/// purpose.
#[derive(Default)]
pub struct MockQuerier {
    status: Option<QueryStatusResponse>,
    config: Option<Config>,
    app_config: Option<Json>,
    next_upgrade: Option<NextUpgrade>,
    past_upgrades: BTreeMap<u64, PastUpgrade>,
    balances: BTreeMap<Addr, BTreeMap<Denom, Uint128>>,
    supplies: BTreeMap<Denom, Uint128>,
    codes: BTreeMap<Hash256, Code>,
    contracts: BTreeMap<Addr, ContractInfo>,
    raw_query_handler: MockRawQueryHandler,
    smart_query_handler: Option<SmartQueryHandler>,
}

impl MockQuerier {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_status<T>(mut self, chain_id: T, last_finalized_block: BlockInfo) -> Self
    where
        T: Into<String>,
    {
        self.status = Some(QueryStatusResponse {
            chain_id: chain_id.into(),
            last_finalized_block,
        });
        self
    }

    pub fn with_config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    pub fn with_app_config<T>(mut self, config: T) -> StdResult<Self>
    where
        T: Serialize,
    {
        self.app_config = Some(config.to_json_value()?);
        Ok(self)
    }

    pub fn with_next_upgrade(mut self, next_upgrade: Option<NextUpgrade>) -> Self {
        self.next_upgrade = next_upgrade;
        self
    }

    pub fn with_past_upgrade(mut self, height: u64, upgrade: PastUpgrade) -> Self {
        self.past_upgrades.insert(height, upgrade);
        self
    }

    pub fn with_balance<D, A>(mut self, address: Addr, denom: D, amount: A) -> StdResult<Self>
    where
        D: TryInto<Denom>,
        A: Into<Uint128>,
        StdError: From<D::Error>,
    {
        self.balances
            .entry(address)
            .or_default()
            .insert(denom.try_into()?, amount.into());
        Ok(self)
    }

    pub fn with_supplies<D, A>(mut self, denom: D, amount: A) -> StdResult<Self>
    where
        D: TryInto<Denom>,
        A: Into<Uint128>,
        StdError: From<D::Error>,
    {
        self.supplies.insert(denom.try_into()?, amount.into());
        Ok(self)
    }

    pub fn with_code<T>(mut self, code: T, status: CodeStatus) -> Self
    where
        T: Into<Binary>,
    {
        let code = code.into();
        let code_hash = code.sha2_256();

        self.codes.insert(code_hash, Code { code, status });
        self
    }

    pub fn with_contract(mut self, address: Addr, contract: ContractInfo) -> Self {
        self.contracts.insert(address, contract);
        self
    }

    pub fn with_raw_contract_storage<F>(mut self, address: Addr, callback: F) -> Self
    where
        F: FnOnce(&mut dyn Storage),
    {
        callback(self.raw_query_handler.get_storage_mut(address));
        self
    }

    pub fn with_smart_query_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(Addr, Json) -> GenericResult<Json> + 'static,
    {
        self.smart_query_handler = Some(Box::new(handler));
        self
    }

    pub fn update_smart_query_handler<F>(&mut self, handler: F)
    where
        F: Fn(Addr, Json) -> GenericResult<Json> + 'static,
    {
        self.smart_query_handler = Some(Box::new(handler));
    }
}

impl Querier for MockQuerier {
    fn query_chain(&self, req: Query) -> StdResult<QueryResponse> {
        match req {
            Query::Status(_req) => {
                let status = self
                    .status
                    .clone()
                    .expect("[MockQuerier]: status is not set");
                Ok(QueryResponse::Status(status))
            },
            Query::Config(_req) => {
                let cfg = self
                    .config
                    .clone()
                    .expect("[MockQuerier]: config is not set");
                Ok(QueryResponse::Config(cfg))
            },
            Query::AppConfig(_req) => {
                let app_cfg = self
                    .app_config
                    .clone()
                    .expect("[MockQuerier]: app config is not set");
                Ok(QueryResponse::AppConfig(app_cfg))
            },
            Query::NextUpgrade(_req) => {
                let next_upgrade = self.next_upgrade.clone();
                Ok(QueryResponse::NextUpgrade(next_upgrade))
            },
            Query::PastUpgrades(req) => {
                let upgrades = self
                    .past_upgrades
                    .iter()
                    .filter(|(height, _)| {
                        if let Some(lower_bound) = &req.start_after {
                            *height > lower_bound
                        } else {
                            true
                        }
                    })
                    .take(req.limit.unwrap_or(u32::MAX) as usize)
                    .map(|(k, v)| (*k, v.clone()))
                    .collect();
                Ok(QueryResponse::PastUpgrades(upgrades))
            },
            Query::Balance(req) => {
                let amount = self
                    .balances
                    .get(&req.address)
                    .and_then(|amounts| amounts.get(&req.denom))
                    .cloned()
                    .unwrap_or(Uint128::ZERO);
                Coin::new(req.denom, amount).map(QueryResponse::Balance)
            },
            Query::Balances(req) => {
                let coins = self
                    .balances
                    .get(&req.address)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(denom, _)| {
                        if let Some(lower_bound) = &req.start_after {
                            denom > lower_bound
                        } else {
                            true
                        }
                    })
                    .take(req.limit.unwrap_or(u32::MAX) as usize)
                    .collect::<BTreeMap<_, _>>()
                    .try_into()?;
                Ok(QueryResponse::Balances(coins))
            },
            Query::Supply(req) => {
                let amount = self
                    .supplies
                    .get(&req.denom)
                    .cloned()
                    .unwrap_or(Uint128::ZERO);
                Coin::new(req.denom, amount).map(QueryResponse::Balance)
            },
            Query::Supplies(req) => {
                let coins = self
                    .supplies
                    .iter()
                    .filter(|(denom, _)| {
                        if let Some(lower_bound) = &req.start_after {
                            *denom > lower_bound
                        } else {
                            true
                        }
                    })
                    .take(req.limit.unwrap_or(u32::MAX) as usize)
                    .map(|(k, v)| (k.clone(), *v))
                    .collect::<BTreeMap<_, _>>()
                    .try_into()?;
                Ok(QueryResponse::Supplies(coins))
            },
            Query::Code(req) => {
                let code = self
                    .codes
                    .get(&req.hash)
                    .cloned()
                    .ok_or_else(|| StdError::data_not_found::<Binary>(req.hash.as_ref()))?;
                Ok(QueryResponse::Code(code))
            },
            Query::Codes(req) => {
                let codes = self
                    .codes
                    .iter()
                    .filter(|(hash, _)| {
                        if let Some(lower_bound) = &req.start_after {
                            *hash > lower_bound
                        } else {
                            true
                        }
                    })
                    .take(req.limit.unwrap_or(u32::MAX) as usize)
                    .map(|(k, v)| (*k, v.clone()))
                    .collect();
                Ok(QueryResponse::Codes(codes))
            },
            Query::Contract(req) => {
                let contract = self.contracts.get(&req.address).cloned().ok_or_else(|| {
                    StdError::data_not_found::<ContractInfo>(req.address.as_ref())
                })?;
                Ok(QueryResponse::Contract(contract))
            },
            Query::Contracts(req) => {
                let contracts = self
                    .contracts
                    .iter()
                    .filter(|(address, _)| {
                        if let Some(lower_bound) = &req.start_after {
                            *address > lower_bound
                        } else {
                            true
                        }
                    })
                    .take(req.limit.unwrap_or(u32::MAX) as usize)
                    .map(|(k, v)| (*k, v.clone()))
                    .collect();
                Ok(QueryResponse::Contracts(contracts))
            },
            Query::WasmRaw(req) => {
                let maybe_value = self
                    .raw_query_handler
                    .get_storage(req.contract)
                    .read(&req.key)
                    .map(Binary::from_inner);
                Ok(QueryResponse::WasmRaw(maybe_value))
            },
            Query::WasmScan(req) => {
                let records = self
                    .raw_query_handler
                    .get_storage(req.contract)
                    .scan(req.min.as_deref(), req.max.as_deref(), Order::Ascending)
                    .take(req.limit.unwrap_or(u32::MAX) as usize)
                    .map(|(k, v)| (Binary::from_inner(k), Binary::from_inner(v)))
                    .collect();
                Ok(QueryResponse::WasmScan(records))
            },
            Query::WasmSmart(req) => {
                let handler = self
                    .smart_query_handler
                    .as_ref()
                    .expect("[MockQuerier]: smart query handler not set");
                let response = handler(req.contract, req.msg).map_err(StdError::Host)?;
                Ok(QueryResponse::WasmSmart(response))
            },
            Query::Multi(reqs) => {
                let responses = reqs
                    .into_iter()
                    .map(|req| self.query_chain(req).into_generic_result())
                    .collect::<Vec<_>>();
                Ok(QueryResponse::Multi(responses))
            },
        }
    }
}

// ----------------------------- raw query handler -----------------------------

#[derive(Default)]
struct MockRawQueryHandler {
    storages: BTreeMap<Addr, MockStorage>,
}

impl MockRawQueryHandler {
    pub fn get_storage(&self, address: Addr) -> &MockStorage {
        self.storages.get(&address).unwrap_or_else(|| {
            panic!("[MockQuerier]: raw query handler not set for {address}");
        })
    }

    pub fn get_storage_mut(&mut self, address: Addr) -> &mut MockStorage {
        self.storages.entry(address).or_default()
    }
}
