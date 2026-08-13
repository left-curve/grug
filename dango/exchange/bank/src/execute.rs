use {
    crate::{
        BALANCES, METADATAS, NAMESPACE_OWNERS, ORPHANED_TRANSFERS, SUPPLIES, TRANSFERS_ENABLED,
    },
    anyhow::{anyhow, bail, ensure},
    dango_math::{IsZero, Number, NumberConst, Uint128},
    dango_primitives::{
        Addr, BankMsg, Coins, Denom, EventBuilder, MutableCtx, Part, QuerierExt, Response,
        StdError, StdResult, Storage, SudoCtx,
    },
    dango_types::{
        DangoQuerier,
        bank::{
            Burned, ExecuteMsg, InstantiateMsg, Metadata, Minted, Received, Sent, TransferOrphaned,
        },
    },
    std::collections::HashMap,
};

pub fn instantiate(ctx: MutableCtx, msg: InstantiateMsg) -> anyhow::Result<Response> {
    let mut supplies = HashMap::<Denom, Uint128>::new();

    for (address, coins) in msg.balances {
        for coin in coins {
            BALANCES.save(ctx.storage, (&address, &coin.denom), &coin.amount)?;

            match supplies.get_mut(&coin.denom) {
                Some(supply) => {
                    supply.checked_add_assign(coin.amount)?;
                },
                None => {
                    supplies.insert(coin.denom, coin.amount);
                },
            }
        }
    }

    for (denom, amount) in supplies {
        SUPPLIES.save(ctx.storage, &denom, &amount)?;
    }

    for (namespace, owner) in msg.namespaces {
        NAMESPACE_OWNERS.save(ctx.storage, &namespace, &owner)?;
    }

    for (denom, metadata) in msg.metadatas {
        METADATAS.save(ctx.storage, &denom, &metadata)?;
    }

    Ok(Response::new()) // No need to emit events during genesis.
}

pub fn execute(ctx: MutableCtx, msg: ExecuteMsg) -> anyhow::Result<Response> {
    ensure!(ctx.funds.is_empty(), "don't send funds to bank contract");

    match msg {
        ExecuteMsg::SetNamespaceOwner { namespace, owner } => {
            set_namespace_owner(ctx, namespace, owner)
        },
        ExecuteMsg::SetMetadata { denom, metadata } => set_metadata(ctx, denom, metadata),
        ExecuteMsg::Mint { to, coins } => mint(ctx, to, coins),
        ExecuteMsg::Burn { from, coins } => burn(ctx, from, coins),
        ExecuteMsg::RecoverTransfer { sender, recipient } => {
            recover_transfer(ctx, sender, recipient)
        },
        ExecuteMsg::SetTransfersEnabled(enabled) => set_transfers_enabled(ctx, enabled),
    }
}

fn set_namespace_owner(ctx: MutableCtx, namespace: Part, owner: Addr) -> anyhow::Result<Response> {
    // Only chain owner can grant namespace.
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "you don't have the right, O you don't have the right"
    );

    NAMESPACE_OWNERS.may_update(ctx.storage, &namespace, |maybe_owner| {
        // TODO: for now, we don't support granting a namespace to multiple
        // owners or overwriting an existing owner.
        if let Some(existing_owner) = maybe_owner {
            bail!("namespace `{namespace}` already granted to `{existing_owner}`");
        }

        Ok(owner)
    })?;

    Ok(Response::new())
}

fn set_transfers_enabled(ctx: MutableCtx, enabled: bool) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "you don't have the right, O you don't have the right"
    );

    TRANSFERS_ENABLED.save(ctx.storage, &enabled)?;

    Ok(Response::new())
}

fn set_metadata(ctx: MutableCtx, denom: Denom, metadata: Metadata) -> anyhow::Result<Response> {
    ensure_namespace_owner(&ctx, &denom)?;

    METADATAS.save(ctx.storage, &denom, &metadata)?;

    Ok(Response::default())
}

