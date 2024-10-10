// TODO: replace with https://doc.rust-lang.org/std/ops/trait.Try.html once stabilized
#[macro_export]
#[doc(hidden)]
macro_rules! unwrap_into_generic_result {
    ($expr:expr) => {
        match $expr {
            Ok(val) => val,
            Err(err) => {
                return GenericResult::Err(err.to_string());
            },
        }
    };
}

#[macro_export]
#[doc(hidden)]
#[rustfmt::skip]
macro_rules! make_immutable_ctx {
    ($ctx:ident, $storage:expr, $api:expr, $querier:expr) => {
        {
            debug_assert!($ctx.sender.is_none());
            debug_assert!($ctx.funds.is_none());
            debug_assert!($ctx.mode.is_none());

            ImmutableCtx {
                storage:  $storage,
                api:      $api,
                querier:  QuerierWrapper::new($querier),
                chain_id: $ctx.chain_id,
                block:    $ctx.block,
                contract: $ctx.contract,
            }
        }
    };
}

#[macro_export]
#[doc(hidden)]
#[rustfmt::skip]
macro_rules! make_mutable_ctx {
    ($ctx:ident, $storage:expr, $api:expr, $querier:expr) => {
        {
            debug_assert!($ctx.mode.is_none());

            MutableCtx {
                storage:  $storage,
                api:      $api,
                querier:  QuerierWrapper::new($querier),
                chain_id: $ctx.chain_id,
                block:    $ctx.block,
                contract: $ctx.contract,
                sender:   $ctx.sender.unwrap(),
                funds:    $ctx.funds.unwrap(),
            }
        }
    };
}

#[macro_export]
#[doc(hidden)]
#[rustfmt::skip]
macro_rules! make_sudo_ctx {
    ($ctx:ident, $storage:expr, $api:expr, $querier:expr) => {
        {
            debug_assert!($ctx.sender.is_none());
            debug_assert!($ctx.funds.is_none());
            debug_assert!($ctx.mode.is_none());

            SudoCtx {
                storage:  $storage,
                api:      $api,
                querier:  QuerierWrapper::new($querier),
                chain_id: $ctx.chain_id,
                block:    $ctx.block,
                contract: $ctx.contract,
            }
        }
    };
}

#[macro_export]
#[doc(hidden)]
#[rustfmt::skip]
macro_rules! make_auth_ctx {
    ($ctx:ident, $storage:expr, $api:expr, $querier:expr) => {
        {
            debug_assert!($ctx.sender.is_none());
            debug_assert!($ctx.funds.is_none());

            AuthCtx {
                storage:  $storage,
                api:      $api,
                querier:  QuerierWrapper::new($querier),
                chain_id: $ctx.chain_id,
                block:    $ctx.block,
                contract: $ctx.contract,
                mode:     $ctx.mode.unwrap(),
            }
        }
    };
}
