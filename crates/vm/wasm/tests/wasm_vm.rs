use {
    grug_testing::TestBuilder,
    grug_types::{
        to_json_value, Addr, Binary, Coins, Empty, Message, NonZero, NumberConst, Uint128,
    },
    grug_vm_wasm::{VmError, WasmVm},
    std::{collections::BTreeMap, fs, io, vec},
};

const WASM_CACHE_CAPACITY: usize = 10;
const DENOM: &str = "ugrug";

fn read_wasm_file(filename: &str) -> io::Result<Binary> {
    let path = format!("{}/testdata/{filename}", env!("CARGO_MANIFEST_DIR"));
    fs::read(path).map(Into::into)
}

#[test]
fn bank_transfers() -> anyhow::Result<()> {
    let (mut suite, accounts) = TestBuilder::new_with_vm(WasmVm::new(WASM_CACHE_CAPACITY))
        .add_account("sender", Coins::new_one(DENOM, NonZero::new(100_u128)))?
        .add_account("receiver", Coins::new_empty())?
        .build()?;

    // Check that sender has been given 100 ugrug
    suite
        .query_balance(&accounts["sender"], DENOM)
        .should_succeed_and_equal(Uint128::new(100))?;
    suite
        .query_balance(&accounts["receiver"], DENOM)
        .should_succeed_and_equal(Uint128::ZERO)?;

    // Sender sends 70 ugrug to the receiver across multiple messages
    suite
        .execute_messages_with_gas(&accounts["sender"], 2_500_000, vec![
            Message::Transfer {
                to: accounts["receiver"].address.clone(),
                coins: Coins::new_one(DENOM, NonZero::new(10_u128)),
            },
            Message::Transfer {
                to: accounts["receiver"].address.clone(),
                coins: Coins::new_one(DENOM, NonZero::new(15_u128)),
            },
            Message::Transfer {
                to: accounts["receiver"].address.clone(),
                coins: Coins::new_one(DENOM, NonZero::new(20_u128)),
            },
            Message::Transfer {
                to: accounts["receiver"].address.clone(),
                coins: Coins::new_one(DENOM, NonZero::new(25_u128)),
            },
        ])?
        .should_succeed()?;

    // Check balances again
    suite
        .query_balance(&accounts["sender"], DENOM)
        .should_succeed_and_equal(Uint128::new(30))?;
    suite
        .query_balance(&accounts["receiver"], DENOM)
        .should_succeed_and_equal(Uint128::new(70))?;

    let info = suite.query_info().should_succeed()?;

    let holders: BTreeMap<Addr, Uint128> = suite
        .query_wasm_smart(info.config.bank.clone(), &grug_bank::QueryMsg::Holders {
            denom: DENOM.to_string(),
            start_after: None,
            limit: None,
        })
        .should_succeed()?;

    assert_eq!(
        holders,
        vec![
            (accounts["sender"].address.clone(), Uint128::new(30)),
            (accounts["receiver"].address.clone(), Uint128::new(70))
        ]
        .into_iter()
        .collect::<BTreeMap<Addr, Uint128>>()
    );

    // try execute
    suite
        .execute_message_with_gas(
            &accounts["sender"],
            300000000000,
            Message::execute(
                info.config.bank,
                &grug_bank::ExecuteMsg::Mint {
                    to: accounts["receiver"].address.clone(),
                    denom: "uosmo".to_string(),
                    amount: Uint128::from(100_u128),
                },
                vec![],
            )
            .unwrap(),
        )
        .unwrap();

    Ok(())
}

#[test]
fn gas_limit_too_low() -> anyhow::Result<()> {
    let (mut suite, accounts) = TestBuilder::new_with_vm(WasmVm::new(WASM_CACHE_CAPACITY))
        .add_account("sender", Coins::new_one(DENOM, NonZero::new(100_u128)))?
        .add_account("receiver", Coins::new_empty())?
        .build()?;

    // Make a bank transfer with a small gas limit; should fail.
    // Bank transfers should take around ~1M gas.
    //
    // We can't easily tell whether gas will run out during the Wasm execution
    // (in which case, the error would be a `VmError::GasDepletion`) or during
    // a host function call (in which case, a `VmError::OutOfGas`). We can only
    // say that the error has to be one of the two. Therefore, we simply ensure
    // the error message contains the word "gas".
    suite
        .execute_message_with_gas(&accounts["sender"], 100_000, Message::Transfer {
            to: accounts["receiver"].address.clone(),
            coins: Coins::new_one(DENOM, NonZero::new(10_u128)),
        })?
        .should_fail_with_error("gas")?;

    // Tx is went out of gas.
    // Balances should remain the same
    suite
        .query_balance(&accounts["sender"], DENOM)
        .should_succeed_and_equal(Uint128::new(100))?;
    suite
        .query_balance(&accounts["receiver"], DENOM)
        .should_succeed_and_equal(Uint128::ZERO)?;

    Ok(())
}

#[test]
fn infinite_loop() -> anyhow::Result<()> {
    let (mut suite, accounts) = TestBuilder::new_with_vm(WasmVm::new(WASM_CACHE_CAPACITY))
        .add_account("sender", Coins::new_one(DENOM, NonZero::new(100_u128)))?
        .build()?;

    let (_, tester) = suite.upload_and_instantiate_with_gas(
        &accounts["sender"],
        320_000_000,
        read_wasm_file("grug_tester_infinite_loop.wasm")?,
        "tester/infinite_loop",
        &Empty {},
    )?;

    suite
        .execute_message_with_gas(&accounts["sender"], 1_000_000, Message::Execute {
            contract: tester,
            msg: to_json_value(&Empty {})?,
            funds: Coins::new_empty(),
        })?
        .should_fail_with_error(VmError::GasDepletion)?;

    Ok(())
}

#[test]
fn immutable_state() -> anyhow::Result<()> {
    let (mut suite, accounts) = TestBuilder::new_with_vm(WasmVm::new(WASM_CACHE_CAPACITY))
        .add_account("sender", Coins::new_one(DENOM, NonZero::new(100_u128)))?
        .build()?;

    // Deploy the tester contract
    let (_, tester) = suite.upload_and_instantiate_with_gas(
        &accounts["sender"],
        // Currently, deploying a contract consumes an exceedingly high amount
        // of gas because of the need to allocate hundreds ok kB of contract
        // bytecode into Wasm memory and have the contract deserialize it...
        320_000_000,
        read_wasm_file("grug_tester_immutable_state.wasm")?,
        "tester/immutable_state",
        &Empty {},
    )?;

    // Query the tester contract.
    //
    // During the query, the contract attempts to write to the state by directly
    // calling the `db_write` import.
    //
    // This tests how the VM handles state mutability while serving the `Query`
    // ABCI request.
    suite
        .query_wasm_smart::<_, Empty>(tester.clone(), &Empty {})
        .should_fail_with_error(VmError::ReadOnly)?;

    // Execute the tester contract.
    //
    // During the execution, the contract makes a query to itself and the query
    // tries to write to the storage.
    //
    // This tests how the VM handles state mutability while serving the
    // `FinalizeBlock` ABCI request.
    suite
        .execute_message_with_gas(&accounts["sender"], 1_000_000, Message::Execute {
            contract: tester,
            msg: to_json_value(&Empty {})?,
            funds: Coins::default(),
        })?
        .should_fail_with_error(VmError::ReadOnly)?;

    Ok(())
}
