#[cfg(feature = "tracing")]
use tracing::{debug, info, warn};
use {
    crate::{
        call_in_0_out_1_handle_response, call_in_1_out_1_handle_response,
        call_in_2_out_1_handle_response, has_permission, AppError, AppResult, SharedGasTracker, Vm,
        ACCOUNTS, CHAIN_ID, CODES, CONFIG,
    },
    grug_types::{
        hash, Account, Addr, BankMsg, Binary, BlockInfo, Coins, Config, Context, Event, Hash, Json,
        Storage, SubMsgResult, Tx,
    },
};

// ---------------------------------- config -----------------------------------

pub fn do_set_config(
    storage: &mut dyn Storage,
    sender: &Addr,
    new_cfg: &Config,
) -> AppResult<Vec<Event>> {
    match _do_set_config(storage, sender, new_cfg) {
        Ok(event) => {
            #[cfg(feature = "tracing")]
            info!("Config set");
            Ok(vec![event])
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            warn!(err = err.to_string(), "Failed to set config");
            Err(err)
        },
    }
}

fn _do_set_config(storage: &mut dyn Storage, sender: &Addr, new_cfg: &Config) -> AppResult<Event> {
    // make sure the sender is authorized to set the config
    let cfg = CONFIG.load(storage)?;
    let Some(owner) = cfg.owner else {
        return Err(AppError::OwnerNotSet);
    };
    if sender != owner {
        return Err(AppError::NotOwner {
            sender: sender.clone(),
            owner,
        });
    }

    // save the new config
    CONFIG.save(storage, new_cfg)?;

    Ok(Event::new("set_config").add_attribute("sender", sender))
}

// ---------------------------------- upload -----------------------------------

pub fn do_upload(
    storage: &mut dyn Storage,
    uploader: &Addr,
    code: Vec<u8>,
) -> AppResult<Vec<Event>> {
    match _do_upload(storage, uploader, code) {
        Ok((event, _code_hash)) => {
            #[cfg(feature = "tracing")]
            info!(code_hash = _code_hash.to_string(), "Uploaded code");
            Ok(vec![event])
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            warn!(err = err.to_string(), "Failed to upload code");
            Err(err)
        },
    }
}

// Return the hash of the code that is stored, for logging purpose.
fn _do_upload(
    storage: &mut dyn Storage,
    uploader: &Addr,
    code: Vec<u8>,
) -> AppResult<(Event, Hash)> {
    // Make sure the user has the permission to upload contracts
    let cfg = CONFIG.load(storage)?;
    if !has_permission(&cfg.permissions.upload, cfg.owner.as_ref(), uploader) {
        return Err(AppError::Unauthorized);
    }

    // Make sure that the same code isn't already uploaded
    let code_hash = hash(&code);
    if CODES.has(storage, &code_hash) {
        return Err(AppError::CodeExists { code_hash });
    }

    CODES.save(storage, &code_hash, &code)?;

    Ok((
        Event::new("upload").add_attribute("code_hash", &code_hash),
        code_hash,
    ))
}

// --------------------------------- transfer ----------------------------------

pub fn do_transfer<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    from: Addr,
    to: Addr,
    coins: Coins,
    receive: bool,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    match _do_transfer(
        vm,
        storage,
        block,
        gas_tracker,
        from.clone(),
        to.clone(),
        coins.clone(),
        receive,
    ) {
        Ok(events) => {
            #[cfg(feature = "tracing")]
            info!(
                from = from.to_string(),
                to = to.to_string(),
                coins = coins.to_string(),
                "Transferred coins"
            );
            Ok(events)
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            warn!(err = err.to_string(), "Failed to transfer coins");
            Err(err)
        },
    }
}

fn _do_transfer<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    from: Addr,
    to: Addr,
    coins: Coins,
    // Whether to call the receipient account's `receive` entry point following
    // the transfer, to inform it that the transfer has happened.
    // - `true` when handling `Message::Transfer`
    // - `false` when handling `Message::{Instantaite,Execute}`
    do_receive: bool,
) -> AppResult<Vec<Event>>
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
    let msg = BankMsg { from, to, coins };

    let mut events = call_in_1_out_1_handle_response(
        vm.clone(),
        "bank_execute",
        storage.clone(),
        &account.code_hash,
        &ctx,
        gas_tracker.clone(),
        &msg,
    )?;

    if do_receive {
        events.extend(_do_receive(vm, storage, ctx.block, gas_tracker, msg)?);
    }

    Ok(events)
}

fn _do_receive<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    msg: BankMsg,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    let chain_id = CHAIN_ID.load(&storage)?;
    let account = ACCOUNTS.load(&storage, &msg.to)?;
    let ctx = Context {
        chain_id,
        block,
        contract: msg.to,
        sender: Some(msg.from),
        funds: Some(msg.coins),
        simulate: None,
    };

    call_in_0_out_1_handle_response(
        vm,
        "receive",
        storage,
        &account.code_hash,
        &ctx,
        gas_tracker,
    )
}

