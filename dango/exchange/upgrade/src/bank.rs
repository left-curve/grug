//! Wind-down of the token balances.
//!
//! Dango is being shut down. Every balance left on the chain has to be returned
//! to the address its owner deposited from. That mapping only exists for user
//! accounts, which the account factory records; a balance held by a contract
//! maps back to nobody.
//!
//! This one-time state transition makes every remaining balance belong to a
//! user account or to the chain owner:
//!
//! 1. **Refund the orphaned transfers.** Tokens sent to an address that doesn't
//!    exist are withheld in the bank contract. Each entry goes back to its
//!    sender, unless the sender is itself not a user account, in which case it
//!    goes to the owner to refund by hand.
//! 2. **Sweep the rest.** Any balance still held by an address that is neither
//!    a user account nor the owner goes to the owner.
//! 3. **Disable transfers**, so no new stranded balance can appear afterwards.
//!
//! Nothing is created or destroyed: every token moved here is moved from one
//! balance to another, and the transition ends by checking that the balances
//! still add up to the total supply.

use {
    dango_account_factory::USERS,
    dango_app::{APP_CONFIG, AppResult, CONFIG, CONTRACT_NAMESPACE, StorageProvider},
    dango_bank::{BALANCES, ORPHANED_TRANSFERS, SUPPLIES, TRANSFERS_ENABLED},
    dango_math::{IsZero, Number, NumberConst, Uint128},
    dango_primitives::{Addr, Coins, Denom, JsonDeExt, Order, StdError, StdResult, Storage},
    dango_types::config::{AppAddresses, AppConfig},
    std::collections::{BTreeMap, BTreeSet},
};

pub fn do_bank_upgrades(storage: Box<dyn Storage>) -> AppResult<()> {
    // Every address is read from state rather than hardcoded, so the same code
    // is correct on mainnet, on testnet, and in tests.
    let cfg = CONFIG.load(&storage)?;
    let app_cfg = APP_CONFIG.load(&storage)?.deserialize_json::<AppConfig>()?;

    // Two storage handles over the same (Arc-backed) buffer: one scoped to the
    // bank, whose balances are what we move, and one to the account factory,
    // which is the authority on whether an address is a user account.
    let mut bank_storage = StorageProvider::new(storage.clone(), &[CONTRACT_NAMESPACE, &cfg.bank]);
    let factory_storage = StorageProvider::new(
        storage,
        &[CONTRACT_NAMESPACE, &app_cfg.addresses.account_factory],
    );

    wind_down(
        &mut bank_storage,
        &factory_storage,
        cfg.bank,
        cfg.owner,
        &app_cfg.addresses,
    )
    // `?` converts `StdError` into `AppError`; there is no
    // `From<anyhow::Error>`, so bridge through `StdError::host`.
    .map_err(|err| StdError::host(err.to_string()))?;

    Ok(())
}

/// What the wind-down moved, for logging and for tests to reconcile against.
#[derive(Debug, Default)]
pub struct Summary {
    /// Orphaned transfers returned to a sender that is a user account.
    pub refunded: Vec<(Addr, Addr, Coins)>,
    /// Orphaned transfers whose sender is not a user account, so the tokens
    /// went to the owner instead. These are the ones the owner has to refund
    /// by hand.
    pub diverted: Vec<(Addr, Addr, Coins)>,
    /// Balances swept to the owner because their holder is neither a user
    /// account nor the owner.
    pub swept: Vec<(Addr, Coins)>,
}