fn mint(ctx: MutableCtx, to: Addr, coins: Coins) -> anyhow::Result<Response> {
    // Handle orphaned transfers.
    // See the comments in `bank_execute` for more details.
    let recipient_exists = ctx.querier.query_contract(to).is_ok();

    // While transfers are disabled, no new orphaned transfer may be created.
    // See the equivalent check in `bank_execute` for the reasoning. In practice
    // the gateway always mints to itself, so this only guards against a caller
    // that doesn't.
    ensure!(
        recipient_exists || transfers_enabled(ctx.storage)?,
        "token transfers are disabled; can't mint to the non-existent address {to}"
    );

    let recipient = if recipient_exists {
        to
    } else {
        ORPHANED_TRANSFERS.may_update(ctx.storage, (ctx.sender, to), |transfers| {
            let mut transfers = transfers.unwrap_or_default();
            transfers.insert_many(coins.clone())?;
            Ok::<_, StdError>(transfers)
        })?;

        ctx.contract
    };

    for coin in &coins {
        ensure_namespace_owner(&ctx, coin.denom)?;

        increase_supply(ctx.storage, coin.denom, *coin.amount)?;
        increase_balance(ctx.storage, &recipient, coin.denom, *coin.amount)?;
    }

    Ok(Response::new()
        .add_event(Minted {
            user: recipient,
            minter: ctx.sender,
            coins: coins.clone(),
        })?
        .may_add_event(if !recipient_exists {
            Some(TransferOrphaned {
                from: ctx.sender,
                to: recipient,
                coins,
            })
        } else {
            None
        })?)
}

fn burn(ctx: MutableCtx, from: Addr, coins: Coins) -> anyhow::Result<Response> {
    for coin in &coins {
        ensure_namespace_owner(&ctx, coin.denom)?;

        decrease_supply(ctx.storage, coin.denom, *coin.amount)?;
        decrease_balance(ctx.storage, &from, coin.denom, *coin.amount)?;
    }

    Ok(Response::new().add_event(Burned {
        user: from,
        burner: ctx.sender,
        coins,
    })?)
}

fn ensure_namespace_owner(ctx: &MutableCtx, denom: &Denom) -> anyhow::Result<()> {
    match denom.namespace() {
        // The denom has a namespace. The namespace's owner can mint/burn.
        Some(part) => {
            let maybe_owner = NAMESPACE_OWNERS.may_load(ctx.storage, part)?;
            ensure!(
                maybe_owner == Some(ctx.sender),
                "sender does not own the namespace `{part}`"
            );
        },
        // The denom is a top-level denom (i.e. doesn't have a namespace).
        // Only the chain owner can mint/burn.
        None => {
            ensure!(
                ctx.sender == ctx.querier.query_owner()?,
                "only chain owner can mint, burn, or set metadata of top-level denoms"
            );
        },
    }

    Ok(())
}

fn recover_transfer(ctx: MutableCtx, sender: Addr, recipient: Addr) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == sender
            || ctx.sender == recipient
            || ctx.sender == ctx.querier.query_owner()?,
        "only the sender, the recipient, or the chain owner can recover an orphaned transfer"
    );

    let Some(coins) = ORPHANED_TRANSFERS.may_take(ctx.storage, (sender, recipient))? else {
        // Orphaned transfer not found.
        // Do nothing (return with no-op; do not throw error).
        return Ok(Response::new());
    };

    for coin in &coins {
        decrease_balance(ctx.storage, &ctx.contract, coin.denom, *coin.amount)?;
        increase_balance(ctx.storage, &ctx.sender, coin.denom, *coin.amount)?;
    }

    Ok(Response::new()
        .add_event(Sent {
            user: ctx.contract,
            to: ctx.sender,
            coins: coins.clone(),
        })?
        .add_event(Received {
            user: ctx.sender,
            from: ctx.contract,
            coins,
        })?)
}