// -------------------------------- instantiate --------------------------------

pub fn do_instantiate<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    sender: Addr,
    code_hash: Hash,
    msg: &Json,
    salt: Binary,
    funds: Coins,
    admin: Option<Addr>,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    match _do_instantiate(
        vm,
        storage,
        block,
        gas_tracker,
        sender,
        code_hash,
        msg,
        salt,
        funds,
        admin,
    ) {
        Ok((events, _address)) => {
            #[cfg(feature = "tracing")]
            info!(address = _address.to_string(), "Instantiated contract");
            Ok(events)
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            warn!(err = err.to_string(), "Failed to instantiate contract");
            Err(err)
        },
    }
}

pub fn _do_instantiate<VM>(
    vm: VM,
    mut storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    sender: Addr,
    code_hash: Hash,
    msg: &Json,
    salt: Binary,
    funds: Coins,
    admin: Option<Addr>,
) -> AppResult<(Vec<Event>, Addr)>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    let chain_id = CHAIN_ID.load(&storage)?;

    // Make sure the user has the permission to instantiate contracts
    let cfg = CONFIG.load(&storage)?;
    if !has_permission(&cfg.permissions.instantiate, cfg.owner.as_ref(), &sender) {
        return Err(AppError::Unauthorized);
    }

    // Compute the contract address, and make sure there isn't already an
    // account of the same address.
    let address = Addr::compute(&sender, &code_hash, &salt);
    if ACCOUNTS.has(&storage, &address) {
        return Err(AppError::AccountExists { address });
    }

    // Save the account info
    let account = Account { code_hash, admin };
    ACCOUNTS.save(&mut storage, &address, &account)?;

    // Make the fund transfer
    let mut events = vec![];
    if !funds.is_empty() {
        events.extend(_do_transfer(
            vm.clone(),
            storage.clone(),
            block.clone(),
            gas_tracker.clone(),
            sender.clone(),
            address.clone(),
            funds.clone(),
            false,
        )?);
    }

    // Call the contract's `instantiate` entry point
    let ctx = Context {
        chain_id,
        block,
        contract: address,
        sender: Some(sender),
        funds: Some(funds),
        simulate: None,
    };
    events.extend(call_in_1_out_1_handle_response(
        vm,
        "instantiate",
        storage,
        &account.code_hash,
        &ctx,
        gas_tracker,
        msg,
    )?);

    Ok((events, ctx.contract))
}

// ---------------------------------- execute ----------------------------------

pub fn do_execute<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    contract: Addr,
    sender: Addr,
    msg: &Json,
    funds: Coins,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    match _do_execute(
        vm,
        storage,
        block,
        gas_tracker,
        contract.clone(),
        sender,
        msg,
        funds,
    ) {
        Ok(events) => {
            #[cfg(feature = "tracing")]
            info!(contract = contract.to_string(), "Executed contract");
            Ok(events)
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            warn!(err = err.to_string(), "Failed to execute contract");
            Err(err)
        },
    }
}

fn _do_execute<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    contract: Addr,
    sender: Addr,
    msg: &Json,
    funds: Coins,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    let chain_id = CHAIN_ID.load(&storage)?;
    let account = ACCOUNTS.load(&storage, &contract)?;

    // Make the fund transfer
    let mut events = vec![];
    if !funds.is_empty() {
        events.extend(_do_transfer(
            vm.clone(),
            storage.clone(),
            block.clone(),
            gas_tracker.clone(),
            sender.clone(),
            contract.clone(),
            funds.clone(),
            false,
        )?);
    }

    // Call the contract's `execute` entry point
    let ctx = Context {
        chain_id,
        block,
        contract,
        sender: Some(sender),
        funds: Some(funds),
        simulate: None,
    };
    events.extend(call_in_1_out_1_handle_response(
        vm,
        "execute",
        storage,
        &account.code_hash,
        &ctx,
        gas_tracker,
        msg,
    )?);

    Ok(events)
}

// ---------------------------------- migrate ----------------------------------

pub fn do_migrate<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    contract: Addr,
    sender: Addr,
    new_code_hash: Hash,
    msg: &Json,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    match _do_migrate(
        vm,
        storage,
        block,
        gas_tracker,
        contract.clone(),
        sender,
        new_code_hash,
        msg,
    ) {
        Ok(events) => {
            #[cfg(feature = "tracing")]
            info!(contract = contract.to_string(), "Migrated contract");
            Ok(events)
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            warn!(err = err.to_string(), "Failed to execute contract");
            Err(err)
        },
    }
}

