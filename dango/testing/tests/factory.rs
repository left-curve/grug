use {
    dango_account_factory::{MAX_ACCOUNTS_PER_USER, USERS},
    dango_genesis::{AccountOption, GenesisOption},
    dango_math::Uint128,
    dango_primitives::{
        Addressable, Coins, HashExt, JsonSerExt, Message, NonEmpty, Op, QuerierExt, ResultExt,
        Signer, btree_map, coins,
    },
    dango_storage::StorageQuerier,
    dango_testing::{
        Factory, HyperlaneTestSuite, Preset, TestAccount, mock_arbitrum, setup_test_naive,
        setup_test_naive_with_custom_genesis,
    },
    dango_types::{
        account,
        account_factory::{self, Account, RegisterUserData, UserIndexOrName, Username},
        auth::AccountStatus,
        bank,
        constants::usdc,
    },
    std::str::FromStr,
};

/// Prior to PR [#1460](https://github.com/left-curve/left-curve/pull/1460),
/// users are expected to first make a deposit before sending the `RegisterUser`
/// message. Sending the `RegisterUser` message without a deposit resulting in
/// the transaction failing. This design has drawbacks; see the PR's description.
/// Since PR #1460, this test now reflects the intended onboarding procedure.
#[tokio::test]
async fn onboarding_without_deposit() {
    let (suite, mut accounts, codes, contracts, validator_sets) =
        setup_test_naive_with_custom_genesis(
            Default::default(),
            GenesisOption {
                account: AccountOption {
                    minimum_deposit: coins! { usdc::DENOM.clone() => 10_000_000 },
                    ..Preset::preset_test()
                },
                ..Preset::preset_test()
            },
        );
    let mut suite = HyperlaneTestSuite::new(suite, validator_sets, &contracts);

    // Make an empty block to advance block height from 0 to 1.
    //
    // The reason of this is when the chain does `CheckTx`, it does it under the
    // state of the _last finalized block_. Without advancing the block here,
    // that would be block 0, in other words the genesis block. The single-signature
    // account won't claim orphaned transfers during genesis. For a realistic test,
    // we do `CheckTx` at a post-genesis block.
    suite.make_empty_block().await;

    let chain_id = suite.chain_id.clone();

    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        3,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    // Send the register user transaction without making a deposit first.
    suite
        .execute(
            &mut Factory::new(contracts.account_factory),
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterUser {
                key: user.first_key(),
                key_hash: user.first_key_hash(),
                seed: 3,
                signature: user
                    .sign_arbitrary(RegisterUserData {
                        chain_id: chain_id.clone(),
                        key: user.first_key(),
                        key_hash: user.first_key_hash(),
                        seed: 3,
                        referrer: None,
                    })
                    .unwrap(),
                referrer: None,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // The account should have been created in the `Inactive` state.
    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(AccountStatus::Inactive);

    // Attempting to send a transaction at this time. `CheckTx` should fail.
    let mut user = user.query_user_index(suite.querier());
    let tx = user
        .sign_transaction(
            NonEmpty::new_unchecked(vec![
                Message::transfer(user.address(), Coins::new()).unwrap(),
            ]),
            &suite.chain_id,
            100_000,
        )
        .unwrap();

    suite
        .check_tx(tx.clone())
        .should_fail_with_error(format!("account {} is not active", user.address()));

    // Make a deposit of the minimum amount.
    suite
        .receive_warp_transfer(
            &mut accounts.owner,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            &user,
            10_000_000, // Minimum deposit is 10_000_000. Need to send at this that amount.
        )
        .await
        .should_succeed();

    // Account should have been activated.
    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(AccountStatus::Active);

    // Try again, should succeed.
    suite.check_tx(tx).should_succeed();

    // User opens a new account. While his first account required an initial
    // deposit, any subsequent account should be activated by default.
    let user_index = user.user_index();
    suite
        .execute(
            &mut user,
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterAccount {},
            Coins::new(),
        )
        .await
        .should_succeed();

    // Ensure the user now has two accounts and they are both active.
    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user_index)),
        )
        .should_succeed_and(|res| {
            res.accounts.len() == 2
                && res.accounts.values().all(|address| {
                    suite
                        .query_wasm_path(*address, dango_auth::account::STATUS.path())
                        .unwrap()
                        .is_active()
                })
        });
}

