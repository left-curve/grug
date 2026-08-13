use {
    crate::Codes,
    dango_primitives::{Binary, Empty, StdResult},
    dango_types::config::Hyperlane,
    dango_vm_rust::{ContractBuilder, ContractWrapper, RustVm},
};

/// Get the binary codes for Dango smart contracts, for use in building the
/// genesis state.
pub trait GenesisCodes {
    type Code: Clone + Into<Binary>;

    fn genesis_codes() -> Codes<Self::Code>;
}

impl GenesisCodes for RustVm {
    type Code = ContractWrapper;

    fn genesis_codes() -> Codes<ContractWrapper> {
        let account_factory = ContractBuilder::new(Box::new(dango_account_factory::instantiate))
            .with_execute(Box::new(dango_account_factory::execute))
            .with_query(Box::new(dango_account_factory::query))
            .with_authenticate(Box::new(dango_account_factory::authenticate))
            .build();

        // Previously this was the multisig account code hash, now removed.
        let _account_multi = ContractBuilder::new(Box::new(|_, _: Empty| -> StdResult<_> {
            unreachable!("the multisig contract has been deleted");
        }))
        .build();

        // Note: the order of `build()` calls in this function determines each
        // contract's index in the Rust VM's registry, which is what the chain
        // stores as its code. Adding an entry point to a builder is therefore
        // safe for contracts that are already deployed; reordering the calls
        // is not.
        let account = ContractBuilder::new(Box::new(dango_account::instantiate))
            .with_execute(Box::new(dango_account::execute))
            .with_authenticate(Box::new(dango_account::authenticate))
            .with_receive(Box::new(dango_account::receive))
            .with_query(Box::new(dango_account::query))
            .build();

        let bank = ContractBuilder::new(Box::new(dango_bank::instantiate))
            .with_execute(Box::new(dango_bank::execute))
            .with_query(Box::new(dango_bank::query))
            .with_bank_execute(Box::new(dango_bank::bank_execute))
            .with_bank_query(Box::new(dango_bank::bank_query))
            .build();

        // Previously this was the DEX (spot exchange) code hash, now removed.
        let _dex = ContractBuilder::new(Box::new(|_, _: Empty| -> StdResult<_> {
            unreachable!("the dex contract has been deleted");
        }))
        .build();

        let gateway = ContractBuilder::new(Box::new(dango_gateway::instantiate))
            .with_execute(Box::new(dango_gateway::execute))
            .with_query(Box::new(dango_gateway::query))
            .with_cron_execute(Box::new(dango_gateway::cron_execute))
            .build();

        let ism = ContractBuilder::new(Box::new(dango_hyperlane_ism::instantiate))
            .with_execute(Box::new(dango_hyperlane_ism::execute))
            .with_query(Box::new(dango_hyperlane_ism::query))
            .build();

        let mailbox = ContractBuilder::new(Box::new(dango_hyperlane_mailbox::instantiate))
            .with_execute(Box::new(dango_hyperlane_mailbox::execute))
            .with_query(Box::new(dango_hyperlane_mailbox::query))
            .build();

        let va = ContractBuilder::new(Box::new(dango_hyperlane_va::instantiate))
            .with_execute(Box::new(dango_hyperlane_va::execute))
            .with_query(Box::new(dango_hyperlane_va::query))
            .build();

        let oracle = ContractBuilder::new(Box::new(dango_oracle::instantiate))
            .with_execute(Box::new(dango_oracle::execute))
            .with_authenticate(Box::new(dango_oracle::authenticate))
            .with_query(Box::new(dango_oracle::query))
            .build();

        // Previously this was the taxman code hash, now removed. Gas fees are
        // handled directly by the state machine.
        let _taxman = ContractBuilder::new(Box::new(|_, _: Empty| -> StdResult<_> {
            unreachable!("the taxman contract has been deleted");
        }))
        .build();

        let vesting = ContractBuilder::new(Box::new(dango_vesting::instantiate))
            .with_execute(Box::new(dango_vesting::execute))
            .with_query(Box::new(dango_vesting::query))
            .build();

        let warp = ContractBuilder::new(Box::new(dango_warp::instantiate))
            .with_execute(Box::new(dango_warp::execute))
            .with_query(Box::new(dango_warp::query))
            .build();

        let perps = ContractBuilder::new(Box::new(dango_perps::instantiate))
            .with_execute(Box::new(dango_perps::execute))
            .with_query(Box::new(dango_perps::query))
            .with_cron_execute(Box::new(dango_perps::cron_execute))
            .with_authenticate(Box::new(dango_perps::authenticate))
            .build();

        #[cfg(feature = "metrics")]
        {
            dango_oracle::metrics::init_metrics();
            dango_perps::metrics::init_metrics();
            // TODO: add other contracts that emit metrics
        }

        Codes {
            account,
            account_factory,
            bank,
            gateway,
            hyperlane: Hyperlane { ism, mailbox, va },
            oracle,
            perps,
            vesting,
            warp,
        }
    }
}

// TODO: implement `GenesisCodes` for `WasmVm` and `HybridVm`.
// For now, we don't want to include them here because wasmer v6 has changed to
// the Business Source License. We want to make sure anything in the `dango/`
// directory does NOT have dependency on it.
