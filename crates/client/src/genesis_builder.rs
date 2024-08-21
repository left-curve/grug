use {
    crate::AdminOption,
    anyhow::{bail, ensure},
    chrono::{DateTime, SecondsFormat, Utc},
    grug::{
        Addr, Binary, Coins, Config, Defined, Duration, GenesisState, Hash256, HashExt, Json,
        JsonExt, Message, Permission, Permissions, StdError, Undefined, GENESIS_SENDER,
    },
    std::{collections::BTreeMap, fs, path::Path},
};

#[derive(Default)]
pub struct GenesisBuilder<
    O = Undefined<Addr>,
    B = Undefined<Addr>,
    T = Undefined<Addr>,
    U = Undefined<Permission>,
    I = Undefined<Permission>,
> {
    // Consensus parameters.
    // These are generated by CometBFT when running `cometbft init`.
    // If not provided, it means to simply not modify existing values.
    genesis_time: Option<DateTime<Utc>>,
    chain_id: Option<String>,
    // Chain configs
    owner: O,
    bank: B,
    taxman: T,
    cronjobs: BTreeMap<Addr, Duration>,
    upload_permission: U,
    instantiate_permission: I,
    // App configs
    app_configs: BTreeMap<String, Json>,
    // Genesis messages
    upload_msgs: Vec<Message>,
    other_msgs: Vec<Message>,
}

impl GenesisBuilder {
    pub fn new() -> Self {
        GenesisBuilder::default()
    }
}

impl<B, T, U, I> GenesisBuilder<Undefined<Addr>, B, T, U, I> {
    pub fn set_owner(self, owner: Addr) -> GenesisBuilder<Defined<Addr>, B, T, U, I> {
        GenesisBuilder {
            genesis_time: self.genesis_time,
            chain_id: self.chain_id,
            owner: Defined::new(owner),
            bank: self.bank,
            taxman: self.taxman,
            cronjobs: self.cronjobs,
            upload_permission: self.upload_permission,
            instantiate_permission: self.instantiate_permission,
            app_configs: self.app_configs,
            upload_msgs: self.upload_msgs,
            other_msgs: self.other_msgs,
        }
    }
}

impl<O, T, U, I> GenesisBuilder<O, Undefined<Addr>, T, U, I> {
    pub fn set_bank(self, bank: Addr) -> GenesisBuilder<O, Defined<Addr>, T, U, I> {
        GenesisBuilder {
            genesis_time: self.genesis_time,
            chain_id: self.chain_id,
            owner: self.owner,
            bank: Defined::new(bank),
            taxman: self.taxman,
            cronjobs: self.cronjobs,
            upload_permission: self.upload_permission,
            instantiate_permission: self.instantiate_permission,
            app_configs: self.app_configs,
            upload_msgs: self.upload_msgs,
            other_msgs: self.other_msgs,
        }
    }
}

impl<O, B, U, I> GenesisBuilder<O, B, Undefined<Addr>, U, I> {
    pub fn set_taxman(self, taxman: Addr) -> GenesisBuilder<O, B, Defined<Addr>, U, I> {
        GenesisBuilder {
            genesis_time: self.genesis_time,
            chain_id: self.chain_id,
            owner: self.owner,
            bank: self.bank,
            taxman: Defined::new(taxman),
            cronjobs: self.cronjobs,
            upload_permission: self.upload_permission,
            instantiate_permission: self.instantiate_permission,
            app_configs: self.app_configs,
            upload_msgs: self.upload_msgs,
            other_msgs: self.other_msgs,
        }
    }
}

impl<O, B, T, I> GenesisBuilder<O, B, T, Undefined<Permission>, I> {
    pub fn set_upload_permission(
        self,
        upload_permission: Permission,
    ) -> GenesisBuilder<O, B, T, Defined<Permission>, I> {
        GenesisBuilder {
            genesis_time: self.genesis_time,
            chain_id: self.chain_id,
            owner: self.owner,
            bank: self.bank,
            taxman: self.taxman,
            cronjobs: self.cronjobs,
            upload_permission: Defined::new(upload_permission),
            instantiate_permission: self.instantiate_permission,
            app_configs: self.app_configs,
            upload_msgs: self.upload_msgs,
            other_msgs: self.other_msgs,
        }
    }
}

impl<O, B, T, U> GenesisBuilder<O, B, T, U, Undefined<Permission>> {
    pub fn set_instantiate_permission(
        self,
        instantiate_permission: Permission,
    ) -> GenesisBuilder<O, B, T, U, Defined<Permission>> {
        GenesisBuilder {
            genesis_time: self.genesis_time,
            chain_id: self.chain_id,
            owner: self.owner,
            bank: self.bank,
            taxman: self.taxman,
            cronjobs: self.cronjobs,
            upload_permission: self.upload_permission,
            instantiate_permission: Defined::new(instantiate_permission),
            app_configs: self.app_configs,
            upload_msgs: self.upload_msgs,
            other_msgs: self.other_msgs,
        }
    }
}

impl<O, B, T, U, I> GenesisBuilder<O, B, T, U, I> {
    pub fn with_genesis_time<Time>(mut self, genesis_time: Time) -> Self
    where
        Time: Into<DateTime<Utc>>,
    {
        self.genesis_time = Some(genesis_time.into());
        self
    }

