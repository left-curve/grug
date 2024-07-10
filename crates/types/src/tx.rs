use {
    crate::{to_json_value, Addr, Binary, Coins, Config, Hash, Json, StdError, StdResult},
    serde::{Deserialize, Serialize},
    serde_with::skip_serializing_none,
};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Tx {
    pub sender: Addr,
    pub msgs: Vec<Message>,
    pub data: Json,
    pub credential: Binary,
    pub gas_limit: u64,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Message {
    /// Update the chain-level configurations.
    ///
    /// Only the `owner` is authorized to do this. If the owner is set to `None`,
    /// no one can update the config.
    Configure { new_cfg: Config },
    /// Send coins to the given recipient address.
    Transfer { to: Addr, coins: Coins },
    /// Upload a Wasm binary code and store it in the chain's state.
    Upload { code: Binary },
    /// Register a new account.
    Instantiate {
        code_hash: Hash,
        msg: Json,
        salt: Binary,
        funds: Coins,
        admin: Option<Addr>,
    },
    /// Execute a contract.
    Execute {
        contract: Addr,
        msg: Json,
        funds: Coins,
    },
    /// Update the `code_hash` associated with a contract.
    ///
    /// Only the contract's `admin` is authorized to do this. If the admin is
    /// set to `None`, no one can update the code hash.
    Migrate {
        contract: Addr,
        new_code_hash: Hash,
        msg: Json,
    },
}

impl Message {
    pub fn configure(new_cfg: Config) -> Self {
        Self::Configure { new_cfg }
    }

    pub fn transfer<C>(to: Addr, coins: C) -> StdResult<Self>
    where
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        Ok(Self::Transfer {
            to,
            coins: coins.try_into()?,
        })
    }

    pub fn upload<B>(code: B) -> Self
    where
        B: Into<Binary>,
    {
        Self::Upload { code: code.into() }
    }

    pub fn instantiate<M, S, C>(
        code_hash: Hash,
        msg: &M,
        salt: S,
        funds: C,
        admin: Option<Addr>,
    ) -> StdResult<Self>
    where
        M: Serialize,
        S: Into<Binary>,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        Ok(Self::Instantiate {
            code_hash,
            msg: to_json_value(msg)?,
            salt: salt.into(),
            funds: funds.try_into()?,
            admin,
        })
    }

    pub fn execute<M, C>(contract: Addr, msg: &M, funds: C) -> StdResult<Self>
    where
        M: Serialize,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        Ok(Self::Execute {
            contract,
            msg: to_json_value(msg)?,
            funds: funds.try_into()?,
        })
    }

    pub fn migrate<M>(contract: Addr, new_code_hash: Hash, msg: &M) -> StdResult<Self>
    where
        M: Serialize,
    {
        Ok(Self::Migrate {
            contract,
            new_code_hash,
            msg: to_json_value(msg)?,
        })
    }
}

/// Builder for field `data` in [`Tx`].
#[derive(Default)]
pub struct DataBuilder {
    json: serde_json::Map<String, Json>,
}

impl DataBuilder {
    pub fn add_field<T: Serialize>(mut self, key: impl Into<String>, value: T) -> StdResult<Self> {
        self.json.insert(key.into(), to_json_value(&value)?);
        Ok(self)
    }

    pub fn add_field_raw(mut self, key: impl Into<String>, value: Json) -> Self {
        self.json.insert(key.into(), value);
        self
    }

    pub fn finalize(self) -> Json {
        Json::Object(self.json)
    }
}
