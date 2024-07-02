use {
    crate::{
        call_in_1_out_1, AppError, AppResult, GasTracker, StorageProvider, Vm, ACCOUNTS, CHAIN_ID,
        CODES, CONFIG, CONTRACT_NAMESPACE, LAST_FINALIZED_BLOCK,
    },
    grug_storage::Bound,
    grug_types::{
        AccountResponse, Addr, BankQuery, BankQueryResponse, Binary, BlockInfo, Coin, Coins,
        Context, GenericResult, Hash, InfoResponse, Json, Order, StdResult, Storage,
        WasmRawResponse, WasmSmartResponse,
    },
};

const DEFAULT_PAGE_LIMIT: u32 = 30;

pub fn query_info(storage: &dyn Storage) -> AppResult<InfoResponse> {
    Ok(InfoResponse {
        chain_id: CHAIN_ID.load(storage)?,
        config: CONFIG.load(storage)?,
        last_finalized_block: LAST_FINALIZED_BLOCK.load(storage)?,
    })
}

pub fn query_balance<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: GasTracker,
    address: Addr,
    denom: String,
) -> AppResult<Coin>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    _query_bank(vm, storage, block, gas_tracker, &BankQuery::Balance {
        address,
        denom,
    })
    .map(|res| res.as_balance())
}

pub fn query_balances<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: GasTracker,
    address: Addr,
    start_after: Option<String>,
    limit: Option<u32>,
) -> AppResult<Coins>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    _query_bank(vm, storage, block, gas_tracker, &BankQuery::Balances {
        address,
        start_after,
        limit,
    })
    .map(|res| res.as_balances())
}

pub fn query_supply<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: GasTracker,
    denom: String,
) -> AppResult<Coin>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    _query_bank(vm, storage, block, gas_tracker, &BankQuery::Supply {
        denom,
    })
    .map(|res| res.as_supply())
}

pub fn query_supplies<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: GasTracker,
    start_after: Option<String>,
    limit: Option<u32>,
) -> AppResult<Coins>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    _query_bank(vm, storage, block, gas_tracker, &BankQuery::Supplies {
        start_after,
        limit,
    })
    .map(|res| res.as_supplies())
}

fn _query_bank<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: GasTracker,
    msg: &BankQuery,
) -> AppResult<BankQueryResponse>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    let chain_id = CHAIN_ID.load(&storage)?;
    let cfg = CONFIG.load(&storage)?;
    let account = ACCOUNTS.load(&storage, &cfg.bank)?;
    let ctx = Context {
        chain_id,
        block,
        contract: cfg.bank,
        sender: None,
        funds: None,
        simulate: None,
    };

    call_in_1_out_1::<_, _, GenericResult<BankQueryResponse>>(
        vm,
        storage,
        gas_tracker,
        "bank_query",
        &account.code_hash,
        &ctx,
        true,
        msg,
    )?
    .into_std_result()
    .map_err(AppError::Std)
}

pub fn query_code(storage: &dyn Storage, hash: Hash) -> AppResult<Binary> {
    Ok(CODES.load(storage, &hash)?.into())
}

pub fn query_codes(
    storage: &dyn Storage,
    start_after: Option<Hash>,
    limit: Option<u32>,
) -> AppResult<Vec<Hash>> {
    let start = start_after.as_ref().map(Bound::exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);

    CODES
        .keys(storage, start, None, Order::Ascending)
        .take(limit as usize)
        .collect::<StdResult<Vec<_>>>()
        .map_err(Into::into)
}

pub fn query_account(storage: &dyn Storage, address: Addr) -> AppResult<AccountResponse> {
    let account = ACCOUNTS.load(storage, &address)?;
    Ok(AccountResponse {
        address,
        code_hash: account.code_hash,
        admin: account.admin,
    })
}

pub fn query_accounts(
    storage: &dyn Storage,
    start_after: Option<Addr>,
    limit: Option<u32>,
) -> AppResult<Vec<AccountResponse>> {
    let start = start_after.as_ref().map(Bound::exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);

    ACCOUNTS
        .range(storage, start, None, Order::Ascending)
        .take(limit as usize)
        .map(|item| {
            let (address, account) = item?;
            Ok(AccountResponse {
                address,
                code_hash: account.code_hash,
                admin: account.admin,
            })
        })
        .collect()
}

pub fn query_wasm_raw(
    storage: Box<dyn Storage>,
    contract: Addr,
    key: Binary,
) -> AppResult<WasmRawResponse> {
    let substore = StorageProvider::new(storage, &[CONTRACT_NAMESPACE, &contract]);
    let value = substore.read(&key);
    Ok(WasmRawResponse {
        contract,
        key,
        value: value.map(Binary::from),
    })
}

pub fn query_wasm_smart<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: GasTracker,
    contract: Addr,
    msg: Json,
) -> AppResult<WasmSmartResponse>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    let chain_id = CHAIN_ID.load(&storage)?;
    let account = ACCOUNTS.load(&storage, &contract)?;
    let ctx = Context {
        chain_id,
        block,
        contract,
        sender: None,
        funds: None,
        simulate: None,
    };
    let data = call_in_1_out_1::<_, _, GenericResult<Json>>(
        vm,
        storage,
        gas_tracker,
        "query",
        &account.code_hash,
        &ctx,
        true,
        &msg,
    )?
    .into_std_result()?;

    Ok(WasmSmartResponse {
        contract: ctx.contract,
        data,
    })
}