/// Return every balance to a user account or to the owner, then turn transfers
/// off. See the module-level documentation for the phases.
fn wind_down(
    bank_storage: &mut dyn Storage,
    factory_storage: &dyn Storage,
    bank: Addr,
    owner: Addr,
    addresses: &AppAddresses,
) -> anyhow::Result<Summary> {
    let mut summary = Summary::default();

    refund_orphaned_transfers(
        bank_storage,
        factory_storage,
        bank,
        owner,
        addresses,
        &mut summary,
    )?;

    sweep_non_user_holders(
        bank_storage,
        factory_storage,
        owner,
        addresses,
        &mut summary,
    )?;

    assert_invariants(bank_storage, factory_storage, owner)?;

    // Turning transfers off is what stops users from stranding funds again
    // between this block and the shutdown.
    TRANSFERS_ENABLED.save(bank_storage, &false)?;

    tracing::info!(
        refunded = summary.refunded.len(),
        diverted = summary.diverted.len(),
        swept = summary.swept.len(),
        "Wound down the token balances"
    );

    Ok(summary)
}

/// Phase 1. Return every orphaned transfer, then clear the map.
fn refund_orphaned_transfers(
    bank_storage: &mut dyn Storage,
    factory_storage: &dyn Storage,
    bank: Addr,
    owner: Addr,
    addresses: &AppAddresses,
    summary: &mut Summary,
) -> anyhow::Result<()> {
    // Collect before mutating: writing balances while ranging over the map
    // would invalidate the iterator.
    let orphans = ORPHANED_TRANSFERS
        .range(bank_storage, None, None, Order::Ascending)
        .collect::<StdResult<Vec<_>>>()?;

    for ((sender, recipient), coins) in orphans {
        // The recipient never existed, which is why the transfer was orphaned
        // in the first place. So the tokens go back to the sender, unless the
        // sender is a contract, in which case there is no user to return them
        // to and the owner takes over the refund.
        let sender_is_user = is_user_account(factory_storage, sender)?;

        let destination = if sender_is_user {
            sender
        } else {
            owner
        };

        for coin in &coins {
            move_balance(bank_storage, bank, destination, coin.denom, *coin.amount)?;
        }

        tracing::warn!(
            %sender,
            sender_label = label(sender, owner, addresses),
            %recipient,
            coins = %coins,
            %destination,
            needs_manual_refund = !sender_is_user,
            "Refunded an orphaned transfer"
        );

        if sender_is_user {
            summary.refunded.push((sender, recipient, coins));
        } else {
            summary.diverted.push((sender, recipient, coins));
        }
    }

    // Clear the primary map together with its recipient index.
    //
    // The index has to be cleared wholesale, not entry by entry:
    // `recover_transfer` deletes through `may_take`, which skips index
    // maintenance, so the index also holds entries whose primary record was
    // recovered long ago. Mainnet carries 15 of those.
    ORPHANED_TRANSFERS.clear_all(bank_storage);

    // The bank's balance should be exactly the sum of the orphaned transfers,
    // so the loop above leaves it at zero. Sweep anything left rather than
    // trusting that, since a residue here would otherwise trip the invariants.
    let residue = balances_of(bank_storage, bank)?;

    for (denom, amount) in residue {
        tracing::warn!(
            %denom,
            %amount,
            "The bank contract held more than its orphaned transfers; sweeping the residue to the \
             owner"
        );

        move_balance(bank_storage, bank, owner, &denom, amount)?;
    }

    Ok(())
}

/// Phase 2. Sweep every balance whose holder is neither a user account nor the
/// owner.
///
/// We expect this to find only the hyperlane validator-announce contract, which
/// holds a few cents of USDC on both chains. It is written generically so that
/// the end state is true by construction, whatever the fork block turns up.
fn sweep_non_user_holders(
    bank_storage: &mut dyn Storage,
    factory_storage: &dyn Storage,
    owner: Addr,
    addresses: &AppAddresses,
    summary: &mut Summary,
) -> anyhow::Result<()> {
    for holder in non_user_holders(bank_storage, factory_storage, owner)? {
        let coins = balances_of(bank_storage, holder)?;

        for (denom, amount) in &coins {
            tracing::warn!(
                address = %holder,
                label = label(holder, owner, addresses),
                %denom,
                %amount,
                "Swept a balance held by a non-user account to the owner"
            );

            move_balance(bank_storage, holder, owner, denom, *amount)?;
        }

        summary.swept.push((holder, Coins::new_unchecked(coins)));
    }

    Ok(())
}