/// There are two major problems with existing blockchain systems related to
/// token transfers:
///
/// 1. **It's not possible for the recipient to reject a token transfer.**
///
///    For example, a user who wants to unwrap their Wrapped Ether (WETH) tokens
///    may mistakenly send the tokens to the WETH contract, while the correct
///    way of doing it is to call the `withdraw` method. Due to how ERC-20 is
///    designed, it is not possible for the WETH contract to reject this transfer.
///    A significant amount of money has been lost due to this.
///
///    Dango solves this by introducing a `receive` entry point to every contract.
///    A contract can simply throw an error if it does not wish to accept a
///    specific transfer of tokens.
///
/// 2. **It is possible to send tokens to a non-existent recipient.**
///
///    This can happen if the sender makes a typo when inputting the recipient's
///    address. There will be no way to recover the tokens.
///
///    To solve this, the Dango bank contract checks whether the recipient exists
///    before executing the transfer. If the recipient doesn't exist, we call this
///    an "**orphaned transfer**". The tokens will be temporarily held in the bank
///    contract. Either the sender or the recipient (once it exists) can claim
///    the tokens by calling the `recover_transfer` method.
///
/// Every balance movement caused by a transfer passes through here: the
/// `Transfer` message, the funds attached to an `Execute` or `Instantiate`
/// message, and the gas fee withheld by the state machine. That makes this the
/// one place where transfers can be turned off, which the wind-down does.
pub fn bank_execute(ctx: SudoCtx, msg: BankMsg) -> anyhow::Result<Response> {
    // While transfers are disabled, the only legs that survive are the ones a
    // bridge deposit or withdrawal needs, plus the gas fee paid to the chain
    // owner. Resolve those two addresses only when the gate is on, so the
    // normal path pays for nothing beyond reading the flag.
    //
    // The gate has to key on `(from, to)`: `bank_execute` runs with a
    // `SudoCtx`, so it can't see who signed the transaction.
    let gate = if transfers_enabled(ctx.storage)? {
        None
    } else {
        Some((ctx.querier.query_gateway()?, ctx.querier.query_owner()?))
    };

    let mut events = EventBuilder::with_capacity(msg.transfers.len() * 3);

    for (to, coins) in msg.transfers {
        // If the recipient exists, increase the recipient's balance. Otherwise,
        // 1. withhold the tokens in the bank contract;
        // 2. record the transfer in the `ORPHANED_TRANSFERS` map.
        let recipient_exists = ctx.querier.query_contract(to).is_ok();

        if let Some((gateway, owner)) = gate {
            // A user sending funds to the gateway is a withdrawal; the gateway
            // sending funds out is a deposit, a refund, or a withdrawal fee. A
            // transfer to the chain owner is the gas fee. Nothing else moves.
            ensure!(
                msg.from == gateway || to == gateway || to == owner,
                "token transfers are disabled; the chain is winding down"
            );

            // Even on an allowed leg, the funds must land somewhere that
            // exists. Without this, a bridge deposit addressed to an account
            // that was never created would strand tokens in this contract all
            // over again, which is what the wind-down has just cleaned up.
            ensure!(
                recipient_exists,
                "token transfers are disabled; can't send to the non-existent address {to}"
            );
        }

        let recipient = if recipient_exists {
            to
        } else {
            ORPHANED_TRANSFERS.may_update(ctx.storage, (msg.from, to), |amount| {
                let mut amount = amount.unwrap_or_default();
                amount.insert_many(coins.clone())?;
                Ok::<_, StdError>(amount)
            })?;

            ctx.contract
        };

        for coin in &coins {
            decrease_balance(ctx.storage, &msg.from, coin.denom, *coin.amount)?;
            increase_balance(ctx.storage, &recipient, coin.denom, *coin.amount)?;
        }

        events
            .may_push(if !recipient_exists {
                Some(TransferOrphaned {
                    from: msg.from,
                    to,
                    coins: coins.clone(),
                })
            } else {
                None
            })?
            .push(Sent {
                user: msg.from,
                to: recipient,
                coins: coins.clone(),
            })?
            .push(Received {
                user: recipient,
                from: msg.from,
                coins,
            })?;
    }

    Ok(Response::new().add_events(events)?)
}

/// Whether token transfers are enabled.
///
/// An absent flag means enabled, so a chain that predates the wind-down needs
/// neither a genesis change nor a migration of this item.
pub fn transfers_enabled(storage: &dyn Storage) -> StdResult<bool> {
    Ok(TRANSFERS_ENABLED.may_load(storage)?.unwrap_or(true))
}

fn increase_supply(
    storage: &mut dyn Storage,
    denom: &Denom,
    amount: Uint128,
) -> anyhow::Result<Option<Uint128>> {
    SUPPLIES
        .may_modify(storage, denom, |maybe_supply| -> StdResult<_> {
            let supply = maybe_supply.unwrap_or(Uint128::ZERO).checked_add(amount)?;
            // Only write to storage if the supply is non-zero.
            if supply.is_zero() {
                Ok(None)
            } else {
                Ok(Some(supply))
            }
        })
        .map_err(|err| {
            anyhow!("failed to increase supply! denom: {denom}, amount: {amount}, reason: {err}")
        })
}

fn decrease_supply(
    storage: &mut dyn Storage,
    denom: &Denom,
    amount: Uint128,
) -> anyhow::Result<Option<Uint128>> {
    SUPPLIES
        .may_modify(storage, denom, |maybe_supply| -> StdResult<_> {
            let supply = maybe_supply.unwrap_or(Uint128::ZERO).checked_sub(amount)?;
            // If supply is reduced to zero, delete it, to save disk space.
            if supply.is_zero() {
                Ok(None)
            } else {
                Ok(Some(supply))
            }
        })
        .map_err(|err| {
            anyhow!("failed to decrease supply! denom: {denom}, amount: {amount}, reason: {err}")
        })
}

