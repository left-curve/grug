use grug::{ImmutableCtx, StdResult};

#[derive(grug::QueryRequest)]
pub enum QueryMsg {
    #[returns(String)]
    Named { field1: i32, field2: String },
    #[returns(String)]
    Unnamed(i32),
    #[returns(u64)]
    Buzz,
}

// This macro create the entry_point query.
// Based on QueryMsg, for each variant we have a function that will be called.
// The macro is able at compilation time to check if
// - The Request implement QueryRequest
// - If the function signature is correct
// - If the return type of the function match the #[returns()] attribute
grug::query_entry_point!( QueryMsg,
    Named(QueryNamedRequest) => query_named,
    Unnamed(QueryUnnamedRequest) => query_unnamed,
    Buzz(QueryBuzzRequest) => query_buzz
);

fn query_named(_ctx: ImmutableCtx, _request: QueryNamedRequest) -> StdResult<String> {
    todo!()
}

fn query_unnamed(_ctx: ImmutableCtx, _request: QueryUnnamedRequest) -> StdResult<String> {
    todo!()
}

fn query_buzz(_ctx: ImmutableCtx, _request: QueryBuzzRequest) -> StdResult<u64> {
    todo!()
}