    pub fn with_chain_id<Id>(mut self, chain_id: Id) -> Self
    where
        Id: ToString,
    {
        self.chain_id = Some(chain_id.to_string());
        self
    }

    pub fn add_cronjob(mut self, contract: Addr, interval: Duration) -> Self {
        self.cronjobs.insert(contract, interval);
        self
    }

    pub fn add_app_config<K, V>(mut self, key: K, value: &V) -> anyhow::Result<Self>
    where
        K: Into<String>,
        V: JsonExt,
    {
        let key = key.into();
        let value = value.to_json_value()?;

        ensure!(
            !self.app_configs.contains_key(&key),
            "app config key `{key}` is already set"
        );

        self.app_configs.insert(key, value);

        Ok(self)
    }

    pub fn upload<D>(&mut self, code: D) -> anyhow::Result<Hash256>
    where
        D: Into<Binary>,
    {
        let code = code.into();
        let code_hash = code.hash256();

        self.upload_msgs.push(Message::upload(code));

        Ok(code_hash)
    }

    pub fn upload_file<P>(&mut self, path: P) -> anyhow::Result<Hash256>
    where
        P: AsRef<Path>,
    {
        let code = fs::read(path)?;

        self.upload(code)
    }

    pub fn instantiate<M, S, C>(
        &mut self,
        code_hash: Hash256,
        msg: &M,
        salt: S,
        funds: C,
        admin_opt: AdminOption,
    ) -> anyhow::Result<Addr>
    where
        M: JsonExt,
        S: Into<Binary>,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        let salt = salt.into();
        let address = Addr::compute(GENESIS_SENDER, code_hash, &salt);
        let admin = admin_opt.decide(address);

        let msg = Message::instantiate(code_hash, msg, salt, funds, admin)?;
        self.other_msgs.push(msg);

        Ok(address)
    }

    pub fn upload_and_instantiate<D, M, S, C>(
        &mut self,
        code: D,
        msg: &M,
        salt: S,
        funds: C,
        admin_opt: AdminOption,
    ) -> anyhow::Result<Addr>
    where
        D: Into<Binary>,
        M: JsonExt,
        S: Into<Binary>,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        let code_hash = self.upload(code)?;
        self.instantiate(code_hash, msg, salt, funds, admin_opt)
    }

    pub fn upload_file_and_instantiate<P, M, S, C>(
        &mut self,
        path: P,
        msg: &M,
        salt: S,
        funds: C,
        admin_opt: AdminOption,
    ) -> anyhow::Result<Addr>
    where
        P: AsRef<Path>,
        M: JsonExt,
        S: Into<Binary>,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        let code_hash = self.upload_file(path)?;
        self.instantiate(code_hash, msg, salt, funds, admin_opt)
    }

    pub fn execute<M, C>(&mut self, contract: Addr, msg: &M, funds: C) -> anyhow::Result<()>
    where
        M: JsonExt,
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        let msg = Message::execute(contract, msg, funds)?;
        self.other_msgs.push(msg);

        Ok(())
    }
}

impl
    GenesisBuilder<
        Defined<Addr>,
        Defined<Addr>,
        Defined<Addr>,
        Defined<Permission>,
        Defined<Permission>,
    >
{
    pub fn build(self) -> GenesisState {
        let permissions = Permissions {
            upload: self.upload_permission.into_inner(),
            instantiate: self.instantiate_permission.into_inner(),
        };

        let config = Config {
            owner: self.owner.into_inner(),
            bank: self.bank.into_inner(),
            taxman: self.taxman.into_inner(),
            cronjobs: self.cronjobs,
            permissions,
        };

        let mut msgs = self.upload_msgs;
        msgs.extend(self.other_msgs);

        GenesisState {
            config,
            msgs,
            app_configs: self.app_configs,
        }
    }

    pub fn build_and_write_to_cometbft_genesis<P>(self, path: P) -> anyhow::Result<GenesisState>
    where
        P: AsRef<Path>,
    {
        let cometbft_genesis_raw = fs::read(path.as_ref())?;
        let mut cometbft_genesis = Json::from_json_slice(cometbft_genesis_raw)?;

        let Some(obj) = cometbft_genesis.as_object_mut() else {
            bail!("CometBFT genesis file is not a JSON object");
        };

        if let Some(genesis_time) = &self.genesis_time {
            let genesis_time_str = genesis_time.to_rfc3339_opts(SecondsFormat::Nanos, true);
            obj.insert("genesis_time".to_string(), Json::String(genesis_time_str));
        }

        if let Some(chain_id) = &self.chain_id {
            obj.insert("chain_id".to_string(), Json::String(chain_id.clone()));
        }

        let genesis_state = self.build();
        let genesis_state_json = genesis_state.to_json_value()?;

        obj.insert("app_state".to_string(), genesis_state_json);

        let cometbft_genesis_raw = cometbft_genesis.to_json_string_pretty()?;

        fs::write(path, cometbft_genesis_raw)?;

        Ok(genesis_state)
    }
}
