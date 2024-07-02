use {
    crate::{do_reply, process_msg, AppError, AppResult, Buffer, Shared, SharedGasTracker, Vm},
    grug_types::{Addr, BlockInfo, Event, GenericResult, ReplyOn, Storage, SubMessage},
};

/// Recursively execute submessages emitted in a contract response using a
/// depth-first approach.
///
/// Note: The `sender` in this function signature is the contract, i.e. the
/// account that emitted the submessages, not the transaction's sender.
pub fn handle_submessages<VM>(
    vm: VM,
    // This function takes a boxed store instead of using a generic like others.
    //
    // This is because this function is recursive: every layer of recursion, it
    // wraps the store with `Shared<Buffer<S>>`.
    //
    // Although the recursion is guaranteed to be bounded at run time (thanks to
    // gas limit), the compiler can't understand this. The compiler thinks the
    // wrapping can possibly go on infinitely. It would throw this error:
    //
    // > error: reached the recursion limit while instantiating
    // > `process_msg::<Shared<Buffer<Shared<Buffer<Shared<...>>>>>>`
    //
    // To prevent this, we use `Box<dyn Storage>` instead, which is an opaque
    // type, so that the compiler does not think about how many layers of
    // wrapping there are.
    //
    // Another complexity involved here is that we need the store to be clonable.
    // However we can't write `Box<dyn Storage + Clone>` because `Clone` is not
    // an object-safe trait:
    // https://doc.rust-lang.org/reference/items/traits.html#object-safety
    //
    // Instead, we use the `dyn_clone::DynClone` trait:
    // https://docs.rs/dyn-clone/1.0.16/dyn_clone/
    storage: Box<dyn Storage>,
    block: BlockInfo,
    gas_tracker: SharedGasTracker,
    sender: Addr,
    submsgs: Vec<SubMessage>,
) -> AppResult<Vec<Event>>
where
    VM: Vm + Clone,
    AppError: From<VM::Error>,
{
    let mut events = vec![];
    for submsg in submsgs {
        let buffer = Shared::new(Buffer::new(storage.clone(), None));
        let result = process_msg(
            vm.clone(),
            Box::new(buffer.share()),
            block.clone(),
            gas_tracker.clone(),
            sender.clone(),
            submsg.msg,
        );
        match (submsg.reply_on, result) {
            // success - callback requested
            // flush state changes, log events, give callback
            (ReplyOn::Success(payload) | ReplyOn::Always(payload), Result::Ok(submsg_events)) => {
                buffer.disassemble().consume();
                events.extend(submsg_events.clone());
                events.extend(do_reply(
                    vm.clone(),
                    storage.clone(),
                    block.clone(),
                    gas_tracker.clone(),
                    sender.clone(),
                    &payload,
                    &GenericResult::Ok(submsg_events),
                )?);
            },
            // error - callback requested
            // discard uncommitted state changes, give callback
            (ReplyOn::Error(payload) | ReplyOn::Always(payload), Result::Err(err)) => {
                events.extend(do_reply(
                    vm.clone(),
                    storage.clone(),
                    block.clone(),
                    gas_tracker.clone(),
                    sender.clone(),
                    &payload,
                    &GenericResult::Err(err.to_string()),
                )?);
            },
            // success - callback not requested
            // flush state changes, log events, move on to the next submsg
            (ReplyOn::Error(_) | ReplyOn::Never, Result::Ok(submsg_events)) => {
                buffer.disassemble().consume();
                events.extend(submsg_events);
            },
            // error - callback not requested
            // abort by throwing error
            (ReplyOn::Success(_) | ReplyOn::Never, Result::Err(err)) => {
                return Err(err);
            },
        };
    }
    Ok(events)
}
