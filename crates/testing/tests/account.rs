use {
    grug_account::Credential,
    grug_testing::{Signer, TestBuilder},
    grug_types::{
        from_json_value, Coins, Duration, Message, NonZero, NumberConst, Timestamp, Tx, Uint256,
    },
    grug_vm_rust::ContractBuilder,
};

#[test]
fn check_tx_and_finalize() -> anyhow::Result<()> {
    let (mut suite, accounts) = TestBuilder::new()
        .add_account("rhaki", [("uatom", 100_u128)])?
        .add_account("larry", Coins::new())?
        .add_account("owner", Coins::new())?
        .set_genesis_time(Timestamp::from_nanos(0))
        .set_block_time(Duration::from_seconds(1))
        .set_owner("owner")?
        .build()?;

    let transfer_msg = Message::transfer(
        accounts["larry"].address,
        Coins::one("uatom", NonZero::new(Uint256::from(10_u128))),
    )?;

    // Create a tx to set sequence to 1.
    suite.send_message(&accounts["rhaki"], transfer_msg.clone())?;

    // Create a tx with sequence 0, 1, 2, 4.
    let info = suite.query_info().should_succeed();

    let txs: Vec<Tx> = [0, 1, 2, 4]
        .into_iter()
        .filter_map(|sequence| {
            (|| -> anyhow::Result<_> {
                // Sign the tx
                let tx = accounts["rhaki"].sign_transaction(
                    vec![transfer_msg.clone()],
                    0,
                    &info.chain_id,
                    sequence,
                )?;

                // Check the tx and if the result is ok, return the tx.
                suite.check_tx(tx.clone())?.result.into_std_result()?;

                Ok(tx)
            })()
            .ok()
        })
        .collect();

    // The tx with sequence 0 should fails check_tx.
    assert_eq!(txs.len(), 3);
    assert_eq!(
        from_json_value::<Credential>(txs[0].credential.clone())?.sequence,
        1
    );
    assert_eq!(
        from_json_value::<Credential>(txs[1].credential.clone())?.sequence,
        2
    );
    assert_eq!(
        from_json_value::<Credential>(txs[2].credential.clone())?.sequence,
        4
    );

    // Create a block with the txs.
    // The tx with sequence 1 should succeed.
    // The tx with sequence 2 should succeed.
    // The tx with sequence 4 should fail.
    let result = suite.make_block(txs)?;

    result.tx_outcomes[0].result.clone().should_succeed();
    result.tx_outcomes[1].result.clone().should_succeed();
    result.tx_outcomes[2].result.clone().should_fail();

    suite
        .query_balance(&accounts["rhaki"], "uatom")
        .should_succeed_and_equal(Uint256::from(70_u128));
    suite
        .query_balance(&accounts["larry"], "uatom")
        .should_succeed_and_equal(Uint256::from(30_u128));

    // Try create a block with a tx with sequence = 3
    let tx = accounts["rhaki"].sign_transaction(vec![transfer_msg], 0, &info.chain_id, 3)?;

    suite.make_block(vec![tx])?.tx_outcomes[0]
        .result
        .clone()
        .should_succeed();

    suite
        .query_balance(&accounts["rhaki"], "uatom")
        .should_succeed_and_equal(Uint256::from(60_u128));
    suite
        .query_balance(&accounts["larry"], "uatom")
        .should_succeed_and_equal(Uint256::from(40_u128));

    Ok(())
}

mod backrunner {
    use grug_types::{
        AuthCtx, AuthResponse, Coins, Message, Number, NumberConst, Response, StdResult, Tx,
        Uint128, Uint256,
    };

    // This contract is used for testing the backrunning feature, so we simply
    // skip all authentications in `authenticate`.
    pub fn authenticate(_ctx: AuthCtx, _tx: Tx) -> StdResult<AuthResponse> {
        // Do request backrunning.
        Ok(AuthResponse::new().request_backrun(true))
    }

