use {
    crate::{
        account_factory,
        referral::load_referral_data,
        state::{
            FEE_SHARE_RATIO, REFEREE_TO_REFERRER, REFERRER_TO_REFEREE_STATISTICS,
            USER_REFERRAL_DATA,
        },
    },
    anyhow::ensure,
    dango_order_book::round_to_day,
    dango_primitives::{MutableCtx, QuerierExt, Response},
    dango_types::{
        account_factory::{self, UserIndex},
        perps::{RefereeStats, ReferralSet},
    },
};

/// Register a referral relationship between a referrer and a referee.
///
/// Caller must be one of:
///
/// - the account factory (during user registration, when the user provides
///   a referral code at sign-up);
/// - the chain's owner (admin override — additionally bypasses the
///   referrer-opt-in and referee-immutability checks below);
/// - an account owned by the referee (self-service after registration).
pub fn set_referral(
    ctx: MutableCtx,
    referrer: UserIndex,
    referee: UserIndex,
) -> anyhow::Result<Response> {
    // Referrer and referee must be different users.
    ensure!(referrer != referee, "a user cannot refer themselves");

    let account_factory = account_factory(ctx.querier);
    let owner = ctx.querier.query_owner()?;

    // Only three parties can set the referrer:
    // - the chain owner;
    // - the account factory contract (this is the case when the referee specifies
    //   a referral code during registration);
    // - the referee himself (this is the case when the referee did not specify
    //   a referral code during registration, but later goes to the setting menu
    //   to choose a referrer).
    ensure!(
        ctx.sender == owner || ctx.sender == account_factory || {
            // The account owner lookup is lazy: only done if sender is neither
            // the chain owner nor the account factory.
            // TODO: refactor to raw query (query_wasm_path).
            let sender_user_index = ctx
                .querier
                .query_wasm_smart(
                    account_factory,
                    account_factory::QueryAccountRequest {
                        address: ctx.sender,
                    },
                )?
                .owner;
            sender_user_index == referee
        },
        "caller is not the account factory, chain owner, or an account owned by the referee"
    );

    // The referrer must have a share ratio set (i.e. has opted in as a referrer).
    // Exception: bypass this check if sender is the owner.
    ensure!(
        ctx.sender == owner || FEE_SHARE_RATIO.has(ctx.storage, referrer),
        "referrer {referrer} has no fee share ratio set"
    );

    // The referral relationship is immutable once set.
    // Exception: bypass this check if sender is the owner.
    ensure!(
        ctx.sender == owner || !REFEREE_TO_REFERRER.has(ctx.storage, referee),
        "referee {referee} already has a referrer"
    );

    // Save the referee-to-referrer relation.
    REFEREE_TO_REFERRER.save(ctx.storage, referee, &referrer)?;

    // Initialize per-referee statistics for the referrer.
    //
    // On owner-driven overwrite (when the referee already had a different
    // referrer), the previous referrer's state is intentionally retained:
    // the old `(old_referrer, referee)` row in `REFERRER_TO_REFEREE_STATISTICS`
    // remains as the historical stats for the period that relationship was
    // active, and the old referrer's `referee_count` is not decremented so it
    // continues to read as "users that have been a direct referee of this
    // referrer at one point in time".
    REFERRER_TO_REFEREE_STATISTICS.save(
        ctx.storage,
        (referrer, referee),
        &RefereeStats {
            registered_at: ctx.block.timestamp,
            ..Default::default()
        },
    )?;

    // Increment the referrer's referee count.
    {
        let today = round_to_day(ctx.block.timestamp);

        let mut data = load_referral_data(ctx.storage, referrer, None)?;
        data.referee_count += 1;

        USER_REFERRAL_DATA.save(ctx.storage, (referrer, today), &data)?;
    }

    Ok(Response::new().add_event(ReferralSet { referrer, referee })?)
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_primitives::{
            Addr, Coins, Config, Duration, EventName, JsonDeExt, JsonSerExt, MockContext,
            MockQuerier, Permission, Permissions, ResultExt, Storage, Timestamp,
        },
        dango_types::{
            account_factory::Account,
            config::{AppAddresses, AppConfig},
            perps::{FeeShareRatio, UserReferralData},
        },
        std::collections::BTreeMap,
    };

    const OWNER: Addr = Addr::mock(1);
    const ACCOUNT_FACTORY: Addr = Addr::mock(2);
    const REFEREE_ADDR: Addr = Addr::mock(10);
    const OUTSIDER_ADDR: Addr = Addr::mock(11);

    const REFERRER: UserIndex = 1;
    const REFEREE: UserIndex = 2;
    const OUTSIDER: UserIndex = 3;

    /// Non-zero block timestamp so `registered_at` and `round_to_day` produce
    /// deterministic, meaningful values in the post-condition test.
    const BLOCK_TIME: Timestamp = Timestamp::from_seconds(1_700_000_000);

    fn mock_config() -> Config {
        Config {
            owner: OWNER,
            bank: Addr::mock(100),
            gas_token: dango_primitives::Denom::new_unchecked(["ugas"]),
            gas_fee_rate: Default::default(),
            gas_exemptions: Default::default(),
            cronjobs: BTreeMap::new(),
            permissions: Permissions {
                upload: Permission::Everybody,
                instantiate: Permission::Everybody,
            },
            max_orphan_age: Duration::from_seconds(1000),
        }
    }

    /// Querier that exposes the chain owner and the account-factory address.
    /// Sufficient for every branch that doesn't fall through to the wasm
    /// smart query.
    fn base_querier() -> MockQuerier {
        MockQuerier::new()
            .with_config(mock_config())
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory: ACCOUNT_FACTORY,
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap()
    }

    /// Extends `base_querier()` with a smart-query handler that answers any
    /// query to the account factory with an `Account` owned by
    /// `account_owner`. Used only by tests that reach the wasm smart query.
    fn querier_with_account_owner(account_owner: UserIndex) -> MockQuerier {
        base_querier().with_smart_query_handler(move |addr, _msg| {
            assert_eq!(addr, ACCOUNT_FACTORY, "unexpected smart query to {addr}");
            Ok(Account {
                index: 0,
                owner: account_owner,
            }
            .to_json_value()
            .unwrap())
        })
    }

    fn seed_referrer(storage: &mut dyn Storage) {
        FEE_SHARE_RATIO
            .save(storage, REFERRER, &FeeShareRatio::new_percent(25))
            .unwrap();
    }

    #[test]
    fn self_referral_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(base_querier())
            .with_sender(ACCOUNT_FACTORY)
            .with_funds(Coins::default());

        set_referral(ctx.as_mutable(), REFERRER, REFERRER)
            .should_fail_with_error("a user cannot refer themselves");
    }

    #[test]
    fn account_factory_can_set_referral() {
        let mut ctx = MockContext::new()
            .with_querier(base_querier())
            .with_sender(ACCOUNT_FACTORY)
            .with_funds(Coins::default());

        seed_referrer(&mut ctx.storage);

        set_referral(ctx.as_mutable(), REFERRER, REFEREE).should_succeed();
    }

    #[test]
    fn chain_owner_can_set_referral() {
        let mut ctx = MockContext::new()
            .with_querier(base_querier())
            .with_sender(OWNER)
            .with_funds(Coins::default());

        seed_referrer(&mut ctx.storage);

        set_referral(ctx.as_mutable(), REFERRER, REFEREE).should_succeed();
    }

    #[test]
    fn referee_own_account_can_set_referral() {
        let mut ctx = MockContext::new()
            .with_querier(querier_with_account_owner(REFEREE))
            .with_sender(REFEREE_ADDR)
            .with_funds(Coins::default());

        seed_referrer(&mut ctx.storage);

        set_referral(ctx.as_mutable(), REFERRER, REFEREE).should_succeed();
    }

    #[test]
    fn unauthorized_third_party_account_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(querier_with_account_owner(OUTSIDER))
            .with_sender(OUTSIDER_ADDR)
            .with_funds(Coins::default());

        seed_referrer(&mut ctx.storage);

        set_referral(ctx.as_mutable(), REFERRER, REFEREE).should_fail_with_error(
            "caller is not the account factory, chain owner, or an account owned by the referee",
        );
    }

    /// The referrer-opt-in requirement applies to non-owner callers.
    /// (Owner-bypass is covered separately in `owner_bypasses_fee_share_ratio_check`.)
    #[test]
    fn referrer_without_fee_share_ratio_rejected() {
        let mut ctx = MockContext::new()
            .with_querier(base_querier())
            .with_sender(ACCOUNT_FACTORY)
            .with_funds(Coins::default());

        // Deliberately do NOT seed FEE_SHARE_RATIO.

        set_referral(ctx.as_mutable(), REFERRER, REFEREE)
            .should_fail_with_error("has no fee share ratio set");
    }

    /// Immutability applies to non-owner callers: a referee with an existing
    /// referrer cannot have it overwritten by the account factory or the
    /// referee's own account. (Owner-bypass is covered separately in
    /// `owner_can_overwrite_existing_referrer`.)
    #[test]
    fn already_set_referee_rejected_for_non_owner() {
        let mut ctx = MockContext::new()
            .with_querier(base_querier())
            .with_sender(ACCOUNT_FACTORY)
            .with_funds(Coins::default());

        seed_referrer(&mut ctx.storage);
        REFEREE_TO_REFERRER
            .save(&mut ctx.storage, REFEREE, &OUTSIDER)
            .unwrap();

        set_referral(ctx.as_mutable(), REFERRER, REFEREE)
            .should_fail_with_error("already has a referrer");
    }

    /// The chain owner can register a referral even when the referrer has
    /// not opted in by setting a `FEE_SHARE_RATIO`.
    #[test]
    fn owner_bypasses_fee_share_ratio_check() {
        let mut ctx = MockContext::new()
            .with_querier(base_querier())
            .with_sender(OWNER)
            .with_funds(Coins::default())
            .with_block_timestamp(BLOCK_TIME);

        // Deliberately do NOT seed FEE_SHARE_RATIO.

        set_referral(ctx.as_mutable(), REFERRER, REFEREE).should_succeed();

        assert_eq!(
            REFEREE_TO_REFERRER.load(&ctx.storage, REFEREE).unwrap(),
            REFERRER,
        );
    }

    /// The chain owner can overwrite an existing referee-to-referrer mapping.
    /// The previous referrer's per-referee stats row and lifetime
    /// `referee_count` are intentionally retained: they read as the stats
    /// for the period that prior relationship was active, and as the count
    /// of users that were ever a direct referee, respectively.
    #[test]
    fn owner_can_overwrite_existing_referrer() {
        let mut ctx = MockContext::new()
            .with_querier(base_querier())
            .with_sender(OWNER)
            .with_funds(Coins::default())
            .with_block_timestamp(BLOCK_TIME);

        seed_referrer(&mut ctx.storage);

        // Pre-seed an existing OUTSIDER -> REFEREE relationship, including
        // the per-referee stats row and the OUTSIDER's lifetime referee count.
        REFEREE_TO_REFERRER
            .save(&mut ctx.storage, REFEREE, &OUTSIDER)
            .unwrap();
        REFERRER_TO_REFEREE_STATISTICS
            .save(
                &mut ctx.storage,
                (OUTSIDER, REFEREE),
                &RefereeStats {
                    registered_at: BLOCK_TIME,
                    ..Default::default()
                },
            )
            .unwrap();
        USER_REFERRAL_DATA
            .save(
                &mut ctx.storage,
                (OUTSIDER, round_to_day(BLOCK_TIME)),
                &UserReferralData {
                    referee_count: 1,
                    ..Default::default()
                },
            )
            .unwrap();

        set_referral(ctx.as_mutable(), REFERRER, REFEREE).should_succeed();

        // Mapping is overwritten to the new referrer.
        assert_eq!(
            REFEREE_TO_REFERRER.load(&ctx.storage, REFEREE).unwrap(),
            REFERRER,
        );

        // New per-referee stats row created for the new (referrer, referee).
        REFERRER_TO_REFEREE_STATISTICS
            .load(&ctx.storage, (REFERRER, REFEREE))
            .unwrap();

        // Old per-referee stats row is retained — represents the period
        // when REFEREE was a referee of OUTSIDER.
        REFERRER_TO_REFEREE_STATISTICS
            .load(&ctx.storage, (OUTSIDER, REFEREE))
            .unwrap();

        // Old referrer's lifetime referee_count is not decremented.
        let outsider_data = USER_REFERRAL_DATA
            .load(&ctx.storage, (OUTSIDER, round_to_day(BLOCK_TIME)))
            .unwrap();
        assert_eq!(outsider_data.referee_count, 1);

        // New referrer's referee_count is incremented to 1.
        let referrer_data = USER_REFERRAL_DATA
            .load(&ctx.storage, (REFERRER, round_to_day(BLOCK_TIME)))
            .unwrap();
        assert_eq!(referrer_data.referee_count, 1);
    }

    #[test]
    fn happy_path_writes_expected_state() {
        let mut ctx = MockContext::new()
            .with_querier(base_querier())
            .with_sender(OWNER)
            .with_funds(Coins::default())
            .with_block_timestamp(BLOCK_TIME);

        seed_referrer(&mut ctx.storage);

        let response = set_referral(ctx.as_mutable(), REFERRER, REFEREE).should_succeed();

        // Referee-to-referrer mapping saved.
        assert_eq!(
            REFEREE_TO_REFERRER.load(&ctx.storage, REFEREE).unwrap(),
            REFERRER,
        );

        // Per-referee stats initialized with the block timestamp.
        let stats = REFERRER_TO_REFEREE_STATISTICS
            .load(&ctx.storage, (REFERRER, REFEREE))
            .unwrap();
        assert_eq!(stats.registered_at, BLOCK_TIME);

        // Referrer's daily cumulative referral data incremented.
        let data = USER_REFERRAL_DATA
            .load(&ctx.storage, (REFERRER, round_to_day(BLOCK_TIME)))
            .unwrap();
        assert_eq!(data.referee_count, 1);

        // `ReferralSet` event emitted with the expected payload.
        let event = response
            .subevents
            .iter()
            .find(|e| e.ty == ReferralSet::EVENT_NAME)
            .expect("ReferralSet event missing");
        let payload: ReferralSet = event.data.clone().deserialize_json().unwrap();
        assert_eq!(payload.referrer, REFERRER);
        assert_eq!(payload.referee, REFEREE);
    }
}
