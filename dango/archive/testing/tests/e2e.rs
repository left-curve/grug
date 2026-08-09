//! Full-pipeline e2e: a transfer broadcast to a mock Dango node must surface in
//! the activity read API once the `App` has ingested its block — exercising the
//! whole chain (node httpd → remote block source → committer → projection →
//! REST read API), not a single layer.

use {
    dango_archive_testing::Env,
    dango_primitives::{BroadcastClient, Coins, MOCK_CHAIN_ID, Message, NonEmpty, Signer},
    dango_types::constants::usdc,
    std::time::Duration,
};

#[tokio::test(flavor = "multi_thread")]
async fn transfer_is_indexed_into_the_activity_feed() {
    let mut env = Env::setup().await.expect("set up the e2e environment");

    // Sign + broadcast a transfer user1 -> user2. Read `user2`'s address by
    // reference so `env.accounts` is never partially moved.
    let recipient = *env.accounts.user2.address.inner();
    let tx = env
        .accounts
        .user1
        .sign_transaction(
            NonEmpty::new_unchecked(vec![
                Message::transfer(recipient, Coins::one(usdc::DENOM.clone(), 100).unwrap())
                    .unwrap(),
            ]),
            MOCK_CHAIN_ID,
            1_000_000,
        )
        .expect("sign the transfer");
    let broadcast = env
        .client
        .broadcast_tx(tx)
        .await
        .expect("broadcast the transfer");
    let tx_hash = broadcast.tx_hash.to_string();

    // The sender, formatted exactly as the `Address` scalar round-trips it.
    let sender = env.accounts.user1.address.inner().to_string();

    // Bridge the async pipeline: live tail (or healer) → store → projection.
    let data = env
        .wait_for_transactions_involving(&sender, "sender", Duration::from_secs(30))
        .await
        .expect("the transfer to be indexed");

    let items = data["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "exactly the one broadcast tx should match");

    let node = &items[0];
    assert_eq!(
        node["hash"].as_str().expect("hash string"),
        tx_hash,
        "the indexed hash matches the broadcast tx",
    );
    assert_eq!(
        node["sender"].as_str().expect("sender string"),
        sender,
        "the indexed sender is user1",
    );
    assert!(
        node["success"].as_bool().expect("success bool"),
        "the transfer succeeded",
    );
    assert!(
        node["blockHeight"].as_u64().expect("blockHeight u64") >= 1,
        "indexed at a real block height",
    );
}
