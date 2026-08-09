use {
    crate::{ContractWrapper, HyperlaneTestSuite, TestAccount, TestAccounts, mock_ethereum},
    dango_db_memory::MemDb,
    dango_genesis::{Codes, Contracts},
    dango_indexer_hooked::HookedIndexer,
    dango_primitives::{Coins, Hash256, HashExt, JsonSerExt, Op, ResultExt},
    dango_proposal_preparer::ProposalPreparer,
    dango_pyth_client::PythClientCache,
    dango_types::{account_factory, auth::Key, constants::usdc},
    dango_vm_rust::RustVm,
    std::ops::DerefMut,
};

pub async fn add_user_public_key(
    suite: &mut HyperlaneTestSuite<MemDb, RustVm, ProposalPreparer<PythClientCache>, HookedIndexer>,
    contracts: &Contracts,
    test_account: &mut TestAccount,
) -> (Key, Hash256) {
    let (_, pk) = TestAccount::new_key_pair();
    let key_hash = pk.to_json_vec().unwrap().sha2_256();
    suite
        .execute(
            test_account,
            contracts.account_factory,
            &account_factory::ExecuteMsg::UpdateKey {
                key: Op::Insert(pk),
                key_hash,
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    (pk, key_hash)
}

pub async fn add_account_with_existing_user(
    suite: &mut HyperlaneTestSuite<MemDb, RustVm, ProposalPreparer<PythClientCache>, HookedIndexer>,
    contracts: &Contracts,
    test_account: &mut TestAccount,
) -> TestAccount {
    test_account
        .register_new_account(
            suite.deref_mut(),
            contracts.account_factory,
            Coins::one(usdc::DENOM.clone(), 100_000_000).unwrap(), // Make sure this is bigger than the minimum deposit.
        )
        .await
        .unwrap()
}

pub async fn create_user_and_account(
    suite: &mut HyperlaneTestSuite<MemDb, RustVm, ProposalPreparer<PythClientCache>, HookedIndexer>,
    accounts: &mut TestAccounts,
    contracts: &Contracts,
    codes: &Codes<ContractWrapper>,
) -> TestAccount {
    let user = TestAccount::new_random().predict_address(
        contracts.account_factory,
        0,
        codes.account.to_bytes().sha2_256(),
        true,
    );

    // Create the user and its first single-signature account.
    user.register_user(suite.deref_mut(), contracts.account_factory, Coins::new())
        .await;

    // Make the initial deposit.
    suite
        .receive_warp_transfer(
            &mut accounts.owner,
            mock_ethereum::DOMAIN,
            mock_ethereum::USDC_WARP,
            &user,
            150_000_000, // Make sure this is bigger than the minimum deposit.
        )
        .await
        .should_succeed();

    user.query_user_index(suite.querier())
}