/// Phase 3. Verify the chain is in the state the refund process needs.
///
/// A violation means this migration is wrong, so it panics rather than
/// returning — the chain must not continue on corrupt state.
fn assert_invariants(
    bank_storage: &dyn Storage,
    factory_storage: &dyn Storage,
    owner: Addr,
) -> anyhow::Result<()> {
    // 1. For every token, the balances add up to the total supply.
    let mut totals = BTreeMap::<Denom, Uint128>::new();

    for res in BALANCES.range(bank_storage, None, None, Order::Ascending) {
        let ((_, denom), amount) = res?;
        totals
            .entry(denom)
            .or_insert(Uint128::ZERO)
            .checked_add_assign(amount)?;
    }

    let supplies = SUPPLIES
        .range(bank_storage, None, None, Order::Ascending)
        .collect::<StdResult<BTreeMap<_, _>>>()?;

    assert_eq!(
        totals, supplies,
        "balances don't add up to the total supplies!"
    );

    // 2. Every address holding a balance is a user account or the owner.
    let strays = non_user_holders(bank_storage, factory_storage, owner)?;

    assert!(
        strays.is_empty(),
        "{} non-user accounts still hold a balance: {:?}",
        strays.len(),
        strays
    );

    // 3. No orphaned transfer survives, in the map or in its recipient index.
    assert!(
        ORPHANED_TRANSFERS
            .range(bank_storage, None, None, Order::Ascending)
            .next()
            .is_none(),
        "orphaned transfers survived the wind-down!"
    );

    assert!(
        ORPHANED_TRANSFERS.idx.recipient.is_empty(bank_storage),
        "the orphaned transfer recipient index survived the wind-down!"
    );

    tracing::info!("All wind-down invariants passed");

    Ok(())
}

/// Every address that holds a balance and is neither a user account nor the
/// owner.
fn non_user_holders(
    bank_storage: &dyn Storage,
    factory_storage: &dyn Storage,
    owner: Addr,
) -> anyhow::Result<Vec<Addr>> {
    let mut holders = BTreeSet::new();

    for res in BALANCES.range(bank_storage, None, None, Order::Ascending) {
        let ((address, _), _) = res?;
        holders.insert(address);
    }

    holders
        .into_iter()
        .filter(|address| *address != owner)
        .map(|address| Ok((address, is_user_account(factory_storage, address)?)))
        .filter_map(|res| match res {
            Ok((address, false)) => Some(Ok(address)),
            Ok((_, true)) => None,
            Err(err) => Some(Err(err)),
        })
        .collect()
}

/// Whether the address is one of a Dango user's accounts.
///
/// The account factory's `by_account` index is the authority on this. Looking
/// the address up there reads only the index, not the whole `User` record.
fn is_user_account(factory_storage: &dyn Storage, address: Addr) -> StdResult<bool> {
    USERS
        .idx
        .by_account
        .may_load_key(factory_storage, address)
        .map(|user_index| user_index.is_some())
}

/// All of an address's balances.
fn balances_of(bank_storage: &dyn Storage, address: Addr) -> StdResult<BTreeMap<Denom, Uint128>> {
    BALANCES
        .prefix(&address)
        .range(bank_storage, None, None, Order::Ascending)
        .collect()
}

/// Move tokens from one balance to another, deleting the sender's entry once it
/// reaches zero, the same way the bank contract does.
fn move_balance(
    bank_storage: &mut dyn Storage,
    from: Addr,
    to: Addr,
    denom: &Denom,
    amount: Uint128,
) -> anyhow::Result<()> {
    let remaining = may_load_balance(bank_storage, from, denom)?.checked_sub(amount)?;

    if remaining.is_zero() {
        BALANCES.remove(bank_storage, (&from, denom));
    } else {
        BALANCES.save(bank_storage, (&from, denom), &remaining)?;
    }

    let credited = may_load_balance(bank_storage, to, denom)?.checked_add(amount)?;

    BALANCES.save(bank_storage, (&to, denom), &credited)?;

    Ok(())
}

