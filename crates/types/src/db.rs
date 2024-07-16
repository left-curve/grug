use {
    crate::{StdError, StdResult},
    serde::{Deserialize, Serialize},
    std::collections::BTreeMap,
};

/// A shorthand for an owned KV pair.
pub type Record = (Vec<u8>, Vec<u8>);

/// A batch of Db ops, ready to be committed.
/// For RocksDB, this is similar to rocksdb::WriteBatch.
pub type Batch<K = Vec<u8>, V = Vec<u8>> = BTreeMap<K, Op<V>>;

/// Represents a database operation, either inserting a value or deleting one.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Op<V = Vec<u8>> {
    Insert(V),
    Delete,
}

impl<V> Op<V> {
    // similar to Option::as_ref
    pub fn as_ref(&self) -> Op<&V> {
        match self {
            Op::Insert(v) => Op::Insert(v),
            Op::Delete => Op::Delete,
        }
    }

    // similar to Option::map
    pub fn map<T>(self, f: fn(V) -> T) -> Op<T> {
        match self {
            Op::Insert(v) => Op::Insert(f(v)),
            Op::Delete => Op::Delete,
        }
    }

    pub fn into_option(self) -> Option<V> {
        match self {
            Op::Insert(v) => Some(v),
            Op::Delete => None,
        }
    }
}

/// Describing iteration order.
#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    Ascending = 1,
    Descending = 2,
}

// we need to convert Order into a primitive type such as i32 so that it can be
// passed over FFI
impl From<Order> for i32 {
    fn from(order: Order) -> Self {
        order as _
    }
}

impl TryFrom<i32> for Order {
    type Error = StdError;

    fn try_from(value: i32) -> StdResult<Self> {
        match value {
            1 => Ok(Order::Ascending),
            2 => Ok(Order::Descending),
            _ => {
                let reason = format!("must be 1 (asc) or 2 (desc), found {value}");
                Err(StdError::deserialize::<Self>(reason))
            },
        }
    }
}
