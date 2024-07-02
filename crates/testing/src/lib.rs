use {
    grug_app::App,
    grug_db_memory::MemDb,
    grug_types::{BlockInfo, GenesisState, Hash, QueryRequest, QueryResponse, Timestamp, Uint64},
    grug_vm_rust::RustVm,
    std::time::{SystemTime, UNIX_EPOCH},
};

fn current_time() -> Timestamp {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("failed to get current system time")
        .as_nanos()
        .try_into()
        .expect("current system time overflows u64");
    Timestamp::from_nanos(nanos)
}

pub struct MockApp {
    inner: App<MemDb, RustVm>,
}

// need to implement this to make clippy not complain
// TODO: create a clippy.toml to disable this
impl Default for MockApp {
    fn default() -> Self {
        Self::new()
    }
}

impl MockApp {
    pub fn new() -> Self {
        Self {
            inner: App::new(MemDb::new(), RustVm::new(), None),
        }
    }

    pub fn init_chain(&mut self, chain_id: impl ToString, genesis_state: GenesisState) {
        let block = BlockInfo {
            height: Uint64::new(0), // genesis height is always zero
            timestamp: current_time(),
            hash: Hash::ZERO,
        };
        self.inner
            .do_init_chain(chain_id.to_string(), block, genesis_state)
            .unwrap();
    }

    pub fn query(&self, req: QueryRequest) -> QueryResponse {
        self.inner.do_query_app(req, 0, false).unwrap()
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        grug_types::{
            hash, to_json_value, Addr, Coins, Config, Empty, Message, MutableCtx, Permission,
            Permissions, Response, StdResult,
        },
        grug_vm_rust::{ContractWrapper, ExecuteFn, MigrateFn, QueryFn, ReceiveFn, ReplyFn},
        std::collections::BTreeSet,
    };

    fn bank_instantiate(_ctx: MutableCtx, _msg: Empty) -> StdResult<Response> {
        Ok(Response::new().add_attribute("action", "bank_instantiate"))
    }

    #[test]
    fn init_chain_works() {
        let mut app = MockApp::new();
        let bank_contract = ContractWrapper::new(
            Box::new(bank_instantiate),
            None::<ExecuteFn>,
            None::<MigrateFn>,
            None::<ReceiveFn>,
            None::<ReplyFn>,
            None::<QueryFn>,
        );
        let bank_code = bank_contract.into_bytes();
        let bank_code_hash = hash(&bank_code);
        let genesis_state = GenesisState {
            config: Config {
                owner: None,
                bank: Addr::mock(1),
                begin_blockers: vec![],
                end_blockers: vec![],
                permissions: Permissions {
                    upload: Permission::Everybody,
                    instantiate: Permission::Everybody,
                    create_client: Permission::Everybody,
                    create_connection: Permission::Everybody,
                    create_channel: Permission::Everybody,
                },
                allowed_clients: BTreeSet::new(),
            },
            msgs: vec![
                Message::Upload {
                    code: bank_code.into(),
                },
                Message::Instantiate {
                    code_hash: bank_code_hash,
                    msg: to_json_value(&Empty {}).unwrap(),
                    salt: b"bank".to_vec().into(),
                    funds: Coins::new_empty(),
                    admin: None,
                },
            ],
        };
        app.init_chain("dev-1", genesis_state);

        let info = app.query(QueryRequest::Info {}).as_info();
        dbg!(&info);

        let code_hashes = app
            .query(QueryRequest::Codes {
                start_after: None,
                limit: None,
            })
            .as_codes();
        dbg!(&code_hashes);

        let accounts = app
            .query(QueryRequest::Accounts {
                start_after: None,
                limit: None,
            })
            .as_accounts();
        dbg!(&accounts);
    }
}