fn _do_migrate<VM>(
    vm: VM,
    mut storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    contract: Addr,
    sender: Addr,
    new_code_hash: Hash,
    msg: &Json,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    let chain_id = CHAIN_ID.load(&storage)?;
    let mut account = ACCOUNTS.load(&storage, &contract)?;

    // Only the account's admin can migrate it
    let Some(admin) = &account.admin else {
        return Err(AppError::AdminNotSet);
    };
    if sender != admin {
        return Err(AppError::NotAdmin {
            sender,
            admin: account.admin.unwrap(),
        });
    }

    // Update account info and save
    account.code_hash = new_code_hash;
    ACCOUNTS.save(&mut storage, &contract, &account)?;

    let ctx = Context {
        chain_id,
        block,
        contract,
        sender: Some(sender),
        funds: None,
        simulate: None,
    };

    call_in_1_out_1_handle_response(
        vm,
        "migrate",
        storage,
        &account.code_hash,
        &ctx,
        gas_tracker,
        msg,
    )
}

// ----------------------------------- reply -----------------------------------

pub fn do_reply<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    contract: Addr,
    msg: &Json,
    result: &SubMsgResult,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    match _do_reply(
        vm,
        storage,
        block,
        gas_tracker,
        contract.clone(),
        msg,
        result,
    ) {
        Ok(events) => {
            #[cfg(feature = "tracing")]
            info!(contract = contract.to_string(), "Performed callback");
            Ok(events)
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            warn!(err = err.to_string(), "Failed to perform callback");
            Err(err)
        },
    }
}

fn _do_reply<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    contract: Addr,
    msg: &Json,
    result: &SubMsgResult,
) -> AppResult<Vec<Event>>
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

    call_in_2_out_1_handle_response(
        vm,
        "reply",
        storage,
        &account.code_hash,
        &ctx,
        gas_tracker,
        msg,
        result,
    )
}

// ------------------------- before/after transaction --------------------------

pub fn do_before_tx<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    tx: &Tx,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    match _do_before_or_after_tx(vm, "before_tx", storage, block, gas_tracker, tx) {
        Ok(events) => {
            // TODO: add txhash here?
            #[cfg(feature = "tracing")]
            debug!(
                sender = tx.sender.to_string(),
                "Called before transaction hook"
            );
            Ok(events)
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            warn!(
                err = err.to_string(),
                "Failed to call before transaction hook"
            );
            Err(err)
        },
    }
}

pub fn do_after_tx<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    tx: &Tx,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    match _do_before_or_after_tx(vm, "after_tx", storage, block, gas_tracker, tx) {
        Ok(events) => {
            // TODO: add txhash here?
            #[cfg(feature = "tracing")]
            debug!(
                sender = tx.sender.to_string(),
                "Called after transaction hook"
            );
            Ok(events)
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            warn!(
                err = err.to_string(),
                "Failed to call after transaction hook"
            );
            Err(err)
        },
    }
}

fn _do_before_or_after_tx<VM>(
    vm: VM,
    name: &'static str,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    tx: &Tx,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    let chain_id = CHAIN_ID.load(&storage)?;
    let account = ACCOUNTS.load(&storage, &tx.sender)?;
    let ctx = Context {
        chain_id,
        block,
        contract: tx.sender.clone(),
        sender: None,
        funds: None,
        simulate: Some(false),
    };

    call_in_1_out_1_handle_response(vm, name, storage, &account.code_hash, &ctx, gas_tracker, tx)
}

// ---------------------------- before/after block -----------------------------

pub fn do_before_block<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    contract: Addr,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    match _do_before_or_after_block(
        vm,
        "before_block",
        storage,
        block,
        gas_tracker,
        contract.clone(),
    ) {
        Ok(events) => {
            #[cfg(feature = "tracing")]
            info!(contract = contract.to_string(), "Called before block hook");
            Ok(events)
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            warn!(err = err.to_string(), "Failed to call before block hook");
            Err(err)
        },
    }
}

pub fn do_after_block<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    contract: Addr,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    match _do_before_or_after_block(
        vm,
        "after_block",
        storage,
        block,
        gas_tracker,
        contract.clone(),
    ) {
        Ok(events) => {
            #[cfg(feature = "tracing")]
            info!(contract = contract.to_string(), "Called after block hook");
            Ok(events)
        },
        Err(err) => {
            #[cfg(feature = "tracing")]
            warn!(err = err.to_string(), "Failed to call after block hook");
            Err(err)
        },
    }
}

fn _do_before_or_after_block<VM>(
    vm: VM,
    name: &'static str,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    contract: Addr,
) -> AppResult<Vec<Event>>
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

    call_in_0_out_1_handle_response(vm, name, storage, &account.code_hash, &ctx, gas_tracker)
}
