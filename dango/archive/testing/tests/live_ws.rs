//! Focused proof of the live `/ws` subscriber: drive `subscribe_full_blocks()`
//! directly against the mock node — no block source, no RocksDB store, no REST
//! healer — so a block can reach the assertion *only* through the WebSocket
//! `fullBlock` channel.
//!
//! The full `e2e` test runs the whole `RemoteBlockSource`, where the sentinel
//! fetcher (`/block/full/range`) can backfill a live block over REST; its own
//! comment reads "live tail (or healer)". That makes it a pipeline test, not a
//! live-tail test: it would still pass with a broken subscription. This one
//! cannot — if the `/ws` request shape, the subscribe ack, or the `fullBlock`
//! frame decoding is wrong, no block arrives and the test times out.

use {
    dango_archive_block_source::HttpdClient,
    dango_archive_testing::PendingEnv,
    dango_primitives::{BroadcastClient, Coins, MOCK_CHAIN_ID, Message, NonEmpty, Signer},
    dango_types::constants::usdc,
    futures::StreamExt,
    std::time::Duration,
    tokio::time::timeout,
};

#[tokio::test(flavor = "multi_thread")]
async fn live_subscriber_receives_blocks_over_ws() {
    // Mock node only (real chain + httpd serving `/ws`); the indexer is never
    // started, so this subscription is the sole reader of the node.
    let mut env = PendingEnv::setup().await.expect("set up the mock node");

    // Open the live `fullBlock` subscription at the current tip. Because it opens
    // at the tip (no `since`), every block it yields is one produced *after* this
    // call — i.e. genuinely live, not a replay.
    let client = HttpdClient::new(format!("http://127.0.0.1:{}", env.node_port))
        .expect("build the node httpd client");
    let mut blocks = client
        .subscribe_full_blocks()
        .await
        .expect("open the fullBlock subscription");

    // Mint exactly one block by broadcasting a transfer. The mock node is
    // `OnBroadcast`, so no empty block races ahead of it: the next block on the
    // stream is this one.
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
    let tx_hash = env
        .client
        .broadcast_tx(tx)
        .await
        .expect("broadcast the transfer")
        .tx_hash
        .to_string();

    // The block must arrive over `/ws` — there is no other path in this setup.
    let block = timeout(Duration::from_secs(10), blocks.next())
        .await
        .expect("a fullBlock frame within 10s")
        .expect("the stream is still open")
        .expect("the frame decodes into a FullBlock");

    assert!(
        block.block.info.height >= 1,
        "the live block sits at a real height",
    );
    assert_eq!(
        block.block.txs.len(),
        1,
        "the live block carries exactly the one broadcast tx",
    );
    assert_eq!(
        block.block.txs[0].1.to_string(),
        tx_hash,
        "the tx in the live block is the one we broadcast",
    );
}