/// An inactive account must not be able to receive a transfer from any sender
/// other than the gateway. Only the gateway can deposit into (and thereby
/// activate) an inactive account.
#[tokio::test]
async fn inactive_account_rejects_transfer_from_non_gateway() {
    let (mut suite, mut accounts, codes, contracts, _) = setup_test_naive_with_custom_genesis(
        Default::default(),
        GenesisOption {
            account: AccountOption {
                minimum_deposit: coins! { usdc::DENOM.clone() => 10_000_000 },
                ..Preset::preset_test()
            },
            ..Preset::preset_test()
        },
    );

    let chain_id = suite.chain_id.clone();

    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        3,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    // Register the user without making a deposit. The account is created in
    // the `Inactive` state.
    suite
        .execute(
            &mut Factory::new(contracts.account_factory),
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterUser {
                key: user.first_key(),
                key_hash: user.first_key_hash(),
                seed: 3,
                signature: user
                    .sign_arbitrary(RegisterUserData {
                        chain_id: chain_id.clone(),
                        key: user.first_key(),
                        key_hash: user.first_key_hash(),
                        seed: 3,
                        referrer: None,
                    })
                    .unwrap(),
                referrer: None,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(AccountStatus::Inactive);

    // The owner (a regular activated user, not the gateway) attempts to
    // transfer USDC into the inactive account. The transfer must be rejected.
    suite
        .transfer(
            &mut accounts.owner,
            user.address(),
            coins! { usdc::DENOM.clone() => 10_000_000 },
        )
        .await
        .should_fail_with_error(format!(
            "account {} is not active, only the gateway can deposit into it",
            user.address()
        ));

    // The account must still be inactive.
    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(AccountStatus::Inactive);
}

/// A sufficient deposit routed through the gateway activates an inactive
/// account.
#[tokio::test]
async fn gateway_deposit_activates_account() {
    let (suite, mut accounts, codes, contracts, validator_sets) =
        setup_test_naive_with_custom_genesis(
            Default::default(),
            GenesisOption {
                account: AccountOption {
                    minimum_deposit: coins! { usdc::DENOM.clone() => 10_000_000 },
                    ..Preset::preset_test()
                },
                ..Preset::preset_test()
            },
        );
    let mut suite = HyperlaneTestSuite::new(suite, validator_sets, &contracts);

    // Make an empty block so that subsequent transactions are processed at a
    // post-genesis block height (see `onboarding_without_deposit` for context).
    suite.make_empty_block().await;

    let chain_id = suite.chain_id.clone();

    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        3,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    // Register the user without a deposit. The account starts as `Inactive`.
    suite
        .execute(
            &mut Factory::new(contracts.account_factory),
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterUser {
                key: user.first_key(),
                key_hash: user.first_key_hash(),
                seed: 3,
                signature: user
                    .sign_arbitrary(RegisterUserData {
                        chain_id: chain_id.clone(),
                        key: user.first_key(),
                        key_hash: user.first_key_hash(),
                        seed: 3,
                        referrer: None,
                    })
                    .unwrap(),
                referrer: None,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(AccountStatus::Inactive);

    // Receive a warp transfer of the minimum amount. The funds are forwarded
    // by the gateway to the account, which must flip the account to `Active`.
    suite
        .receive_warp_transfer(
            &mut accounts.owner,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            &user,
            10_000_000,
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(AccountStatus::Active);
}

/// A transfer from the gateway to an inactive account must not be rejected,
/// even when the amount is below the minimum deposit. The account stays
/// `Inactive` but the funds are credited.
#[tokio::test]
async fn gateway_transfer_to_inactive_account_is_accepted() {
    let (suite, mut accounts, codes, contracts, validator_sets) =
        setup_test_naive_with_custom_genesis(
            Default::default(),
            GenesisOption {
                account: AccountOption {
                    minimum_deposit: coins! { usdc::DENOM.clone() => 10_000_000 },
                    ..Preset::preset_test()
                },
                ..Preset::preset_test()
            },
        );
    let mut suite = HyperlaneTestSuite::new(suite, validator_sets, &contracts);

    suite.make_empty_block().await;

    let chain_id = suite.chain_id.clone();

    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        3,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    // Register the user without a deposit. The account starts as `Inactive`.
    suite
        .execute(
            &mut Factory::new(contracts.account_factory),
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterUser {
                key: user.first_key(),
                key_hash: user.first_key_hash(),
                seed: 3,
                signature: user
                    .sign_arbitrary(RegisterUserData {
                        chain_id: chain_id.clone(),
                        key: user.first_key(),
                        key_hash: user.first_key_hash(),
                        seed: 3,
                        referrer: None,
                    })
                    .unwrap(),
                referrer: None,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(AccountStatus::Inactive);

    // Gateway forwards a transfer below the minimum deposit. The transfer
    // must succeed (gateway is the authorized sender) but the account stays
    // inactive since the amount is insufficient.
    suite
        .receive_warp_transfer(
            &mut accounts.owner,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            &user,
            1_000_000, // 1 USDC, below the 10 USDC minimum.
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(AccountStatus::Inactive);

    // The account should have been credited with the 1 USDC.
    suite
        .query_balance(&user, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::new(1_000_000));
}

/// Multiple sub-minimum gateway deposits to an inactive account must
/// accumulate: once the balance reaches the minimum, the next deposit (even
/// a small one) flips the account to `Active`.
#[tokio::test]
async fn gateway_deposits_accumulate_to_activate() {
    let (suite, mut accounts, codes, contracts, validator_sets) =
        setup_test_naive_with_custom_genesis(
            Default::default(),
            GenesisOption {
                account: AccountOption {
                    minimum_deposit: coins! { usdc::DENOM.clone() => 10_000_000 },
                    ..Preset::preset_test()
                },
                ..Preset::preset_test()
            },
        );
    let mut suite = HyperlaneTestSuite::new(suite, validator_sets, &contracts);

    suite.make_empty_block().await;

    let chain_id = suite.chain_id.clone();

    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        3,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    // Register the user without a deposit. The account starts as `Inactive`.
    suite
        .execute(
            &mut Factory::new(contracts.account_factory),
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterUser {
                key: user.first_key(),
                key_hash: user.first_key_hash(),
                seed: 3,
                signature: user
                    .sign_arbitrary(RegisterUserData {
                        chain_id: chain_id.clone(),
                        key: user.first_key(),
                        key_hash: user.first_key_hash(),
                        seed: 3,
                        referrer: None,
                    })
                    .unwrap(),
                referrer: None,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // First sub-minimum deposit (6 USDC < 10 USDC). Account stays inactive.
    suite
        .receive_warp_transfer(
            &mut accounts.owner,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            &user,
            6_000_000,
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(AccountStatus::Inactive);

    // Second sub-minimum deposit (4 USDC). On its own it wouldn't activate,
    // but combined with the first the balance now reaches 10 USDC, so the
    // account must flip to `Active`.
    suite
        .receive_warp_transfer(
            &mut accounts.owner,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            &user,
            4_000_000,
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(AccountStatus::Active);

    suite
        .query_balance(&user, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::new(10_000_000));
}

/// If minimum deposit is zero, then the account is automatically activated.
/// No need to make a deposit.
#[tokio::test]
async fn onboarding_without_deposit_when_minimum_deposit_is_zero() {
    // Set up the test with minimum deposit set to zero.
    let (mut suite, mut accounts, codes, contracts, _) = setup_test_naive(Default::default());

    let chain_id = suite.chain_id.clone();

    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        3,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    // Attempt to register a user without making a deposit.
    suite
        .execute(
            &mut accounts.owner,
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterUser {
                key: user.first_key(),
                key_hash: user.first_key_hash(),
                seed: 3,
                signature: user
                    .sign_arbitrary(RegisterUserData {
                        chain_id,
                        key: user.first_key(),
                        key_hash: user.first_key_hash(),
                        seed: 3,
                        referrer: None,
                    })
                    .unwrap(),
                referrer: None,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // Now that the user has been created, query it's index.
    let user = user.query_user_index(suite.querier());

    // The user's key should have been recorded in account factory.
    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user.user_index())),
        )
        .should_succeed_and(|res| {
            res.keys == btree_map! { user.first_key_hash() => user.first_key() }
        });

    // The user's account info should have been recorded in account factory.
    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user.user_index())),
        )
        .map(|res| res.accounts)
        .should_succeed_and_equal(btree_map! { 10u32 => user.address() });
    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryAccountRequest {
                address: user.address(),
            },
        )
        .should_succeed_and_equal(Account {
            // We have 10 genesis accounts (owner + users 1-9), indexed from
            // zero, so this one should have the index of 10.
            index: 10,
            owner: user.user_index(),
        });

    // The newly created account should be active.
    suite
        .query_wasm_smart(user.address(), account::QueryStatusRequest {})
        .should_succeed_and_equal(AccountStatus::Active);

    // The newly created account should have zero balance.
    suite
        .query_balances(&user)
        .should_succeed_and(|coins| coins.is_empty());
}

/// Since PR [#1460](https://github.com/left-curve/left-curve/pull/1460), it's
/// not longer necessary to make a deposit before onboarding.
/// However, we keep this test for the edge case -- what if someone sends a
/// transfer before creating the account? The user needs to be able to recover
/// the funds.
#[tokio::test]
async fn onboarding_with_deposit_when_minimum_deposit_is_zero() {
    // Set up the test with minimum deposit set to zero.
    let (suite, mut accounts, codes, contracts, validator_sets) =
        setup_test_naive(Default::default());
    let mut suite = HyperlaneTestSuite::new(suite, validator_sets, &contracts);

    // Generate a random key for the user.
    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        3,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    // Make the initial deposit, even though not required.
    // The deposit should be held in the bank contract as an orphaned transfer.
    suite
        .receive_warp_transfer(
            &mut accounts.owner,
            mock_arbitrum::DOMAIN,
            mock_arbitrum::USDC_WARP,
            &user,
            10_000_000,
        )
        .await
        .should_succeed();

    // Sign the `RegisterUserData`.
    let signature = user
        .sign_arbitrary(RegisterUserData {
            chain_id: suite.chain_id.clone(),
            key: user.first_key(),
            key_hash: user.first_key_hash(),
            seed: 3,
            referrer: None,
        })
        .unwrap();

    // Onboard the user.
    // Attempt to register a user without making a deposit.
    suite
        .execute(
            &mut accounts.owner,
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterUser {
                key: user.first_key(),
                key_hash: user.first_key_hash(),
                seed: 3,
                signature,
                referrer: None,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // Now that the user has been created, he can claim the orphaned transfer.
    let mut user = user.query_user_index(suite.querier());
    let user_address = user.address();

    suite
        .execute(
            &mut user,
            contracts.bank,
            &bank::ExecuteMsg::RecoverTransfer {
                sender: contracts.gateway,
                recipient: user_address,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // Make sure a single-signature account is created with the deposited balance.
    suite
        .query_balance(&user, usdc::DENOM.clone())
        .should_succeed_and_equal(Uint128::new(10_000_000));
}

/// Sending funds along with the `RegisterUser` message must be rejected: the
/// new account is created inactive and can't use the funds, so they would be
/// locked.
#[tokio::test]
async fn onboarding_with_funds_attached_is_rejected() {
    let (mut suite, mut accounts, codes, contracts, _) = setup_test_naive(Default::default());

    let chain_id = suite.chain_id.clone();

    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        3,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    let funds = coins! { usdc::DENOM.clone() => 1 };

    suite
        .execute(
            &mut accounts.owner,
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterUser {
                key: user.first_key(),
                key_hash: user.first_key_hash(),
                seed: 3,
                signature: user
                    .sign_arbitrary(RegisterUserData {
                        chain_id,
                        key: user.first_key(),
                        key_hash: user.first_key_hash(),
                        seed: 3,
                        referrer: None,
                    })
                    .unwrap(),
                referrer: None,
            },
            funds,
        )
        .await
        .should_fail_with_error("no funds expected during user registration");
}

#[tokio::test]
async fn update_key() {
    let (mut suite, _, codes, contracts, _) = setup_test_naive(Default::default());

    let chain_id = suite.chain_id.clone();

    // Create a new key offchain; then, predict what its address would be.
    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        0,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    // User uses account factory as sender to send an empty transaction.
    // Account factory should interpret this action as the user wishes to create
    // an account and claim the funds held in orphaned transfer in bank.
    suite
        .execute(
            &mut Factory::new(contracts.account_factory),
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterUser {
                key: user.first_key(),
                key_hash: user.first_key_hash(),
                seed: 0,
                signature: user
                    .sign_arbitrary(RegisterUserData {
                        chain_id: chain_id.clone(),
                        key: user.first_key(),
                        key_hash: user.first_key_hash(),
                        seed: 0,
                        referrer: None,
                    })
                    .unwrap(),
                referrer: None,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // Now that the user has been created, query it's index.
    let mut user = user.query_user_index(suite.querier());

    // Try to delete last key, should fail.
    let first_key_hash = user.first_key_hash();
    suite
        .execute(
            &mut user,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateKey {
                key: Op::Delete,
                key_hash: first_key_hash,
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error(format!(
            "can't delete the last key associated with user index {}",
            user.user_index()
        ));

    // Query keys should return only one key.
    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user.user_index())),
        )
        .map(|res| res.keys)
        .should_succeed_and_equal(btree_map! { user.first_key_hash() => user.first_key() });

    // Add a new key to the user's account.
    let (_, pk) = TestAccount::new_key_pair();
    let key_hash = pk.to_json_vec().unwrap().sha2_256();
    suite
        .execute(
            &mut user,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateKey {
                key: Op::Insert(pk),
                key_hash,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // Query keys should return two keys.
    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user.user_index())),
        )
        .map(|res| res.keys)
        .should_succeed_and_equal(btree_map! {
            user.first_key_hash() => user.first_key(),
            key_hash => pk,
        });

    // It shouldn't be able to add the same key more than once.
    suite
        .execute(
            &mut user,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateKey {
                key: Op::Insert(pk),
                key_hash,
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error(format!(
            "key is already associated with user index {}",
            user.user_index()
        ));

    // Delete the first key should be possible since there is another key.
    suite
        .execute(
            &mut user,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateKey {
                key: Op::Delete,
                key_hash: first_key_hash,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // Query keys should return only one key.
    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user.user_index())),
        )
        .map(|res| res.keys)
        .should_succeed_and_equal(btree_map! { key_hash => pk });
}

#[tokio::test]
async fn single_signature_account_count_limit() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    let user_index = accounts.user1.user_index();

    // User 1 should have one account now. Open 4 more.
    for _ in 2..=MAX_ACCOUNTS_PER_USER {
        suite
            .execute(
                &mut accounts.user1,
                contracts.account_factory,
                &account_factory::ExecuteMsg::RegisterAccount {},
                Coins::new(),
            )
            .await
            .should_succeed();
    }

    // Query user 1's account count that is stored in factory. Should be 5.
    suite
        .query_wasm_path(contracts.account_factory, &USERS.path(user_index))
        .map(|res| res.accounts.len())
        .should_succeed_and_equal(MAX_ACCOUNTS_PER_USER);

    // Attempt to open one more account. Should fail.
    suite
        .execute(
            &mut accounts.user1,
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterAccount {},
            Coins::new(),
        )
        .await
        .should_fail_with_error(format!("user {user_index} has reached max account count"));
}

/// New users should automatically get a `user_{index}` default username.
#[tokio::test]
async fn new_user_gets_default_username() {
    let (mut suite, _, codes, contracts, _) = setup_test_naive(Default::default());

    let chain_id = suite.chain_id.clone();

    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        0,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    suite
        .execute(
            &mut Factory::new(contracts.account_factory),
            contracts.account_factory,
            &account_factory::ExecuteMsg::RegisterUser {
                key: user.first_key(),
                key_hash: user.first_key_hash(),
                seed: 0,
                signature: user
                    .sign_arbitrary(RegisterUserData {
                        chain_id: chain_id.clone(),
                        key: user.first_key(),
                        key_hash: user.first_key_hash(),
                        seed: 0,
                        referrer: None,
                    })
                    .unwrap(),
                referrer: None,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    let user = user.query_user_index(suite.querier());
    let user_index = user.user_index();

    // Query the user and verify the default username is `user_{index}`.
    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user_index)),
        )
        .should_succeed_and(|res| {
            res.name == Username::default_for_index(user_index) && res.index == user_index
        });
}

/// Genesis users should also have a default `user_{index}` username.
#[test]
fn genesis_users_have_default_username() {
    let (suite, accounts, _, contracts, _) = setup_test_naive(Default::default());

    let user_index = accounts.user1.user_index();

    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user_index)),
        )
        .should_succeed_and(|res| {
            res.name == Username::default_for_index(user_index) && res.index == user_index
        });
}

/// A user with a default username can change it to a custom one.
#[tokio::test]
async fn update_default_username() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    let user_index = accounts.user1.user_index();
    let custom_name = Username::from_str("alice").unwrap();

    suite
        .execute(
            &mut accounts.user1,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateUsername(custom_name.clone()),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify the username was updated.
    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user_index)),
        )
        .should_succeed_and(|res| res.name == custom_name);

    // Verify the user can be looked up by the new username.
    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Name(custom_name)),
        )
        .should_succeed_and(|res| res.index == user_index);
}

/// A user with a custom username cannot change it again.
#[tokio::test]
async fn cannot_change_custom_username() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    let user_index = accounts.user1.user_index();
    let custom_name = Username::from_str("alice").unwrap();

    // First change: default → custom. Should succeed.
    suite
        .execute(
            &mut accounts.user1,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateUsername(custom_name),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Second change: custom → another custom. Should fail.
    let another_name = Username::from_str("bob").unwrap();
    suite
        .execute(
            &mut accounts.user1,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateUsername(another_name),
            Coins::new(),
        )
        .await
        .should_fail_with_error(format!(
            "a custom username is already set for user {user_index}"
        ));
}

/// Users cannot set a reserved `user_N` pattern as their custom username.
#[tokio::test]
async fn cannot_set_reserved_username() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    let reserved_name = Username::from_str("user_999").unwrap();

    suite
        .execute(
            &mut accounts.user1,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateUsername(reserved_name),
            Coins::new(),
        )
        .await
        .should_fail_with_error("usernames matching 'user_N' are reserved");
}

/// Two users cannot claim the same username.
#[tokio::test]
async fn cannot_claim_taken_username() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    let name = Username::from_str("alice").unwrap();

    // User 1 claims "alice".
    suite
        .execute(
            &mut accounts.user1,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateUsername(name.clone()),
            Coins::new(),
        )
        .await
        .should_succeed();

    // User 2 tries to claim "alice". Should fail.
    suite
        .execute(
            &mut accounts.user2,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateUsername(name.clone()),
            Coins::new(),
        )
        .await
        .should_fail_with_error(format!(
            "the username `{name}` is already associated with a user index"
        ));
}

