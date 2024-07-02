use {
    crate::{ContractWrapper, VmError, VmResult, CONTRACTS},
    grug_app::{Instance, QuerierProvider, SharedGasTracker, StorageProvider, Vm},
    grug_types::{from_json_slice, to_json_vec, Context, MockApi},
};

macro_rules! get_contract {
    ($index:expr) => {
        CONTRACTS.get().and_then(|contracts| contracts.get($index)).unwrap_or_else(|| {
            panic!("can't find contract with index {}", $index); // TODO: throw an VmError instead of panicking?
        })
    }
}

#[derive(Default, Clone)]
pub struct RustVm;

impl RustVm {
    pub fn new() -> Self {
        Self
    }
}

impl Vm for RustVm {
    type Error = VmError;
    type Instance = RustInstance;

    fn build_instance(
        &mut self,
        storage: StorageProvider,
        querier: QuerierProvider<Self>,
        code: &[u8],
        _gas_tracker: SharedGasTracker,
    ) -> VmResult<RustInstance> {
        Ok(RustInstance {
            storage,
            querier,
            wrapper: ContractWrapper::from_bytes(code),
            // TODO: add gas tracker
        })
    }
}

pub struct RustInstance {
    storage: StorageProvider,
    querier: QuerierProvider<RustVm>,
    wrapper: ContractWrapper,
    // TODO: add gas tracker
}

impl Instance for RustInstance {
    type Error = VmError;

    fn call_in_0_out_1(mut self, name: &str, ctx: &Context) -> VmResult<Vec<u8>> {
        let contract = get_contract!(self.wrapper.index);
        let out = match name {
            "receive" => {
                let res = contract.receive(ctx.clone(), &mut self.storage, &MockApi, &self.querier);
                to_json_vec(&res)?
            },
            _ => {
                return Err(VmError::IncorrectNumberOfInputs {
                    name: name.into(),
                    num: 0,
                })
            },
        };
        Ok(out)
    }

    fn call_in_1_out_1<P>(mut self, name: &str, ctx: &Context, param: &P) -> VmResult<Vec<u8>>
    where
        P: AsRef<[u8]>,
    {
        let contract = get_contract!(self.wrapper.index);
        let out = match name {
            "instantiate" => {
                let msg = from_json_slice(param)?;
                let res = contract.instantiate(
                    ctx.clone(),
                    &mut self.storage,
                    &MockApi,
                    &self.querier,
                    msg,
                );
                to_json_vec(&res)?
            },
            "execute" => {
                let msg = from_json_slice(param)?;
                let res =
                    contract.execute(ctx.clone(), &mut self.storage, &MockApi, &self.querier, msg);
                to_json_vec(&res)?
            },
            "migrate" => {
                let msg = from_json_slice(param)?;
                let res =
                    contract.migrate(ctx.clone(), &mut self.storage, &MockApi, &self.querier, msg);
                to_json_vec(&res)?
            },
            "query" => {
                let msg = from_json_slice(param)?;
                let res = contract.query(ctx.clone(), &self.storage, &MockApi, &self.querier, msg);
                to_json_vec(&res)?
            },
            _ => {
                return Err(VmError::IncorrectNumberOfInputs {
                    name: name.into(),
                    num: 1,
                })
            },
        };
        Ok(out)
    }

    fn call_in_2_out_1<P1, P2>(
        mut self,
        name: &str,
        ctx: &Context,
        param1: &P1,
        param2: &P2,
    ) -> VmResult<Vec<u8>>
    where
        P1: AsRef<[u8]>,
        P2: AsRef<[u8]>,
    {
        let contract = get_contract!(self.wrapper.index);
        let out = match name {
            "reply" => {
                let msg = from_json_slice(param1)?;
                let submsg_res = from_json_slice(param2)?;
                let res = contract.reply(
                    ctx.clone(),
                    &mut self.storage,
                    &MockApi,
                    &self.querier,
                    msg,
                    submsg_res,
                );
                to_json_vec(&res)?
            },
            _ => {
                return Err(VmError::IncorrectNumberOfInputs {
                    name: name.into(),
                    num: 2,
                })
            },
        };
        Ok(out)
    }
}
