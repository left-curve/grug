use {
    dango_primitives::{
        Addressable, CheckedContractEvent, Coins, Denom, JsonDeExt, LengthBounded, Part,
        QuerierExt, ResultExt, SearchEvent, addr, btree_map, coins,
    },
    dango_testing::{BalanceChange, setup_test_naive},
    dango_types::{
        bank::{
            self, OrphanedTransferResponseItem, QueryOrphanedTransfersByRecipientRequest,
            QueryOrphanedTransfersBySenderRequest, QueryOrphanedTransfersRequest, Received, Sent,
            TransferOrphaned,
        },
        constants::{dango, eth, usdc},
    },
    std::collections::BTreeSet,
};

#[tokio::test]
async fn batch_transfer() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    // Create two non-existent recipient addresses.
    let dead1 = addr!("000000000000000000000000000000000000dead");
    let dead2 = addr!("00000000000000000000000000000000deaddead");

    suite.balances().record_many([
        &accounts.owner.address(),
        &accounts.user1.address(),
        &accounts.user2.address(),
        &contracts.bank,
        &dead1,
        &dead2,
    ]);

    // Owner makes a multi-send to users 1, 2, and the non-existent recipient.
    let events = suite
        .batch_transfer(
            &mut accounts.owner,
            btree_map! {
                accounts.user1.address() => coins! {
                    dango::DENOM.clone() => 100,
                },
                accounts.user2.address() => coins! {
                    dango::DENOM.clone() => 200,
                    usdc::DENOM.clone() => 300,
                },
                dead1 => coins! {
                    dango::DENOM.clone() => 400,
                },
                dead2 => coins! {
                    usdc::DENOM.clone() => 500,
                },
            },
        )
        .await
        .should_succeed()
        .events;

    // Check the emitted events are correct.
    // `sent` events:
    {
        let sends = events
            .clone()
            .search_event::<CheckedContractEvent>()
            .with_predicate(|e| e.ty == "sent")
            .take()
            .all()
            .into_iter()
            .map(|e| e.event.data.deserialize_json::<Sent>().unwrap())
            .collect::<Vec<_>>();

        assert!(vectors_have_same_elements(
            sends,
            vec![
                Sent {
                    user: accounts.owner.address(),
                    to: accounts.user1.address(),
                    coins: coins! {
                        dango::DENOM.clone() => 100,
                    },
                },
                Sent {
                    user: accounts.owner.address(),
                    to: accounts.user2.address(),
                    coins: coins! {
                        dango::DENOM.clone() => 200,
                        usdc::DENOM.clone() => 300,
                    },
                },
                Sent {
                    user: accounts.owner.address(),
                    to: contracts.bank, // The orphaned transfer goes to the bank.
                    coins: coins! {
                        dango::DENOM.clone() => 400,
                    },
                },
                Sent {
                    user: accounts.owner.address(),
                    to: contracts.bank, // The orphaned transfer goes to the bank.
                    coins: coins! {
                        usdc::DENOM.clone() => 500,
                    },
                },
            ]
        ));
    }

    // `received` events:
    {
        let receives = events
            .clone()
            .search_event::<CheckedContractEvent>()
            .with_predicate(|e| e.ty == "received")
            .take()
            .all()
            .into_iter()
            .map(|e| e.event.data.deserialize_json::<Received>().unwrap())
            .collect::<Vec<_>>();

        assert!(vectors_have_same_elements(
            receives,
            vec![
                Received {
                    user: accounts.user1.address(),
                    from: accounts.owner.address(),
                    coins: coins! {
                        dango::DENOM.clone() => 100,
                    },
                },
                Received {
                    user: accounts.user2.address(),
                    from: accounts.owner.address(),
                    coins: coins! {
                        dango::DENOM.clone() => 200,
                        usdc::DENOM.clone() => 300,
                    },
                },
                Received {
                    user: contracts.bank, // The orphaned transfer goes to the bank.
                    from: accounts.owner.address(),
                    coins: coins! {
                        dango::DENOM.clone() => 400,
                    },
                },
                Received {
                    user: contracts.bank, // The orphaned transfer goes to the bank.
                    from: accounts.owner.address(),
                    coins: coins! {
                        usdc::DENOM.clone() => 500,
                    },
                },
            ]
        ));
    }

    // `transfer_orphaned` events:
    {
        let orphans = events
            .search_event::<CheckedContractEvent>()
            .with_predicate(|e| e.ty == "transfer_orphaned")
            .take()
            .all()
            .into_iter()
            .map(|e| e.event.data.deserialize_json::<TransferOrphaned>().unwrap())
            .collect::<Vec<_>>();

        assert!(vectors_have_same_elements(
            orphans,
            vec![
                TransferOrphaned {
                    from: accounts.owner.address(),
                    to: dead1,
                    coins: coins! {
                        dango::DENOM.clone() => 400,
                    },
                },
                TransferOrphaned {
                    from: accounts.owner.address(),
                    to: dead2,
                    coins: coins! {
                        usdc::DENOM.clone() => 500,
                    },
                },
            ]
        ));
    }

    // Check the balance changes.
    {
        suite.balances().should_change(
            &accounts.owner,
            btree_map! {
                dango::DENOM.clone() => BalanceChange::Decreased(700),
                usdc::DENOM.clone() => BalanceChange::Decreased(800),
            },
        );

        suite.balances().should_change(
            &accounts.user1,
            btree_map! {
                dango::DENOM.clone() => BalanceChange::Increased(100),
            },
        );

        suite.balances().should_change(
            &accounts.user2,
            btree_map! {
                dango::DENOM.clone() => BalanceChange::Increased(200),
                usdc::DENOM.clone() => BalanceChange::Increased(300),
            },
        );

        // The token that are supposed to go to the non-existing accounts should
        // have been withheld in the bank contract as orphaned transfers.
        suite.balances().should_change(
            &contracts.bank,
            btree_map! {
                dango::DENOM.clone() => BalanceChange::Increased(400),
                usdc::DENOM.clone() => BalanceChange::Increased(500),
            },
        );

        // The non-existing accounts should have no balance.
        suite.balances().should_change(&dead1, btree_map! {});
        suite.balances().should_change(&dead2, btree_map! {});
    }

    // The orphaned transfers should have been recorded.
    {
        suite
            .query_wasm_smart(
                contracts.bank,
                QueryOrphanedTransfersRequest {
                    start_after: None,
                    limit: None,
                },
            )
            .should_succeed_and_equal(vec![
                OrphanedTransferResponseItem {
                    sender: accounts.owner.address(),
                    recipient: dead1,
                    amount: coins! { dango::DENOM.clone() => 400 },
                },
                OrphanedTransferResponseItem {
                    sender: accounts.owner.address(),
                    recipient: dead2,
                    amount: coins! { usdc::DENOM.clone() => 500 },
                },
            ]);

        suite
            .query_wasm_smart(
                contracts.bank,
                QueryOrphanedTransfersBySenderRequest {
                    sender: accounts.owner.address(),
                    start_after: None,
                    limit: None,
                },
            )
            .should_succeed_and_equal(btree_map! {
                dead1 => coins! { dango::DENOM.clone() => 400 },
                dead2 => coins! { usdc::DENOM.clone() => 500 },
            });

        suite
            .query_wasm_smart(
                contracts.bank,
                QueryOrphanedTransfersByRecipientRequest {
                    recipient: dead1,
                    start_after: None,
                    limit: None,
                },
            )
            .should_succeed_and_equal(btree_map! {
                accounts.owner.address() => coins! { dango::DENOM.clone() => 400 },
            });

        suite
            .query_wasm_smart(
                contracts.bank,
                QueryOrphanedTransfersByRecipientRequest {
                    recipient: dead2,
                    start_after: None,
                    limit: None,
                },
            )
            .should_succeed_and_equal(btree_map! {
                accounts.owner.address() => coins! { usdc::DENOM.clone() => 500 },
            });
    }
}