    // Accounts can do any action while backrunning. In this test, the account
    // attempts to mint itself a token.
    pub fn backrun(ctx: AuthCtx, _tx: Tx) -> StdResult<Response> {
        let info = ctx.querier.query_info()?;

        Ok(Response::new().add_message(Message::execute(
            info.config.bank,
            &grug_bank::ExecuteMsg::Mint {
                to: ctx.contract,
                denom: "nft/badkids/1".to_string(),
                amount: Uint256::ONE,
            },
            Coins::new(),
        )?))
    }

    // The account can also reject and revert state changes from the messages
    // simply by throwing en error while backrunning.
    pub fn bugged_backrun(_ctx: AuthCtx, _tx: Tx) -> StdResult<Response> {
        let _ = Uint128::ONE.checked_div(Uint128::ZERO)?;

        Ok(Response::new())
    }
}

#[test]
fn backrunning_works() -> anyhow::Result<()> {
    let account = ContractBuilder::new(Box::new(grug_account::instantiate))
        .with_receive(Box::new(grug_account::receive))
        .with_authenticate(Box::new(backrunner::authenticate))
        .with_backrun(Box::new(backrunner::backrun))
        .build();

    let (mut suite, accounts) = TestBuilder::new()
        .set_account_code(account, |public_key| grug_account::InstantiateMsg {
            public_key,
        })
        .add_account(
            "sender",
            Coins::one("ugrug", NonZero::new(Uint256::from(50_000_u128))),
        )?
        .add_account("receiver", Coins::new())?
        .set_owner("sender")?
        .build()?;

    // Attempt to send a transaction
    suite.transfer(
        &accounts["sender"],
        accounts["receiver"].address,
        Coins::one("ugrug", NonZero::new(Uint256::from(123_u128))),
    )?;

    // Receiver should have received ugrug, and sender should have minted bad kids.
    suite
        .query_balance(&accounts["receiver"], "ugrug")
        .should_succeed_and_equal(Uint256::from(123_u128));
    suite
        .query_balance(&accounts["sender"], "ugrug")
        .should_succeed_and_equal(Uint256::from(50_000_u128 - 123));
    suite
        .query_balance(&accounts["sender"], "nft/badkids/1")
        .should_succeed_and_equal(Uint256::ONE);

    Ok(())
}

#[test]
fn backrunning_with_error() -> anyhow::Result<()> {
    let bugged_account = ContractBuilder::new(Box::new(grug_account::instantiate))
        .with_receive(Box::new(grug_account::receive))
        .with_authenticate(Box::new(backrunner::authenticate))
        .with_backrun(Box::new(backrunner::bugged_backrun))
        .build();

    let (mut suite, accounts) = TestBuilder::new()
        .set_account_code(bugged_account, |public_key| grug_account::InstantiateMsg {
            public_key,
        })
        .add_account(
            "sender",
            Coins::one("ugrug", NonZero::new(Uint256::from(50_000_u128))),
        )?
        .add_account("receiver", Coins::new())?
        .set_owner("sender")?
        .build()?;

    // Attempt to make a transfer; should fail.
    suite
        .send_message(
            &accounts["sender"],
            Message::transfer(
                accounts["receiver"].address,
                Coins::one("ugrug", NonZero::new(Uint256::from(123_u128))),
            )?,
        )?
        .result
        .should_fail_with_error("division by zero: 1 / 0");

    // Transfer should have been reverted, and sender doesn't get bad kids.
    suite
        .query_balance(&accounts["receiver"], "ugrug")
        .should_succeed_and_equal(Uint256::ZERO);
    suite
        .query_balance(&accounts["sender"], "ugrug")
        .should_succeed_and_equal(Uint256::from(50_000_u128));
    suite
        .query_balance(&accounts["sender"], "nft/badkids/1")
        .should_succeed_and_equal(Uint256::ZERO);

    Ok(())
}
