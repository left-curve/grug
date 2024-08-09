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
pub enum QueryMsg {
    /// Run a loop of the given number of iterations. Within each iteration, a
    /// set of math operations (addition, subtraction, multiplication, division)
    /// are performed.
    ///
    /// This is used for deducing the relation between Wasmer gas metering
    /// points and CPU time (i.e. how many gas points roughly correspond to one
    /// second of run time).
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
    ForceWrite { key: String, value: String },

    CryptoVerify {
        ty: CryptoVerifyType,
        pk: Vec<u8>,
        sig: Vec<u8>,
        msg_hash: Vec<u8>,
    },
    RecoverSepc256k1 {
        sig: Vec<u8>,
        msg_hash: Vec<u8>,
        recovery_id: u8,
        compressed: bool,
    },

    Ed25519BatchVerify {
        prehash_msgs: Vec<Vec<u8>>,
        sigs: Vec<Vec<u8>>,
        pks: Vec<Vec<u8>>,
    },
}

#[grug::derive(serde)]
pub enum CryptoVerifyType {
    Ed25519,
    Secp256k1,
    Secp256r1,
}