/// Returns whether two vectors have the same elements, not necessarily in the
/// same order.
///
/// This assumes the type `T` doesn't implement `Hash` or `Ord`, so we can't
/// trivially do this with a `HashSet` or `BTreeSet`.
fn vectors_have_same_elements<T>(mut a: Vec<T>, mut b: Vec<T>) -> bool
where
    T: PartialEq,
{
    // If lengths differ, they can't have the same elements.
    if a.len() != b.len() {
        return false;
    }

    // Process each element in `a`.
    while let Some(elem) = a.pop() {
        // Try to find and remove a matching element in `b`.
        if let Some(index) = b.iter().position(|e| e == &elem) {
            b.remove(index);
        } else {
            // If the element is not found in `b`, the vectors are different.
            return false;
        }
    }

    // If `b` is left empty, then all elements are matched.
    b.is_empty()
}

#[tokio::test]
async fn set_namespace_owner_can_only_be_called_by_owner() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    // Attempt to set namespace owner as non-owner. Should fail.
    suite
        .execute(
            &mut accounts.user1,
            contracts.bank,
            &bank::ExecuteMsg::SetNamespaceOwner {
                namespace: Part::new_unchecked("test"),
                owner: accounts.user2.address(),
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error("you don't have the right, O you don't have the right");

    // Attempt to set namespace owner as owner. Should succeed.
    suite
        .execute(
            &mut accounts.owner,
            contracts.bank,
            &bank::ExecuteMsg::SetNamespaceOwner {
                namespace: Part::new_unchecked("test"),
                owner: accounts.user2.address(),
            },
            Coins::new(),
        )
        .await
        .should_succeed();
}

#[tokio::test]
async fn set_metadata_can_only_be_called_by_non_namespace_owner() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    // Attempt to set metadata as non-namespace owner. Should fail.
    suite
        .execute(
            &mut accounts.user1,
            contracts.bank,
            &bank::ExecuteMsg::SetMetadata {
                denom: Denom::new_unchecked(["testing", "test"]),
                metadata: bank::Metadata {
                    name: LengthBounded::new_unchecked("Very testy token".to_string()),
                    symbol: LengthBounded::new_unchecked("TESTY".to_string()),
                    description: None,
                    decimals: 6,
                },
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error("sender does not own the namespace `testing`");

    // Set user1 as namespace owner of testing
    suite
        .execute(
            &mut accounts.owner,
            contracts.bank,
            &bank::ExecuteMsg::SetNamespaceOwner {
                namespace: Part::new_unchecked("testing"),
                owner: accounts.user1.address(),
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // Attempt to set metadata as user1. Should succeed.
    suite
        .execute(
            &mut accounts.user1,
            contracts.bank,
            &bank::ExecuteMsg::SetMetadata {
                denom: Denom::new_unchecked(["testing", "test"]),

                metadata: bank::Metadata {
                    name: LengthBounded::new_unchecked("Very testy token".to_string()),
                    symbol: LengthBounded::new_unchecked("TESTY".to_string()),
                    description: None,
                    decimals: 6,
                },
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // Assert metadata is set correctly
    suite
        .query_wasm_smart(
            contracts.bank,
            bank::QueryMetadataRequest {
                denom: Denom::new_unchecked(["testing", "test"]),
            },
        )
        .should_succeed_and_equal(bank::Metadata {
            name: LengthBounded::new_unchecked("Very testy token".to_string()),
            symbol: LengthBounded::new_unchecked("TESTY".to_string()),
            description: None,
            decimals: 6,
        });
}

#[tokio::test]
async fn set_namespace_owner_works() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    // Query namespace owner of testing. Should fail because it's not set.
    suite
        .query_wasm_smart(
            contracts.bank,
            bank::QueryNamespaceOwnerRequest {
                namespace: Part::new_unchecked("testing"),
            },
        )
        .should_fail_with_error(
            "msg: data not found! type: dango_primitives::encoded_bytes::EncodedBytes",
        );

    // Set user1 as namespace owner of testing
    suite
        .execute(
            &mut accounts.owner,
            contracts.bank,
            &bank::ExecuteMsg::SetNamespaceOwner {
                namespace: Part::new_unchecked("testing"),
                owner: accounts.user1.address(),
            },
            Coins::new(),
        )
        .await
        .should_succeed();

    // Query namespace owner of testing. Should succeed.
    suite
        .query_wasm_smart(
            contracts.bank,
            bank::QueryNamespaceOwnerRequest {
                namespace: Part::new_unchecked("testing"),
            },
        )
        .should_succeed_and_equal(accounts.user1.address());
}

#[test]
fn query_namespace_owners_works() {
    let (suite, _, _, contracts, _) = setup_test_naive(Default::default());

    // Query namespace owners. Should succeed.
    suite
        .query_wasm_smart(
            contracts.bank,
            bank::QueryNamespaceOwnersRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and_equal(btree_map! {
            Part::new_unchecked("bridge") => contracts.gateway,
        });

    // Query namespace owners with limit. Should succeed.
    suite
        .query_wasm_smart(
            contracts.bank,
            bank::QueryNamespaceOwnersRequest {
                start_after: None,
                limit: Some(1),
            },
        )
        .should_succeed_and_equal(btree_map! {
            Part::new_unchecked("bridge") => contracts.gateway,
        });
}

#[test]
fn query_metadatas_works() {
    let (suite, _, _, contracts, _) = setup_test_naive(Default::default());

    suite
        .query_wasm_smart(
            contracts.bank,
            bank::QueryMetadatasRequest {
                start_after: None,
                limit: None,
            },
        )
        .should_succeed_and(|metadatas| {
            metadatas.keys().collect::<BTreeSet<_>>()
                == BTreeSet::from([
                    &Denom::new_unchecked(["bridge", "eth"]),
                    &Denom::new_unchecked(["bridge", "usdc"]),
                    &Denom::new_unchecked(["dango"]),
                ])
        });

    // Start after eth
    suite
        .query_wasm_smart(
            contracts.bank,
            bank::QueryMetadatasRequest {
                start_after: Some(Denom::new_unchecked(["bridge", "eth"])),
                limit: None,
            },
        )
        .should_succeed_and(|metadatas| {
            metadatas.keys().collect::<BTreeSet<_>>()
                == BTreeSet::from([
                    &Denom::new_unchecked(["bridge", "usdc"]),
                    &Denom::new_unchecked(["dango"]),
                ])
        });

    // Limit 2
    suite
        .query_wasm_smart(
            contracts.bank,
            bank::QueryMetadatasRequest {
                start_after: None,
                limit: Some(2),
            },
        )
        .should_succeed_and(|metadatas| {
            metadatas.keys().collect::<BTreeSet<_>>()
                == BTreeSet::from([
                    &Denom::new_unchecked(["bridge", "eth"]),
                    &Denom::new_unchecked(["bridge", "usdc"]),
                ])
        });
}

#[tokio::test]
async fn top_level_denom_cannot_be_minted_or_burned_by_non_chain_owner() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(Default::default());

    // Attempt to mint as non-owner. Should fail.
    suite
        .execute(
            &mut accounts.user1,
            contracts.bank,
            &bank::ExecuteMsg::Mint {
                to: accounts.user2.address(),
                coins: coins! { dango::DENOM.clone() => 100 },
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error(
            "only chain owner can mint, burn, or set metadata of top-level denoms",
        );

    // Attempt to burn as non-owner. Should fail.
    suite
        .execute(
            &mut accounts.user1,
            contracts.bank,
            &bank::ExecuteMsg::Burn {
                from: accounts.user2.address(),
                coins: coins! { dango::DENOM.clone() => 100 },
            },
            Coins::new(),
        )
        .await
        .should_fail_with_error(
            "only chain owner can mint, burn, or set metadata of top-level denoms",
        );
}

#[test]
fn query_supplies_works() {
    let (suite, ..) = setup_test_naive(Default::default());

    suite
        .query_supplies(None, None)
        .should_succeed_and_equal(coins! {
            dango::DENOM.clone() => 1000900000000000000,
            usdc::DENOM.clone() => 1000000000000,
            eth::DENOM.clone() => 20000000200000000000,
        });

    // Start after eth
    suite
        .query_supplies(Some(eth::DENOM.clone()), None)
        .should_succeed_and_equal(coins! {
            usdc::DENOM.clone() => 1000000000000,
            dango::DENOM.clone() => 1000900000000000000,
        });

    // Limit 2
    suite
        .query_supplies(None, Some(2))
        .should_succeed_and_equal(coins! {
            usdc::DENOM.clone() => 1000000000000,
            eth::DENOM.clone() => 20000000200000000000,
        });
}
