use grug::Empty;

pub type InstantiateMsg = Empty;

#[grug::derive(serde)]
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
}

#[grug::derive(serde)]
#[derive(grug::QueryRequest)]
pub enum QueryMsg {
    /// Run a loop of the given number of iterations. Within each iteration, a
    /// set of math operations (addition, subtraction, multiplication, division)
    /// are performed.
    ///
    /// This is used for deducing the relation between Wasmer gas metering
    /// points and CPU time (i.e. how many gas points roughly correspond to one
    /// second of run time).
    #[returns(Empty)]
    Loop { iterations: u64 },
    /// Attempt to write a key-value pair to the contract storage.
    ///
    /// If using the Grug library, this is impossible to do, because the
    /// contract is given an `ImmutableCtx` in the `query` function, which
    /// doesn't come with state mutable methods.
    ///
    /// However, a malicious contract can attempt to directly call the `db_write`,
    /// `db_remove`, or `db_remove_range` FFI import methods directly. We need
    /// to test whether the VM can properly reject this behavior.
    #[returns(Empty)]
    ForceWrite { key: String, value: String },

    // #[returns(Empty)]
    // Hello(String),
}

// #[test]
// fn asd() {
//     use grug::AlternativeQuery;
//     let asd = QueryMsg::ForceWrite { key: "Some".to_string(), value: "None".to_string() };

//     let b = asd.into_alternative();

//     match b {
        
//     }

// }

