#[cfg(feature = "async-graphql")]
use async_graphql::{InputValueError, InputValueResult, Scalar, ScalarType, Value};
use {
    crate::{
        Addr, Binary, Coins, Config, Hash256, HashExt, Json, JsonSerExt, LengthBounded, MaxLength,
        NextUpgrade, NonEmpty, StdError, StdResult, btree_map,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    serde::{Deserialize, Serialize},
    serde_with::skip_serializing_none,
    std::collections::BTreeMap,
};

/// An arbitrary binary data used for deriving address when instantiating a
/// contract.
///
/// Must be no more than 82 bytes.
pub type Salt = MaxLength<Binary, 82>;

/// A human-readable string describing a contract. Can be optionally provided
/// during instantiation.
///
/// Must be non-empty and no more than 128 bytes.
pub type Label = LengthBounded<String, 1, 128>;

/// A transaction.
#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct Tx {
    pub sender: Addr,
    pub gas_limit: u64,
    pub msgs: NonEmpty<Vec<Message>>,
    pub data: Json,
    pub credential: Json,
}

impl Tx {
    pub fn tx_hash(&self) -> StdResult<Hash256> {
        Ok(self.to_json_vec()?.sha2_256())
    }
}

// NOTE: implementing `InputType` doesn't work for complex enums, `Message` in this case
/// A transaction that can be sent to the chain.
///
/// Contains:
/// - Sender address
/// - List of messages to execute
/// - Gas limit
/// - Cryptographic signature
///
/// See [Tx](https://github.com/left-curve/left-curve/blob/main/dango/core/types/src/tx.rs).
#[cfg(feature = "async-graphql")]
#[Scalar(name = "Tx")]
impl ScalarType for Tx {
    fn parse(value: Value) -> InputValueResult<Self> {
        value
            .into_json()
            .and_then(serde_json::from_value)
            .map_err(|err| {
                InputValueError::custom(format!("failed to parse `Tx` from GraphQL value: {err}"))
            })
    }

    fn to_value(&self) -> Value {
        serde_json::to_value(self)
            .and_then(serde_json::from_value)
            .map(Value::Object)
            .unwrap_or(Value::Null)
    }
}

/// A transaction but without a gas limit or credential.
///
/// This is for using in gas simulation.
#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct UnsignedTx {
    pub sender: Addr,
    pub msgs: NonEmpty<Vec<Message>>,
    pub data: Json,
}

// NOTE: implementing `InputType` doesn't work for complex enums, `Message` in this case
/// A transaction to simulate.
///
/// Contains:
/// - Sender address
/// - List of messages to execute
///
/// See [UnsignedTx](https://github.com/left-curve/left-curve/blob/main/dango/core/types/src/tx.rs).
#[cfg(feature = "async-graphql")]
#[Scalar(name = "UnsignedTx")]
impl ScalarType for UnsignedTx {
    fn parse(value: Value) -> InputValueResult<Self> {
        value
            .into_json()
            .and_then(serde_json::from_value)
            .map_err(|err| {
                InputValueError::custom(format!(
                    "failed to parse `UnsignedTx` from GraphQL value: {err}"
                ))
            })
    }

    fn to_value(&self) -> Value {
        serde_json::to_value(self)
            .and_then(serde_json::from_value)
            .map(Value::Object)
            .unwrap_or(Value::Null)
    }
}

/// A message.
#[skip_serializing_none]
#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Message {
    /// Update the chain- and app-level configurations.
    Configure(MsgConfigure),
    /// Schedule a chain upgrade at a future block.
    Upgrade(MsgUpgrade),
    /// Send coins to the given recipient address.
    Transfer(MsgTransfer),
    /// Upload a Wasm binary code and store it in the chain's state.
    Upload(MsgUpload),
    /// Instantiate a new contract.
    Instantiate(MsgInstantiate),
    /// Execute a contract.
    Execute(MsgExecute),
    /// Update the code hash associated with a contract.
    Migrate(MsgMigrate),
}

impl Message {
    pub fn configure<T>(new_cfg: Option<Config>, new_app_cfg: Option<T>) -> StdResult<Self>
    where
        T: Serialize,
    {
        Ok(MsgConfigure {
            new_cfg: new_cfg.map(|cfg| cfg.to_json_value()).transpose()?,
            new_app_cfg: new_app_cfg.map(|t| t.to_json_value()).transpose()?,
        }
        .into())
    }

