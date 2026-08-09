use {
    dango_backtrace::BacktracedError,
    dango_primitives::{Binary, Empty, Query, QueryResponse},
};

pub type InstantiateMsg = Empty;

#[dango_primitives::derive(Serde, Borsh)]
pub enum ExecuteMsg {
    /// Perform an infinite loop. Test if the VM can properly halt execution
    /// when gas is exhausted.
    InfiniteLoop {},
    /// See `QueryMsg::ForceWrite`.
    ///
    /// This tests that the VM can not only reject a forced write when calling
    /// the `query` export, but also when handling the `query_chain` import call
    /// within an `execute` call.
    ForceWriteOnQuery { key: String, value: String },
    /// The contract attempts to execute itself in a loop.
    ///
    /// Without proper check, this will cause a stack overflow panic, which
    /// halts the chain.
    ///
    /// This is one of two ways a malicious contract can force a stack overflow;
    /// the other is via a query message, also implemented in this contract.
    StackOverflow {},
}

#[dango_primitives::derive(Serde, Borsh, QueryRequest)]
pub enum QueryMsg {
    #[returns(())]
    FailingQuery { msg: String },
    #[returns(BacktraceQueryResponse)]
    Backtrace { query: Query },
    /// Run a loop of the given number of iterations. Within each iteration, a
    /// set of math operations (addition, subtraction, multiplication, division)
    /// are performed.
    ///
    /// This is used for deducing the relation between Wasmer gas metering
    /// points and CPU time (i.e. how many gas points roughly correspond to one
    /// second of run time).
    #[returns(())]
    Loop { iterations: u64 },
    /// Attempt to write a key-value pair to the contract storage.
    ///
    /// If using the Dango library, this is impossible to do, because the
    /// contract is given an `ImmutableCtx` in the `query` function, which
    /// doesn't come with state mutable methods.
    ///
    /// However, a malicious contract can attempt to directly call the `db_write`,
    /// `db_remove`, or `db_remove_range` FFI import methods directly. We need
    /// to test whether the VM can properly reject this behavior.
    #[returns(())]
    ForceWrite { key: String, value: String },
    /// The contract attempts to make queries in a loop.
    ///
    /// Without proper check, this will cause a stack overflow panic, which
    /// halts the chain.
    ///
    /// This is one of two ways a malicious contract can force a stack overflow;
    /// the other is via an execute message, also implemented in this contract.
    #[returns(())]
    StackOverflow {},
    /// Verify a single Secp256r1 signature.
    #[returns(())]
    VerifySecp256r1 {
        // Note: For production contracts, it's better to use fixed-length types,
        // such as:
        // - `ByteArray<33>` for `pk`,
        // - `ByteArray<32>` for `sig`, and
        // - `Hash256` for `msg_hash`
        // However, for the purpose of testing, we need this function to take
        // input data of incorrect lengths.
        pk: Binary,
        sig: Binary,
        msg_hash: Binary,
    },
    /// Verify a single Secp256k1 signature.
    #[returns(())]
    VerifySecp256k1 {
        pk: Binary,
        sig: Binary,
        msg_hash: Binary,
    },
    /// Recover an Secp256k1 publick key from a signature.
    #[returns(Binary)]
    RecoverSecp256k1 {
        sig: Binary,
        msg_hash: Binary,
        recovery_id: u8,
        compressed: bool,
    },
}

#[dango_primitives::derive(Serde)]
pub enum BacktraceQueryResponse {
    Ok(QueryResponse),
    Err(BacktracedError<String>),
}