/// `Username::is_default` correctly identifies system-generated usernames.
#[test]
fn username_is_default_detection() {
    assert!(Username::default_for_index(0).is_default());
    assert!(Username::default_for_index(42).is_default());
    assert!(Username::default_for_index(u32::MAX).is_default());

    assert!(!Username::from_str("alice").unwrap().is_default());
    assert!(!Username::from_str("user_").unwrap().is_default());
    assert!(!Username::from_str("user_abc").unwrap().is_default());
    assert!(!Username::from_str("user_0x1").unwrap().is_default());
}

/// `ForgotUsername` query returns `User` structs with populated `index`.
#[test]
fn forgot_username_returns_users_with_index() {
    let (suite, accounts, _, contracts, _) = setup_test_naive(Default::default());

    let user_index = accounts.user1.user_index();
    let key_hash = accounts.user1.first_key_hash();

    let users: Vec<account_factory::User> = suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryForgotUsernameRequest {
                key_hash,
                start_after: None,
                limit: None,
            },
        )
        .should_succeed();

    assert_eq!(users.len(), 1);
    assert_eq!(users[0].index, user_index);
    assert_eq!(users[0].name, Username::default_for_index(user_index));
}

/// The chain owner can reset a user's custom username back to the default,
/// after which the user can set a new custom username.
#[tokio::test]
async fn owner_can_reset_username() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    let user_index = accounts.user1.user_index();
    let custom_name = Username::from_str("alice").unwrap();

    // User sets a custom username.
    suite
        .execute(
            &mut accounts.user1,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateUsername(custom_name.clone()),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user_index)),
        )
        .should_succeed_and(|res| res.name == custom_name);

    // Owner resets it back to the default.
    suite
        .execute(
            &mut accounts.owner,
            contracts.account_factory,
            &account_factory::ExecuteMsg::ForceResetUsername { user_index },
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user_index)),
        )
        .should_succeed_and(|res| res.name == Username::default_for_index(user_index));

    // The user can now claim a new custom username.
    let new_name = Username::from_str("alice2").unwrap();
    suite
        .execute(
            &mut accounts.user1,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateUsername(new_name.clone()),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(
            contracts.account_factory,
            account_factory::QueryUserRequest(UserIndexOrName::Index(user_index)),
        )
        .should_succeed_and(|res| res.name == new_name);
}

/// A non-owner cannot reset a user's username.
#[tokio::test]
async fn non_owner_cannot_reset_username() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    let user_index = accounts.user1.user_index();

    suite
        .execute(
            &mut accounts.user2,
            contracts.account_factory,
            &account_factory::ExecuteMsg::ForceResetUsername { user_index },
            Coins::new(),
        )
        .await
        .should_fail_with_error("you don't have the right, O you don't have the right");
}
