use {
    grug::{Addr, Coins, ContractBuilder, Empty, Hash256, NonZero, TestBuilder},
    super_smart_querier::{QueryBuzzRequest, QueryFooRequest, QueryFuzzRequest},
};

pub mod super_smart_querier {
    use grug::{
        to_json_value, Addr, Empty, Hash256, ImmutableCtx, Json, MutableCtx, Response, StdResult,
    };

    #[grug::derive(serde)]
    #[derive(grug::QueryRequest)]
    pub enum QueryMsg {
        #[returns(String)]
        Foo { bar: u64 },
        #[returns(Addr)]
        Fuzz(u8),
        #[returns(Hash256)]
        Buzz,
    }

    pub fn instantiate(_ctx: MutableCtx, _msg: Empty) -> StdResult<Response> {
        Ok(Response::new())
    }

    pub fn query(_ctx: ImmutableCtx, msg: QueryMsg) -> StdResult<Json> {
        match msg {
            QueryMsg::Foo { bar } => {
                let bar = bar.to_string();
                to_json_value(&bar)
            },
            QueryMsg::Fuzz(fuzz) => {
                let fuzz = Addr::mock(fuzz);
                to_json_value(&fuzz)
            },
            QueryMsg::Buzz => {
                let buzz = Hash256::from_array([1; 32]);
                to_json_value(&buzz)
            },
        }
    }
}

#[test]
fn query_super_smart() {
    let (mut suite, accounts) = TestBuilder::new()
        .add_account("larry", Coins::one("uusdc", NonZero::new(123_u128)))
        .unwrap()
        .set_chain_id("kebab")
        .set_owner("larry")
        .unwrap()
        .build()
        .unwrap();

    let code = ContractBuilder::new(Box::new(super_smart_querier::instantiate))
        .with_query(Box::new(super_smart_querier::query))
        .build();

    let (_, contract) = suite
        .upload_and_instantiate(
            &accounts["larry"],
            code,
            "contract",
            &Empty {},
            Coins::new(),
        )
        .unwrap();

    // Here, the compiler should be able to infer the type of the response as
    // `String` based on the request type `QueryFooRequest`.
    suite
        .query_wasm_smart(contract, QueryFooRequest { bar: 12345 })
        .should_succeed_and_equal(12345.to_string());

    // Similarly, for unnamed variant `Fuzz`.
    suite
        .query_wasm_smart(contract, QueryFuzzRequest(123))
        .should_succeed_and_equal(Addr::mock(123));

    // Similarly, for unit variant `Buzz`.
    suite
        .query_wasm_smart(contract, QueryBuzzRequest)
        .should_succeed_and_equal(Hash256::from_array([1; 32]));
}
