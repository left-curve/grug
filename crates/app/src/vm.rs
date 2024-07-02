use {
    crate::{
        handle_submessages, AppError, AppResult, Instance, QuerierProvider, SharedGasTracker,
        StorageProvider, Vm, CODES, CONTRACT_ADDRESS_KEY, CONTRACT_NAMESPACE,
    },
    grug_types::{
        from_json_slice, to_json_vec, Addr, BlockInfo, Context, Event, GenericResult, Hash,
        Response, Storage,
    },
    serde::{de::DeserializeOwned, ser::Serialize},
};

/// Create a VM instance, and call a function that takes no input parameter and
/// returns one output.
pub fn call_in_0_out_1<VM, R>(
    vm: VM,
    name: &'static str,
    storage: Box<dyn Storage>,
    code_hash: &Hash,
    ctx: &Context,
    gas_tracker: SharedGasTracker,
) -> AppResult<R>
where
    R: DeserializeOwned,
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    // Create the VM instance
    let instance = create_vm_instance(
        vm,
        storage,
        ctx.block.clone(),
        &ctx.contract,
        code_hash,
        gas_tracker,
    )?;

    // Call the function; deserialize the output as JSON
    let out_raw = instance.call_in_0_out_1(name, ctx)?;
    let out = from_json_slice(out_raw)?;

    Ok(out)
}

/// Create a VM instance, and call a function that takes exactly one parameter
/// and returns one output.
pub fn call_in_1_out_1<VM, P, R>(
    vm: VM,
    name: &'static str,
    storage: Box<dyn Storage>,
    code_hash: &Hash,
    ctx: &Context,
    gas_tracker: SharedGasTracker,
    param: &P,
) -> AppResult<R>
where
    P: Serialize,
    R: DeserializeOwned,
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    // Create the VM instance
    let instance = create_vm_instance(
        vm,
        storage,
        ctx.block.clone(),
        &ctx.contract,
        code_hash,
        gas_tracker,
    )?;

    // Serialize the param as JSON
    let param_raw = to_json_vec(param)?;

    // Call the function; deserialize the output as JSON
    let out_raw = instance.call_in_1_out_1(name, ctx, &param_raw)?;
    let out = from_json_slice(out_raw)?;

    Ok(out)
}

/// Create a VM instance, and call a function that takes exactly two parameters
/// and returns one output.
pub fn call_in_2_out_1<VM, P1, P2, R>(
    vm: VM,
    name: &'static str,
    storage: Box<dyn Storage>,
    code_hash: &Hash,
    ctx: &Context,
    gas_tracker: SharedGasTracker,
    param1: &P1,
    param2: &P2,
) -> AppResult<R>
where
    P1: Serialize,
    P2: Serialize,
    R: DeserializeOwned,
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    // Create the VM instance
    let instance = create_vm_instance(
        vm,
        storage,
        ctx.block.clone(),
        &ctx.contract,
        code_hash,
        gas_tracker,
    )?;

    // Serialize the params as JSON
    let param1_raw = to_json_vec(param1)?;
    let param2_raw = to_json_vec(param2)?;

    // Call the function; deserialize the output as JSON
    let out_raw = instance.call_in_2_out_1(name, ctx, &param1_raw, &param2_raw)?;
    let out = from_json_slice(out_raw)?;

    Ok(out)
}

/// Create a VM instance, call a function that takes exactly no input parameter
/// and returns [`Response`], and handle the submessages. Return a vector of
/// events emitted.
pub fn call_in_0_out_1_handle_response<VM>(
    vm: VM,
    name: &'static str,
    storage: Box<dyn Storage>,
    code_hash: &Hash,
    ctx: &Context,
    gas_tracker: SharedGasTracker,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    let response = call_in_0_out_1::<_, GenericResult<Response>>(
        vm.clone(),
        name,
        storage.clone(),
        code_hash,
        ctx,
        gas_tracker.clone(),
    )?
    .into_std_result()?;

    handle_response(vm, name, storage, ctx, gas_tracker, response)
}

/// Create a VM instance, call a function that takes exactly one parameter and
/// returns [`Response`], and handle the submessages. Return a vector of events
/// emitted.
pub fn call_in_1_out_1_handle_response<VM, P>(
    vm: VM,
    name: &'static str,
    storage: Box<dyn Storage>,
    code_hash: &Hash,
    ctx: &Context,
    gas_tracker: SharedGasTracker,
    param: &P,
) -> AppResult<Vec<Event>>
where
    P: Serialize,
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    let response = call_in_1_out_1::<_, _, GenericResult<Response>>(
        vm.clone(),
        name,
        storage.clone(),
        code_hash,
        ctx,
        gas_tracker.clone(),
        param,
    )?
    .into_std_result()?;

    handle_response(vm, name, storage, ctx, gas_tracker, response)
}

/// Create a VM instance, call a function that takes exactly two parameter and
/// returns [`Response`], and handle the submessages. Return a vector of events
/// emitted.
pub fn call_in_2_out_1_handle_response<VM, P1, P2>(
    vm: VM,
    name: &'static str,
    storage: Box<dyn Storage>,
    code_hash: &Hash,
    ctx: &Context,
    gas_tracker: SharedGasTracker,
    param1: &P1,
    param2: &P2,
) -> AppResult<Vec<Event>>
where
    P1: Serialize,
    P2: Serialize,
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    let response = call_in_2_out_1::<_, _, _, GenericResult<Response>>(
        vm.clone(),
        name,
        storage.clone(),
        code_hash,
        ctx,
        gas_tracker.clone(),
        param1,
        param2,
    )?
    .into_std_result()?;

    handle_response(vm, name, storage, ctx, gas_tracker, response)
}

fn create_vm_instance<VM>(
    mut vm: VM,
    storage: Box<dyn Storage>,
    block: BlockInfo,
    address: &Addr,
    code_hash: &Hash,
    gas_tracker: SharedGasTracker,
) -> AppResult<VM::Instance>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    // Load the program code from storage and deserialize
    let code = CODES.load(&storage, code_hash)?;

    // Create the providers
    let querier = QuerierProvider::new(vm.clone(), storage.clone(), block, gas_tracker.clone());
    let storage = StorageProvider::new(storage, &[CONTRACT_NAMESPACE, address]);

    Ok(vm.build_instance(storage, querier, &code, gas_tracker)?)
}

pub(crate) fn handle_response<VM>(
    vm: VM,
    name: &'static str,
    storage: Box<dyn Storage>,
    ctx: &Context,
    gas_tracker: SharedGasTracker,
    response: Response,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    // Create an event for this call
    let event = Event::new(name)
        .add_attribute(CONTRACT_ADDRESS_KEY, &ctx.contract)
        .add_attributes(response.attributes);

    // Handle submessages; append events emitted during submessage handling
    let mut events = vec![event];
    events.extend(handle_submessages(
        vm,
        storage,
        ctx.block.clone(),
        gas_tracker,
        ctx.contract.clone(),
        response.submsgs,
    )?);

    Ok(events)
}