fn increase_balance(
    storage: &mut dyn Storage,
    address: &Addr,
    denom: &Denom,
    amount: Uint128,
) -> anyhow::Result<Option<Uint128>> {
    BALANCES
        .may_modify(storage, (address, denom), |maybe_balance| -> StdResult<_> {
            let balance = maybe_balance.unwrap_or(Uint128::ZERO).checked_add(amount)?;
            // Only write to storage if the balance is non-zero.
            if balance.is_zero() {
                Ok(None)
            } else {
                Ok(Some(balance))
            }
        })
        .map_err(|err| {
            anyhow!(
                "failed to increase balance! address: {address}, denom: {denom}, amount: {amount}, reason: {err}"
            )
        })
}

fn decrease_balance(
    storage: &mut dyn Storage,
    address: &Addr,
    denom: &Denom,
    amount: Uint128,
) -> anyhow::Result<Option<Uint128>> {
    BALANCES
        .may_modify(storage, (address, denom), |maybe_balance| -> StdResult<_> {
            let balance = maybe_balance.unwrap_or(Uint128::ZERO).checked_sub(amount)?;
            // If balance is reduced to zero, delete it, to save disk space.
            if balance.is_zero() {
                Ok(None)
            } else {
                Ok(Some(balance))
            }
        })
        .map_err(|err| {
            anyhow!(
                "failed to decrease balance! address: {address}, denom: {denom}, amount: {amount}, reason: {err}"
            )
        })
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_math::Udec128,
        dango_primitives::{
            Coins, Config, ContractInfo, Duration, Hash256, MockContext, MockQuerier, Order,
            Permission, Permissions, ResultExt, btree_map, coins,
        },
        dango_types::{
            config::{AppAddresses, AppConfig, Hyperlane},
            constants::usdc,
        },
    };

    const BANK: Addr = Addr::mock(1);
    const OWNER: Addr = Addr::mock(2);
    const GATEWAY: Addr = Addr::mock(3);
    const ALICE: Addr = Addr::mock(4);
    const BOB: Addr = Addr::mock(5);
    /// An address at which no contract was ever instantiated.
    const DEAD: Addr = Addr::mock(101);

    const ONE_USDC: u128 = 1_000_000;

    fn usdc() -> Denom {
        usdc::DENOM.clone()
    }

    /// A context for `bank_execute`, which runs as the state machine and so
    /// has neither a sender nor funds.
    ///
    /// Every address except `DEAD` is registered as an existing contract,
    /// because `bank_execute` decides whether a transfer is orphaned by asking
    /// the querier whether the recipient exists.
    fn ctx(transfers_enabled: Option<bool>) -> MockContext {
        let mut querier = MockQuerier::new()
            .with_config(Config {
                owner: OWNER,
                bank: BANK,
                gas_token: usdc(),
                gas_fee_rate: Udec128::ZERO,
                gas_exemptions: Default::default(),
                cronjobs: Default::default(),
                permissions: Permissions {
                    upload: Permission::Nobody,
                    instantiate: Permission::Everybody,
                },
                max_orphan_age: Duration::from_weeks(1),
            })
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory: Addr::mock(11),
                    gateway: GATEWAY,
                    hyperlane: Hyperlane {
                        ism: Addr::mock(12),
                        mailbox: Addr::mock(13),
                        va: Addr::mock(14),
                    },
                    oracle: Addr::mock(15),
                    perps: Addr::mock(16),
                    warp: Addr::mock(17),
                },
                minimum_deposit: coins! { usdc() => 10 * ONE_USDC },
            })
            .unwrap();

        for address in [BANK, OWNER, GATEWAY, ALICE, BOB] {
            querier = querier.with_contract(
                address,
                ContractInfo {
                    code_hash: Hash256::ZERO,
                    label: None,
                    admin: None,
                },
            );
        }

        let mut ctx = MockContext::new().with_querier(querier).with_contract(BANK);

        if let Some(enabled) = transfers_enabled {
            TRANSFERS_ENABLED.save(&mut ctx.storage, &enabled).unwrap();
        }

        // Everyone who sends in these tests needs something to send.
        for address in [ALICE, GATEWAY] {
            BALANCES
                .save(
                    &mut ctx.storage,
                    (&address, &usdc()),
                    &Uint128::new(1_000 * ONE_USDC),
                )
                .unwrap();
        }

        ctx
    }

    fn transfer(ctx: &mut MockContext, from: Addr, to: Addr, amount: u128) -> anyhow::Result<()> {
        bank_execute(
            ctx.as_sudo(),
            BankMsg {
                from,
                transfers: btree_map! { to => coins! { usdc() => amount } },
            },
        )
        .map(|_| ())
    }

    fn balance_of(storage: &dyn Storage, address: Addr) -> Uint128 {
        BALANCES
            .may_load(storage, (&address, &usdc()))
            .unwrap()
            .unwrap_or(Uint128::ZERO)
    }

    fn orphan_count(storage: &dyn Storage) -> usize {
        ORPHANED_TRANSFERS
            .range(storage, None, None, Order::Ascending)
            .count()
    }

    // ---------------------------- the flag defaults --------------------------

    /// A chain that predates the wind-down has never written the flag. It must
    /// keep working, or the upgrade would have to migrate every chain's bank.
    #[test]
    fn transfers_allowed_when_flag_absent() {
        let mut ctx = ctx(None);

        transfer(&mut ctx, ALICE, BOB, 100).unwrap();

        assert_eq!(balance_of(&ctx.storage, BOB), Uint128::new(100));
    }

    #[test]
    fn transfers_allowed_when_flag_true() {
        let mut ctx = ctx(Some(true));

        transfer(&mut ctx, ALICE, BOB, 100).unwrap();

        assert_eq!(balance_of(&ctx.storage, BOB), Uint128::new(100));
    }

    // ------------------------------ what's blocked ---------------------------

    #[test]
    fn blocks_user_to_user_when_disabled() {
        let mut ctx = ctx(Some(false));

        transfer(&mut ctx, ALICE, BOB, 100).should_fail_with_error("token transfers are disabled");

        assert_eq!(balance_of(&ctx.storage, BOB), Uint128::ZERO);
    }

    /// A batch is one call, so one disallowed leg fails the whole thing. The
    /// allowed leg must not go through on its own.
    #[test]
    fn blocks_whole_batch_if_one_leg_is_disallowed() {
        let mut ctx = ctx(Some(false));

        bank_execute(
            ctx.as_sudo(),
            BankMsg {
                from: ALICE,
                transfers: btree_map! {
                    GATEWAY => coins! { usdc() => 100 },
                    BOB => coins! { usdc() => 200 },
                },
            },
        )
        .should_fail_with_error("token transfers are disabled");
    }

    // ------------------------------ what's allowed ---------------------------

    /// The escrow leg of a withdrawal: the user attaches funds to the
    /// gateway's `TransferRemote`.
    #[test]
    fn allows_user_to_gateway_when_disabled() {
        let mut ctx = ctx(Some(false));

        transfer(&mut ctx, ALICE, GATEWAY, 100).unwrap();

        assert_eq!(
            balance_of(&ctx.storage, GATEWAY),
            Uint128::new(1_000 * ONE_USDC + 100)
        );
    }

    /// A deposit, a rejected withdrawal's refund, and a withdrawal fee all
    /// leave the gateway.
    #[test]
    fn allows_gateway_to_user_when_disabled() {
        let mut ctx = ctx(Some(false));

        transfer(&mut ctx, GATEWAY, ALICE, 100).unwrap();

        assert_eq!(
            balance_of(&ctx.storage, ALICE),
            Uint128::new(1_000 * ONE_USDC + 100)
        );
    }

    /// The state machine withholds the gas fee by calling `bank_execute`
    /// directly. `gas_fee_rate` is zero on both live chains, so this path is
    /// dormant, but blocking it would freeze every transaction if the rate
    /// were ever raised.
    #[test]
    fn allows_transfer_to_owner_when_disabled() {
        let mut ctx = ctx(Some(false));

        transfer(&mut ctx, ALICE, OWNER, 100).unwrap();

        assert_eq!(balance_of(&ctx.storage, OWNER), Uint128::new(100));
    }

    // ------------------------- no new orphaned transfers ---------------------

    /// While transfers are on, a send to a non-existent address is withheld
    /// here, as it always was.
    #[test]
    fn orphans_a_transfer_to_a_missing_recipient_when_enabled() {
        let mut ctx = ctx(Some(true));

        transfer(&mut ctx, ALICE, DEAD, 100).unwrap();

        assert_eq!(orphan_count(&ctx.storage), 1);
        assert_eq!(balance_of(&ctx.storage, BANK), Uint128::new(100));
    }

    /// The gateway may still move funds while transfers are off, but not into
    /// an address that was never created. Otherwise a deposit would strand
    /// tokens all over again, which is what the wind-down just cleaned up.
    #[test]
    fn blocks_gateway_to_missing_recipient() {
        let mut ctx = ctx(Some(false));

        transfer(&mut ctx, GATEWAY, DEAD, 100)
            .should_fail_with_error("can't send to the non-existent address");

        assert_eq!(orphan_count(&ctx.storage), 0);
        assert_eq!(balance_of(&ctx.storage, BANK), Uint128::ZERO);
    }

    // --------------------------- mint, burn, recover -------------------------

    /// `mint` has its own orphan branch, so it needs the same guard.
    #[test]
    fn blocks_mint_to_missing_recipient_when_disabled() {
        let mut ctx = ctx(Some(false));

        NAMESPACE_OWNERS
            .save(
                &mut ctx.storage,
                &usdc().namespace().unwrap().clone(),
                &GATEWAY,
            )
            .unwrap();

        let mut ctx = ctx.with_sender(GATEWAY).with_funds(Coins::new());

        mint(ctx.as_mutable(), DEAD, coins! { usdc() => 100 })
            .should_fail_with_error("can't mint to the non-existent address");

        assert_eq!(orphan_count(&ctx.storage), 0);
    }

    /// The gateway mints to itself, then transfers. That must keep working, or
    /// no deposit could ever land.
    #[test]
    fn allows_mint_to_an_existing_recipient_when_disabled() {
        let mut ctx = ctx(Some(false));

        NAMESPACE_OWNERS
            .save(
                &mut ctx.storage,
                &usdc().namespace().unwrap().clone(),
                &GATEWAY,
            )
            .unwrap();

        let mut ctx = ctx.with_sender(GATEWAY).with_funds(Coins::new());

        mint(ctx.as_mutable(), GATEWAY, coins! { usdc() => 100 }).unwrap();

        assert_eq!(
            balance_of(&ctx.storage, GATEWAY),
            Uint128::new(1_000 * ONE_USDC + 100)
        );
    }

    /// Burning is how a withdrawal destroys the local token. It moves nothing
    /// between accounts, so it is not gated.
    #[test]
    fn allows_burn_when_disabled() {
        let mut ctx = ctx(Some(false));

        NAMESPACE_OWNERS
            .save(
                &mut ctx.storage,
                &usdc().namespace().unwrap().clone(),
                &GATEWAY,
            )
            .unwrap();

        SUPPLIES
            .save(&mut ctx.storage, &usdc(), &Uint128::new(1_000 * ONE_USDC))
            .unwrap();

        let mut ctx = ctx.with_sender(GATEWAY).with_funds(Coins::new());

        burn(ctx.as_mutable(), GATEWAY, coins! { usdc() => 100 }).unwrap();

        assert_eq!(
            balance_of(&ctx.storage, GATEWAY),
            Uint128::new(1_000 * ONE_USDC - 100)
        );
    }

    /// The wind-down empties the map and the gate stops it refilling, so this
    /// is left open as an escape hatch. With nothing to recover it does
    /// nothing.
    #[test]
    fn recover_transfer_is_a_no_op_with_no_orphans() {
        let ctx = ctx(Some(false));
        let mut ctx = ctx.with_sender(ALICE).with_funds(Coins::new());

        recover_transfer(ctx.as_mutable(), ALICE, DEAD).unwrap();

        assert_eq!(
            balance_of(&ctx.storage, ALICE),
            Uint128::new(1_000 * ONE_USDC)
        );
    }

    // ---------------------------- the owner's switch -------------------------

    #[test]
    fn set_transfers_enabled_is_owner_only() {
        let ctx = ctx(None);
        let mut ctx = ctx.with_sender(ALICE).with_funds(Coins::new());

        set_transfers_enabled(ctx.as_mutable(), false)
            .should_fail_with_error("you don't have the right");

        assert!(transfers_enabled(&ctx.storage).unwrap());
    }

    #[test]
    fn set_transfers_enabled_works_for_the_owner() {
        let ctx = ctx(None);
        let mut ctx = ctx.with_sender(OWNER).with_funds(Coins::new());

        set_transfers_enabled(ctx.as_mutable(), false).unwrap();
        assert!(!transfers_enabled(&ctx.storage).unwrap());

        set_transfers_enabled(ctx.as_mutable(), true).unwrap();
        assert!(transfers_enabled(&ctx.storage).unwrap());
    }
}