fn may_load_balance(storage: &dyn Storage, address: Addr, denom: &Denom) -> StdResult<Uint128> {
    Ok(BALANCES
        .may_load(storage, (&address, denom))?
        .unwrap_or(Uint128::ZERO))
}

/// A human-readable name for an address, when it is one the app config knows
/// about. Log lines are the only record the owner has of what needs a manual
/// refund, so it helps to say which contract sent what.
fn label(address: Addr, owner: Addr, addresses: &AppAddresses) -> &'static str {
    if address == owner {
        "owner"
    } else if address == addresses.account_factory {
        "account factory"
    } else if address == addresses.gateway {
        "gateway"
    } else if address == addresses.hyperlane.ism {
        "hyperlane ism"
    } else if address == addresses.hyperlane.mailbox {
        "hyperlane mailbox"
    } else if address == addresses.hyperlane.va {
        "hyperlane validator announce"
    } else if address == addresses.oracle {
        "oracle"
    } else if address == addresses.perps {
        "perps"
    } else if address == addresses.warp {
        "warp"
    } else {
        "unknown"
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_primitives::{Inner, JsonSerExt, MockStorage, Shared, btree_map, coins as coins_of},
        dango_types::{
            account_factory::{User, UserIndex, Username},
            config::Hyperlane,
            constants::{eth, usdc},
        },
    };

    const BANK: Addr = Addr::mock(1);
    const FACTORY: Addr = Addr::mock(2);
    const OWNER: Addr = Addr::mock(3);
    const GATEWAY: Addr = Addr::mock(4);
    const VA: Addr = Addr::mock(5);
    const ALICE: Addr = Addr::mock(6);
    const BOB: Addr = Addr::mock(7);
    /// A non-user address the app config knows nothing about.
    const STRANGER: Addr = Addr::mock(8);
    /// Addresses that were never created, which is how a transfer to them
    /// became orphaned.
    const DEAD_1: Addr = Addr::mock(101);
    const DEAD_2: Addr = Addr::mock(102);

    /// 1 USDC in base units (6 decimals).
    const ONE_USDC: u128 = 1_000_000;

    fn usdc() -> Denom {
        usdc::DENOM.clone()
    }

    fn eth() -> Denom {
        eth::DENOM.clone()
    }

    fn addresses() -> AppAddresses {
        AppAddresses {
            account_factory: FACTORY,
            gateway: GATEWAY,
            hyperlane: Hyperlane {
                ism: Addr::mock(11),
                mailbox: Addr::mock(12),
                va: VA,
            },
            oracle: Addr::mock(13),
            perps: Addr::mock(14),
            warp: Addr::mock(15),
        }
    }

    fn coins<const N: usize>(array: [(Denom, u128); N]) -> Coins {
        Coins::new_unchecked(
            array
                .into_iter()
                .map(|(denom, amount)| (denom, Uint128::new(amount)))
                .collect(),
        )
    }

    /// Bank and account factory storage handles over one shared in-memory
    /// store, seeded to whatever shape a test needs.
    struct Fixture {
        base: Shared<MockStorage>,
        next_user_index: UserIndex,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                base: Shared::new(MockStorage::new()),
                next_user_index: 0,
            }
        }

        fn bank(&self) -> StorageProvider {
            StorageProvider::new(Box::new(self.base.clone()), &[CONTRACT_NAMESPACE, &BANK])
        }

        fn factory(&self) -> StorageProvider {
            StorageProvider::new(Box::new(self.base.clone()), &[CONTRACT_NAMESPACE, &FACTORY])
        }

        /// Register the address as one of a user's accounts, the way the
        /// account factory does when onboarding.
        fn with_user(mut self, address: Addr) -> Self {
            let index = self.next_user_index;
            self.next_user_index += 1;

            USERS
                .save(
                    &mut self.factory(),
                    index,
                    &User {
                        index,
                        name: Username::default_for_index(index),
                        accounts: btree_map! { index => address },
                        keys: BTreeMap::new(),
                    },
                )
                .unwrap();

            self
        }

        /// Credit a balance, keeping the supply consistent with it.
        fn with_balance(self, address: Addr, denom: Denom, amount: u128) -> Self {
            let mut bank = self.bank();
            let amount = Uint128::new(amount);

            let balance = may_load_balance(&bank, address, &denom).unwrap();
            BALANCES
                .save(
                    &mut bank,
                    (&address, &denom),
                    &balance.checked_add(amount).unwrap(),
                )
                .unwrap();

            let supply = SUPPLIES
                .may_load(&bank, &denom)
                .unwrap()
                .unwrap_or(Uint128::ZERO);
            SUPPLIES
                .save(&mut bank, &denom, &supply.checked_add(amount).unwrap())
                .unwrap();

            self
        }

        /// Record an orphaned transfer, with the tokens withheld in the bank
        /// contract, exactly as `bank_execute` leaves them.
        fn with_orphan(self, sender: Addr, recipient: Addr, coins: Coins) -> Self {
            ORPHANED_TRANSFERS
                .save(&mut self.bank(), (sender, recipient), &coins)
                .unwrap();

            coins.into_iter().fold(self, |fixture, coin| {
                fixture.with_balance(BANK, coin.denom, coin.amount.into_inner())
            })
        }

        /// Overwrite a supply without touching balances, to break invariant 1.
        fn with_broken_supply(self, denom: Denom, amount: u128) -> Self {
            SUPPLIES
                .save(&mut self.bank(), &denom, &Uint128::new(amount))
                .unwrap();

            self
        }

        fn run(&self) -> anyhow::Result<Summary> {
            wind_down(&mut self.bank(), &self.factory(), BANK, OWNER, &addresses())
        }

        fn balance_of(&self, address: Addr, denom: Denom) -> u128 {
            may_load_balance(&self.bank(), address, &denom)
                .unwrap()
                .into_inner()
        }

        fn holds_nothing(&self, address: Addr) -> bool {
            balances_of(&self.bank(), address).unwrap().is_empty()
        }
    }

    // ------------------------- refunding the orphans -------------------------

    /// The common case: a user typed an address that doesn't exist, so the
    /// tokens come back to them.
    #[test]
    fn refunds_orphan_to_user_sender() {
        let fixture = Fixture::new().with_user(ALICE).with_orphan(
            ALICE,
            DEAD_1,
            coins([(usdc(), 100 * ONE_USDC)]),
        );

        let summary = fixture.run().unwrap();

        assert_eq!(fixture.balance_of(ALICE, usdc()), 100 * ONE_USDC);
        assert!(fixture.holds_nothing(BANK));
        assert_eq!(summary.refunded.len(), 1);
        assert!(summary.diverted.is_empty());
    }

    /// One entry can hold several denoms, because repeat sends to the same
    /// recipient are merged. Every one of them is returned.
    #[test]
    fn refunds_multiple_denoms_in_one_orphan() {
        let fixture = Fixture::new().with_user(ALICE).with_orphan(
            ALICE,
            DEAD_1,
            coins([(usdc(), 100 * ONE_USDC), (eth(), 5)]),
        );

        fixture.run().unwrap();

        assert_eq!(fixture.balance_of(ALICE, usdc()), 100 * ONE_USDC);
        assert_eq!(fixture.balance_of(ALICE, eth()), 5);
        assert!(fixture.holds_nothing(BANK));
    }

    /// A refund adds to whatever the sender already holds; it must not
    /// overwrite it.
    #[test]
    fn merges_refund_into_existing_balance() {
        let fixture = Fixture::new()
            .with_user(ALICE)
            .with_balance(ALICE, usdc(), 40 * ONE_USDC)
            .with_orphan(ALICE, DEAD_1, coins([(usdc(), 60 * ONE_USDC)]));

        fixture.run().unwrap();

        assert_eq!(fixture.balance_of(ALICE, usdc()), 100 * ONE_USDC);
    }

    /// A sender that is a contract has no depositor behind it. Returning the
    /// tokens there would leave a contract holding a balance, which is exactly
    /// the state the wind-down removes, so they go to the owner instead.
    #[test]
    fn routes_orphan_from_gateway_to_owner() {
        let fixture = Fixture::new().with_orphan(GATEWAY, DEAD_1, coins([(usdc(), 21 * ONE_USDC)]));

        let summary = fixture.run().unwrap();

        assert_eq!(fixture.balance_of(OWNER, usdc()), 21 * ONE_USDC);
        assert!(fixture.holds_nothing(GATEWAY));
        assert!(fixture.holds_nothing(BANK));
        assert!(summary.refunded.is_empty());
        assert_eq!(
            summary.diverted,
            vec![(GATEWAY, DEAD_1, coins([(usdc(), 21 * ONE_USDC)]))]
        );
    }

    /// The same rule applies to an address the app config has never heard of.
    /// Membership in the account factory is what decides, not a list of known
    /// contracts.
    #[test]
    fn routes_orphan_from_unknown_non_user_to_owner() {
        let fixture = Fixture::new().with_orphan(STRANGER, DEAD_1, coins([(eth(), 7)]));

        let summary = fixture.run().unwrap();

        assert_eq!(fixture.balance_of(OWNER, eth()), 7);
        assert_eq!(summary.diverted.len(), 1);
    }

    /// `recover_transfer` deletes through `may_take`, which leaves the
    /// recipient index behind. Mainnet carries 15 such entries. Clearing only
    /// the entries we can see would leave them, and a stale index makes the
    /// by-recipient query fail forever.
    #[test]
    fn clears_the_recipient_index() {
        let fixture =
            Fixture::new()
                .with_user(ALICE)
                .with_orphan(ALICE, DEAD_1, coins([(usdc(), ONE_USDC)]));

        // A record whose primary entry was recovered, leaving only its index.
        ORPHANED_TRANSFERS
            .save(
                &mut fixture.bank(),
                (BOB, DEAD_2),
                &coins([(usdc(), ONE_USDC)]),
            )
            .unwrap();
        ORPHANED_TRANSFERS
            .primary
            .remove(&mut fixture.bank(), (BOB, DEAD_2));

        fixture.run().unwrap();

        assert!(ORPHANED_TRANSFERS.idx.recipient.is_empty(&fixture.bank()));
    }

    /// A bank balance in excess of the orphaned transfers has no owner to
    /// return it to, so it goes to the chain owner rather than tripping the
    /// invariants.
    #[test]
    fn sweeps_bank_residue_to_owner() {
        let fixture = Fixture::new()
            .with_user(ALICE)
            .with_orphan(ALICE, DEAD_1, coins([(usdc(), 10 * ONE_USDC)]))
            .with_balance(BANK, usdc(), 3 * ONE_USDC);

        fixture.run().unwrap();

        assert_eq!(fixture.balance_of(ALICE, usdc()), 10 * ONE_USDC);
        assert_eq!(fixture.balance_of(OWNER, usdc()), 3 * ONE_USDC);
        assert!(fixture.holds_nothing(BANK));
    }

    // ------------------------- sweeping the strays ---------------------------

    /// The one stray we know about on both live chains.
    #[test]
    fn sweeps_hyperlane_va_balance_to_owner() {
        let fixture = Fixture::new().with_balance(VA, usdc(), 15_200);

        let summary = fixture.run().unwrap();

        assert_eq!(fixture.balance_of(OWNER, usdc()), 15_200);
        assert!(fixture.holds_nothing(VA));
        assert_eq!(summary.swept, vec![(VA, coins([(usdc(), 15_200)]))]);
    }

    /// The sweep is generic, so anything else that turns up at the fork block
    /// is handled too.
    #[test]
    fn sweeps_every_non_user_holder() {
        let fixture = Fixture::new()
            .with_balance(VA, usdc(), 15_200)
            .with_balance(STRANGER, eth(), 42)
            .with_balance(GATEWAY, usdc(), 5 * ONE_USDC);

        let summary = fixture.run().unwrap();

        assert_eq!(fixture.balance_of(OWNER, usdc()), 15_200 + 5 * ONE_USDC);
        assert_eq!(fixture.balance_of(OWNER, eth()), 42);
        assert_eq!(summary.swept.len(), 3);
    }

    /// User balances are the whole point of the exercise. Nothing may touch
    /// them.
    #[test]
    fn leaves_user_balances_untouched() {
        let fixture = Fixture::new()
            .with_user(ALICE)
            .with_user(BOB)
            .with_balance(ALICE, usdc(), 123 * ONE_USDC)
            .with_balance(ALICE, eth(), 9)
            .with_balance(BOB, usdc(), 7)
            .with_balance(VA, usdc(), 15_200);

        fixture.run().unwrap();

        assert_eq!(fixture.balance_of(ALICE, usdc()), 123 * ONE_USDC);
        assert_eq!(fixture.balance_of(ALICE, eth()), 9);
        assert_eq!(fixture.balance_of(BOB, usdc()), 7);
    }

    /// The owner is allowed to hold a balance, so its own funds must survive
    /// while it also collects the sweeps.
    #[test]
    fn leaves_owner_balance_untouched() {
        let fixture = Fixture::new()
            .with_balance(OWNER, usdc(), 50 * ONE_USDC)
            .with_balance(VA, usdc(), 15_200);

        let summary = fixture.run().unwrap();

        assert_eq!(fixture.balance_of(OWNER, usdc()), 50 * ONE_USDC + 15_200);
        assert_eq!(summary.swept.len(), 1);
    }

    /// The owner is a registered user account on both live chains, but the
    /// exemption must not depend on that.
    #[test]
    fn owner_need_not_be_a_registered_user() {
        let fixture = Fixture::new().with_balance(OWNER, usdc(), ONE_USDC);

        let summary = fixture.run().unwrap();

        assert_eq!(fixture.balance_of(OWNER, usdc()), ONE_USDC);
        assert!(summary.swept.is_empty());
    }

    // ---------------------------- whole-run shape ----------------------------

    /// Nothing to do is a valid outcome, and must still leave transfers off.
    #[test]
    fn clean_state_is_a_no_op() {
        let fixture = Fixture::new()
            .with_user(ALICE)
            .with_balance(ALICE, usdc(), 100 * ONE_USDC)
            .with_balance(OWNER, usdc(), ONE_USDC);

        let summary = fixture.run().unwrap();

        assert_eq!(fixture.balance_of(ALICE, usdc()), 100 * ONE_USDC);
        assert_eq!(fixture.balance_of(OWNER, usdc()), ONE_USDC);
        assert!(summary.refunded.is_empty());
        assert!(summary.diverted.is_empty());
        assert!(summary.swept.is_empty());
    }

    #[test]
    fn disables_transfers() {
        let fixture = Fixture::new();

        fixture.run().unwrap();

        assert_eq!(
            TRANSFERS_ENABLED.may_load(&fixture.bank()).unwrap(),
            Some(false)
        );
    }

    /// Every token moved is moved from one balance to another, so the totals
    /// can't drift.
    #[test]
    fn preserves_total_supply_per_denom() {
        let fixture = Fixture::new()
            .with_user(ALICE)
            .with_user(BOB)
            .with_balance(ALICE, usdc(), 100 * ONE_USDC)
            .with_balance(BOB, eth(), 12)
            .with_balance(VA, usdc(), 15_200)
            .with_orphan(ALICE, DEAD_1, coins([(usdc(), 3 * ONE_USDC)]))
            .with_orphan(GATEWAY, DEAD_2, coins([(eth(), 4)]));

        fixture.run().unwrap();

        // `assert_invariants` already checks this, but assert the totals
        // explicitly so the test fails loudly if the check is ever weakened.
        let bank = fixture.bank();
        let mut totals = BTreeMap::<Denom, Uint128>::new();

        for res in BALANCES.range(&bank, None, None, Order::Ascending) {
            let ((_, denom), amount) = res.unwrap();
            totals
                .entry(denom)
                .or_insert(Uint128::ZERO)
                .checked_add_assign(amount)
                .unwrap();
        }

        assert_eq!(
            totals,
            btree_map! {
                usdc() => Uint128::new(103 * ONE_USDC + 15_200),
                eth() => Uint128::new(16),
            }
        );
    }

    // ------------------------------ invariants -------------------------------

    /// Balances that don't add up to the supply mean the migration corrupted
    /// state, or that the state was already corrupt. Either way the chain must
    /// not continue.
    #[test]
    #[should_panic(expected = "balances don't add up to the total supplies!")]
    fn panics_on_supply_mismatch() {
        Fixture::new()
            .with_user(ALICE)
            .with_balance(ALICE, usdc(), 100 * ONE_USDC)
            .with_broken_supply(usdc(), 99 * ONE_USDC)
            .run()
            .unwrap();
    }

    /// The sweep makes this true by construction, so the check is exercised
    /// directly against a state the sweep never saw.
    #[test]
    #[should_panic(expected = "non-user accounts still hold a balance")]
    fn panics_when_a_non_user_holder_survives() {
        let fixture = Fixture::new().with_balance(STRANGER, usdc(), ONE_USDC);

        assert_invariants(&fixture.bank(), &fixture.factory(), OWNER).unwrap();
    }

    /// Same, for the orphaned transfers.
    #[test]
    #[should_panic(expected = "orphaned transfers survived the wind-down!")]
    fn panics_when_an_orphan_survives() {
        let fixture =
            Fixture::new()
                .with_user(ALICE)
                .with_orphan(ALICE, DEAD_1, coins([(usdc(), ONE_USDC)]));

        // Give the bank's withheld tokens a home, so this fails on the orphan
        // check rather than on the supply check.
        let mut bank = fixture.bank();
        BALANCES.remove(&mut bank, (&BANK, &usdc()));
        SUPPLIES.remove(&mut bank, &usdc());

        assert_invariants(&bank, &fixture.factory(), OWNER).unwrap();
    }

    // -------------------------- the address plumbing -------------------------

    /// `do_bank_upgrades` reads every address it needs out of state, so the
    /// same binary is correct on mainnet, on testnet, and here.
    #[test]
    fn reads_every_address_from_state() {
        use dango_primitives::{Config, Duration, Permission, Permissions};

        let fixture = Fixture::new()
            .with_user(ALICE)
            .with_orphan(ALICE, DEAD_1, coins([(usdc(), ONE_USDC)]))
            .with_balance(VA, usdc(), 15_200);

        let mut root = fixture.base.clone();

        CONFIG
            .save(
                &mut root,
                &Config {
                    owner: OWNER,
                    bank: BANK,
                    gas_token: usdc(),
                    gas_fee_rate: Default::default(),
                    gas_exemptions: Default::default(),
                    cronjobs: Default::default(),
                    permissions: Permissions {
                        upload: Permission::Nobody,
                        instantiate: Permission::Everybody,
                    },
                    max_orphan_age: Duration::from_weeks(1),
                },
            )
            .unwrap();

        APP_CONFIG
            .save(
                &mut root,
                &AppConfig {
                    addresses: addresses(),
                    minimum_deposit: coins_of! { usdc() => 10 * ONE_USDC },
                }
                .to_json_value()
                .unwrap(),
            )
            .unwrap();

        do_bank_upgrades(Box::new(fixture.base.clone())).unwrap();

        assert_eq!(fixture.balance_of(ALICE, usdc()), ONE_USDC);
        assert_eq!(fixture.balance_of(OWNER, usdc()), 15_200);
        assert_eq!(
            TRANSFERS_ENABLED.may_load(&fixture.bank()).unwrap(),
            Some(false)
        );
    }
}
