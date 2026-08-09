//! Drives every `ArchiveClient` method against the real archive stack (mock
//! node → block source → committer → projection → REST read API) — the drift
//! guard for the typed client: it must deserialize exactly what the server
//! serializes.
//!
//! Fixture arithmetic (see `backfill.rs`): the mock chain runs with a zero
//! gas-fee rate, so each transfer produces exactly one `Transfer` event and two
//! bank contract-events (`sent` / `received`), and each broadcast drives
//! exactly one block. The test env anchors the `/events/perps` shortcut on the
//! **bank** contract, so the shortcut serves the same events as
//! `/events/contract?contract={bank}`.

use {
    dango_archive_testing::{Env, broadcast_transfer},
    dango_primitives::BlockClient,
    dango_sdk::{
        ArchiveClient,
        archive::{AddressRole, EventType, UnitKind},
    },
    std::time::Duration,
};

/// How many transfers the test broadcasts (one block each).
const TRANSFERS: usize = 3;

#[tokio::test(flavor = "multi_thread")]
async fn archive_client_covers_every_route() {
    let mut env = Env::setup().await.expect("set up the e2e environment");
    let archive = ArchiveClient::new(env.read_url()).expect("build the archive client");

    let sender = *env.accounts.user1.address.inner();
    let recipient = *env.accounts.user2.address.inner();

    // Broadcast the transfers, then bridge the async ingest pipeline by
    // waiting for the last one to be indexed (ingest is in block order, so the
    // earlier ones are then indexed too).
    let mut hashes = vec![];
    for i in 0..TRANSFERS {
        let hash = broadcast_transfer(
            &env.client,
            &mut env.accounts.user1,
            recipient,
            100 + i as u128,
        )
        .await
        .expect("broadcast a transfer");
        hashes.push(hash);
    }
    env.wait_for_tx_indexed(&hashes.last().unwrap().to_string(), Duration::from_secs(30))
        .await
        .expect("the last transfer to be indexed");

    // ---- /up ----

    archive.up().await.expect("the liveness probe answers");

    // ---- blocks ----

    let latest = archive
        .latest_block()
        .await
        .expect("query the latest block")
        .expect("the archive holds blocks");
    let tip = latest.block.info.height;
    assert!(
        tip >= TRANSFERS as u64,
        "the frontier covers the broadcast blocks"
    );

    let by_height = archive
        .block(tip)
        .await
        .expect("query the block by height")
        .expect("the frontier block is held");
    assert_eq!(by_height.block.info.height, tip);
    assert_eq!(by_height.block.info.hash, latest.block.info.hash);
    assert_eq!(by_height.outcome.height, tip);

    // An absent height is `None`, not an error.
    assert!(
        archive
            .block(tip + 1_000_000)
            .await
            .expect("query an absent height")
            .is_none()
    );

    // The `BlockClient` impl serves the same data, erroring on absence.
    let block = archive
        .query_block(Some(tip))
        .await
        .expect("BlockClient::query_block");
    assert_eq!(block.info.height, tip);
    let outcome = archive
        .query_block_outcome(None)
        .await
        .expect("BlockClient::query_block_outcome");
    assert_eq!(outcome.height, tip);
    assert!(archive.query_block(Some(tip + 1_000_000)).await.is_err());

    // ---- /transactions/{hash} ----

    let units = archive
        .transactions_by_hash(hashes[0])
        .await
        .expect("look up the first transfer by hash");
    assert_eq!(units.len(), 1, "the hash resolves to exactly one unit");
    let unit = &units[0];
    assert_eq!(unit.hash, Some(hashes[0]));
    assert_eq!(unit.sender, Some(sender));
    assert_eq!(unit.kind, UnitKind::Transaction);
    assert!(unit.success);
    assert!(unit.tx.is_some(), "the submitted tx is hydrated");
    assert!(unit.outcome.is_some(), "the execution outcome is hydrated");

    // ---- /transactions/involving/{address} ----

    let page = archive
        .transactions_involving(sender, Some(AddressRole::Sender), None, None, None)
        .await
        .expect("the sender's transactions");
    assert_eq!(page.items.len(), TRANSFERS);
    assert!(
        page.items
            .windows(2)
            .all(|w| w[0].block_height >= w[1].block_height),
        "the feed is newest-first"
    );
    assert!(!page.page_info.has_next_page);

    // Narrowing to cron units matches nothing (only transactions ran).
    let crons = archive
        .transactions_involving(sender, None, Some(UnitKind::Cron), None, None)
        .await
        .expect("the sender's cron units");
    assert!(crons.items.is_empty());

    // A `first: 1` cursor walk reassembles the same feed.
    let walked = archive
        .paginate_transactions_involving(sender, Some(AddressRole::Sender), None, Some(1))
        .await
        .expect("paginate the sender's transactions");
    assert_eq!(walked, page.items);

    // ---- /events ----

    let transfers = archive
        .events(&[EventType::Transfer], None, None, None)
        .await
        .expect("the transfer events");
    assert_eq!(
        transfers.items.len(),
        TRANSFERS,
        "exactly one Transfer event per transfer"
    );
    assert!(
        transfers
            .items
            .iter()
            .all(|event| event.ty == EventType::Transfer && event.data.is_some())
    );

    let involving_recipient = archive
        .events(&[], Some(recipient), None, None)
        .await
        .expect("the recipient's events");
    assert!(
        !involving_recipient.items.is_empty(),
        "the recipient participates in the transfers' events"
    );

    // Neither `types` nor `involved`: the server refuses (no index anchor).
    assert!(archive.events(&[], None, None, None).await.is_err());

    // ---- /events/contract and the /events/perps shortcut ----

    let bank = env.contracts.bank;
    let bank_events = archive
        .contract_events(bank, None, &[], None, None)
        .await
        .expect("the bank's contract events");
    assert_eq!(
        bank_events.items.len(),
        2 * TRANSFERS,
        "a `sent` and a `received` per transfer"
    );
    assert!(bank_events.items.iter().all(|event| {
        event.ty == EventType::ContractEvent
            && event.contract == Some(bank)
            && matches!(event.name.as_deref(), Some("sent" | "received"))
    }));

    let sent = archive
        .contract_events(bank, None, &["sent"], None, None)
        .await
        .expect("the bank's `sent` events");
    assert_eq!(sent.items.len(), TRANSFERS);
    assert!(
        sent.items
            .iter()
            .all(|event| event.name.as_deref() == Some("sent"))
    );

    // The test env anchors the perps shortcut on the bank, so the shortcut
    // must serve the very same events.
    let via_shortcut = archive
        .perps_events(None, &["sent"], None, None)
        .await
        .expect("the `sent` events via the perps shortcut");
    assert_eq!(via_shortcut.items, sent.items);

    // A `first: 1` cursor walk reassembles the same feed.
    let walked = archive
        .paginate_contract_events(bank, None, &[], Some(1))
        .await
        .expect("paginate the bank's contract events");
    assert_eq!(walked, bank_events.items);

    let walked = archive
        .paginate_perps_events(None, &["sent"], Some(1))
        .await
        .expect("paginate via the perps shortcut");
    assert_eq!(walked, sent.items);

    // And the generic events walk.
    let walked = archive
        .paginate_events(&[EventType::Transfer], None, Some(1))
        .await
        .expect("paginate the transfer events");
    assert_eq!(walked, transfers.items);
}