    pub fn upgrade<T, U, V>(
        height: u64,
        cargo_version: T,
        git_tag: Option<U>,
        url: Option<V>,
    ) -> Self
    where
        T: Into<String>,
        U: Into<String>,
        V: Into<String>,
    {
        MsgUpgrade {
            height,
            cargo_version: cargo_version.into(),
            git_tag: git_tag.map(Into::into),
            url: url.map(Into::into),
        }
        .into()
    }

    pub fn transfer<C>(to: Addr, coins: C) -> StdResult<Self>
    where
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        Ok(Message::Transfer(btree_map! { to => coins.try_into()? }))
    }

    pub fn batch_transfer<I, C>(transfers: I) -> StdResult<Self>
    where
        I: IntoIterator<Item = (Addr, C)>,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        transfers
            .into_iter()
            .map(|(to, coins)| {
                let coins = coins.try_into()?;
                Ok((to, coins))
            })
            .collect::<StdResult<_>>()
            .map(Message::Transfer)
    }

    pub fn upload<B>(code: B) -> Self
    where
        B: Into<Binary>,
    {
        MsgUpload { code: code.into() }.into()
    }

    pub fn instantiate<M, S, C, L>(
        code_hash: Hash256,
        msg: &M,
        salt: S,
        label: Option<L>,
        admin: Option<Addr>,
        funds: C,
    ) -> StdResult<Self>
    where
        M: Serialize,
        S: Into<Binary>,
        C: TryInto<Coins>,
        L: Into<String>,
        StdError: From<C::Error>,
    {
        Ok(MsgInstantiate {
            code_hash,
            msg: msg.to_json_value()?,
            salt: Salt::new(salt.into())?,
            label: label.map(|l| Label::new(l.into())).transpose()?,
            admin,
            funds: funds.try_into()?,
        }
        .into())
    }

    pub fn execute<M, C>(contract: Addr, msg: &M, funds: C) -> StdResult<Self>
    where
        M: Serialize,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        Ok(MsgExecute {
            contract,
            msg: msg.to_json_value()?,
            funds: funds.try_into()?,
        }
        .into())
    }

    pub fn migrate<M>(contract: Addr, new_code_hash: Hash256, msg: &M) -> StdResult<Self>
    where
        M: Serialize,
    {
        Ok(MsgMigrate {
            contract,
            new_code_hash,
            msg: msg.to_json_value()?,
        }
        .into())
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct MsgConfigure {
    /// The new chain-level config, as a raw JSON value. Parsed into a
    /// [`Config`] at execution time.
    ///
    /// This field is deliberately _not_ typed as `Config`. The `Config` schema
    /// evolves over time (e.g. commit `c5cdf66c9` replaced the `taxman` field
    /// with the gas fields), while `MsgConfigure`s live forever inside
    /// serialized historical blocks — in the indexer's cached block files and
    /// in tx JSONs. If this field were typed, every `Config` schema change
    /// would make each historical block that contains a config update
    /// impossible to deserialize. As a raw JSON value, such blocks survive
    /// schema changes, and additionally preserve the config update's original
    /// content faithfully.
    pub new_cfg: Option<Json>,

    pub new_app_cfg: Option<Json>,
}

pub type MsgUpgrade = NextUpgrade;

// recipient => coins
pub type MsgTransfer = BTreeMap<Addr, Coins>;

#[skip_serializing_none]
#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct MsgUpload {
    pub code: Binary,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct MsgInstantiate {
    pub code_hash: Hash256,
    pub msg: Json,
    pub salt: Salt,
    pub label: Option<Label>,
    pub admin: Option<Addr>,
    pub funds: Coins,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct MsgExecute {
    pub contract: Addr,
    pub msg: Json,
    pub funds: Coins,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct MsgMigrate {
    pub contract: Addr,
    pub new_code_hash: Hash256,
    pub msg: Json,
}

macro_rules! impl_into_message {
    ($variant:ident, $msg:ty) => {
        impl From<$msg> for Message {
            #[inline]
            fn from(msg: $msg) -> Self {
                Self::$variant(msg)
            }
        }
    };
    ($($variant:ident => $msg:ty),+ $(,)?) => {
        $(
            impl_into_message!($variant, $msg);
        )+
    };
}

impl_into_message! {
    Configure   => MsgConfigure,
    Upgrade     => MsgUpgrade,
    Transfer    => MsgTransfer,
    Upload      => MsgUpload,
    Instantiate => MsgInstantiate,
    Execute     => MsgExecute,
    Migrate     => MsgMigrate,
}
