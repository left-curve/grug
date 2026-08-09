use {
    crate::VALIDATOR_SETS,
    anyhow::ensure,
    dango_hyperlane_types::{
        isms::multisig::{ExecuteMsg, InstantiateMsg, ValidatorSet},
        mailbox::Domain,
    },
    dango_primitives::{HexByteArray, MutableCtx, QuerierExt, Response},
    std::collections::BTreeSet,
};

pub fn instantiate(ctx: MutableCtx, msg: InstantiateMsg) -> anyhow::Result<Response> {
    for (domain, validator_set) in msg.validator_sets {
        ensure!(
            validator_set.threshold > 0,
            "threshold must be greater than zero for domain {domain}"
        );

        ensure!(
            validator_set.validators.len() >= validator_set.threshold as usize,
            "not enough validators for domain {domain}! threshold: {}, validators: {}",
            validator_set.threshold,
            validator_set.validators.len()
        );

        VALIDATOR_SETS.save(ctx.storage, domain, &validator_set)?;
    }

    Ok(Response::new())
}

pub fn execute(ctx: MutableCtx, msg: ExecuteMsg) -> anyhow::Result<Response> {
    match msg {
        ExecuteMsg::SetValidators {
            domain,
            threshold,
            validators,
        } => set_validators(ctx, domain, threshold, validators),
    }
}

#[inline]
fn set_validators(
    ctx: MutableCtx,
    domain: Domain,
    threshold: u32,
    validators: BTreeSet<HexByteArray<20>>,
) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "only the chain owner can call `set_validators`"
    );

    ensure!(threshold > 0, "threshold must be greater than zero");

    ensure!(
        validators.len() >= threshold as usize,
        "not enough validators! threshold: {}, validators: {}",
        threshold,
        validators.len()
    );

    VALIDATOR_SETS.save(
        ctx.storage,
        domain,
        &ValidatorSet {
            threshold,
            validators,
        },
    )?;

    Ok(Response::new())
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_primitives::{
            Addr, Coins, Config, Duration, MockContext, MockQuerier, Permission, Permissions,
            ResultExt, btree_set,
        },
        hex_literal::hex,
        std::collections::BTreeMap,
    };

    const V1: HexByteArray<20> =
        HexByteArray::from_inner(hex!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    const V2: HexByteArray<20> =
        HexByteArray::from_inner(hex!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"));

    fn mock_config(owner: Addr) -> Config {
        Config {
            owner,
            bank: Addr::mock(10),
            gas_token: dango_primitives::Denom::new_unchecked(["ugas"]),
            gas_fee_rate: Default::default(),
            gas_exemptions: Default::default(),
            cronjobs: BTreeMap::new(),
            permissions: Permissions {
                upload: Permission::Everybody,
                instantiate: Permission::Everybody,
            },
            max_orphan_age: Duration::from_seconds(3600),
        }
    }

    // ----------------------------- instantiate --------------------------------

    #[test]
    fn instantiate_rejects_zero_threshold() {
        let mut ctx = MockContext::new()
            .with_sender(Addr::mock(1))
            .with_funds(Coins::default());

        instantiate(
            ctx.as_mutable(),
            InstantiateMsg {
                validator_sets: BTreeMap::from([(
                    0,
                    ValidatorSet {
                        threshold: 0,
                        validators: BTreeSet::new(),
                    },
                )]),
            },
        )
        .should_fail_with_error("threshold must be greater than zero for domain 0");
    }

    #[test]
    fn instantiate_rejects_zero_threshold_with_validators() {
        let mut ctx = MockContext::new()
            .with_sender(Addr::mock(1))
            .with_funds(Coins::default());

        instantiate(
            ctx.as_mutable(),
            InstantiateMsg {
                validator_sets: BTreeMap::from([(
                    0,
                    ValidatorSet {
                        threshold: 0,
                        validators: btree_set! { V1 },
                    },
                )]),
            },
        )
        .should_fail_with_error("threshold must be greater than zero for domain 0");
    }

    #[test]
    fn instantiate_rejects_threshold_exceeding_validator_count() {
        let mut ctx = MockContext::new()
            .with_sender(Addr::mock(1))
            .with_funds(Coins::default());

        instantiate(
            ctx.as_mutable(),
            InstantiateMsg {
                validator_sets: BTreeMap::from([(
                    0,
                    ValidatorSet {
                        threshold: 2,
                        validators: btree_set! { V1 },
                    },
                )]),
            },
        )
        .should_fail_with_error("not enough validators for domain 0");
    }

    #[test]
    fn instantiate_accepts_valid_config() {
        let mut ctx = MockContext::new()
            .with_sender(Addr::mock(1))
            .with_funds(Coins::default());

        instantiate(
            ctx.as_mutable(),
            InstantiateMsg {
                validator_sets: BTreeMap::from([(
                    0,
                    ValidatorSet {
                        threshold: 1,
                        validators: btree_set! { V1 },
                    },
                )]),
            },
        )
        .should_succeed();
    }

    // ----------------------------- set_validators -----------------------------

    #[test]
    fn set_validators_rejects_zero_threshold() {
        let owner = Addr::mock(1);
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config(owner)))
            .with_sender(owner)
            .with_funds(Coins::default());

        set_validators(ctx.as_mutable(), 0, 0, btree_set! { V1 })
            .should_fail_with_error("threshold must be greater than zero");
    }

    #[test]
    fn set_validators_rejects_non_owner() {
        let owner = Addr::mock(1);
        let non_owner = Addr::mock(99);
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config(owner)))
            .with_sender(non_owner)
            .with_funds(Coins::default());

        set_validators(ctx.as_mutable(), 0, 1, btree_set! { V1 })
            .should_fail_with_error("only the chain owner can call `set_validators`");
    }

    #[test]
    fn set_validators_accepts_valid_config() {
        let owner = Addr::mock(1);
        let mut ctx = MockContext::new()
            .with_querier(MockQuerier::new().with_config(mock_config(owner)))
            .with_sender(owner)
            .with_funds(Coins::default());

        set_validators(ctx.as_mutable(), 0, 2, btree_set! { V1, V2 }).should_succeed();
    }
}
