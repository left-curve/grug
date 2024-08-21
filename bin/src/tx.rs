use {
    crate::prompt::{confirm, print_json_pretty, read_password},
    anyhow::anyhow,
    clap::{Parser, Subcommand},
    colored::Colorize,
    grug_app::GAS_COSTS,
    grug_client::{Client, GasOption, SigningKey, SigningOption},
    grug_types::{from_json_str, Addr, Binary, Coins, Hash256, Message, UnsignedTx},
    serde::Serialize,
    std::{fs::File, io::Read, path::PathBuf, str::FromStr},
    tendermint_rpc::endpoint::broadcast::tx_sync,
};

#[derive(Parser)]
pub struct TxCmd {
    /// Tendermint RPC address
    #[arg(long, global = true, default_value = "http://127.0.0.1:26657")]
    node: String,

    /// Name of the key to sign transactions
    #[arg(long, global = true)]
    key: Option<String>,

    /// Transaction sender address
    #[arg(long, global = true)]
    sender: Option<Addr>,

    /// Chain identifier [default: query from chain]
    #[arg(long, global = true)]
    chain_id: Option<String>,

    /// Account sequence number [default: query from chain]
    #[arg(long, global = true)]
    sequence: Option<u32>,

    /// Amount of gas units to request
    #[arg(long, global = true)]
    gas_limit: Option<u64>,

    /// Scaling factor to apply to simulated gas consumption
    #[arg(long, global = true, default_value_t = 1.4)]
    gas_adjustment: f64,

    /// Simulate gas usage without submitting the transaction to mempool.
    #[arg(long, global = true)]
    simulate: bool,

    #[command(subcommand)]
    subcmd: SubCmd,
}

#[derive(Subcommand)]
enum SubCmd {
    /// Update the chain-level configurations
    Configure {
        /// Updates to the chain configuration
        updates: String,
        /// Updates to the app configuration
        app_updates: String,
    },
    /// Send coins to the given recipient address
    Transfer {
        /// Recipient address
        to: Addr,
        /// Coins to send in the format: {denom1}:{amount},{denom2}:{amount},...
        coins: String,
    },
    /// Update a Wasm binary code
    Upload {
        /// Path to the Wasm file
        path: PathBuf,
    },
    /// Instantiate a new contract
    Instantiate {
        /// Hash of the Wasm byte code to be associated with the contract
        code_hash: Hash256,
        /// Instantiate message as a JSON string
        msg: String,
        /// Salt in UTF-8 encoding
        salt: String,
        /// Coins to be sent to the contract, in the format: {denom1}:{amount},{denom2}:{amount},...
        #[arg(long)]
        funds: Option<String>,
        /// Administrator address for the contract
        #[arg(long)]
        admin: Option<Addr>,
    },
    /// Execute a contract
    Execute {
        /// Contract address
        contract: Addr,
        /// Execute message as a JSON string
        msg: String,
        /// Coins to be sent to the contract, in the format: {denom1}:{amount},{denom2}:{amount},...
        #[arg(long)]
        funds: Option<String>,
    },
    /// Update the code hash associated with a contract
    Migrate {
        /// Contract address
        contract: Addr,
        /// New code hash
        new_code_hash: Hash256,
        /// Migrate message as a JSON string
        msg: String,
    },
}

impl TxCmd {
    pub async fn run(self, key_dir: PathBuf) -> anyhow::Result<()> {
        let sender = self.sender.ok_or(anyhow!("sender not specified"))?;
        let key_name = self.key.ok_or(anyhow!("key name not specified"))?;

        // Compose the message
        let msg = match self.subcmd {
            SubCmd::Configure {
                updates,
                app_updates,
            } => {
                let updates = from_json_str(updates)?;
                let app_updates = from_json_str(app_updates)?;
                Message::Configure {
                    updates,
                    app_updates,
                }
            },
            SubCmd::Transfer { to, coins } => {
                let coins = Coins::from_str(&coins)?;
                Message::Transfer { to, coins }
            },
            SubCmd::Upload { path } => {
                let mut file = File::open(path)?;
                let mut code = vec![];
                file.read_to_end(&mut code)?;
                Message::Upload { code: code.into() }
            },
            SubCmd::Instantiate {
                code_hash,
                msg,
                salt,
                funds,
                admin,
            } => Message::Instantiate {
                msg: msg.into_bytes().into(),
                salt: salt.into_bytes().into(),
                funds: Coins::from_str(&funds.unwrap_or_default())?,
                code_hash,
                admin,
            },
            SubCmd::Execute {
                contract,
                msg,
                funds,
            } => Message::Execute {
                msg: msg.into_bytes().into(),
                funds: Coins::from_str(funds.as_deref().unwrap_or(Coins::EMPTY_COINS_STR))?,
                contract,
            },
            SubCmd::Migrate {
                contract,
                new_code_hash,
                msg,
            } => Message::Migrate {
                msg: msg.into_bytes().into(),
                new_code_hash,
                contract,
            },
        };

        let client = Client::connect(&self.node)?;

        if self.simulate {
            let unsigned_tx = UnsignedTx {
                sender,
                msgs: vec![msg],
            };
            let outcome = client.simulate(&unsigned_tx).await?;
            print_json_pretty(outcome)?;
        } else {
            // Load signing key
            let key_path = key_dir.join(format!("{key_name}.json"));
            let password = read_password("🔑 Enter a password to encrypt the key".bold())?;
            let signing_key = SigningKey::from_file(&key_path, &password)?;
            let sign_opt = SigningOption {
                signing_key: &signing_key,
                sender,
                chain_id: self.chain_id,
                sequence: self.sequence,
            };

            // Choose gas option
            let gas_opt = if let Some(gas_limit) = self.gas_limit {
                GasOption::Predefined { gas_limit }
            } else {
                GasOption::Simulate {
                    scale: self.gas_adjustment,
                    // We always increase the simulated gas consumption by this
                    // amount, since signature verification is skipped during
                    // simulation.
                    flat_increase: GAS_COSTS.secp256k1_verify,
                }
            };

            // Broadcast transaction
            let maybe_res = client
                .send_message_with_confirmation(msg, gas_opt, sign_opt, |tx| {
                    print_json_pretty(tx)?;
                    Ok(confirm("🤔 Broadcast transaction?".bold())?)
                })
                .await?;

            // Print result
            if let Some(res) = maybe_res {
                print_json_pretty(PrintableBroadcastResponse::from(res))?;
            } else {
                println!("🤷 User aborted");
            }
        }

        Ok(())
    }
}

/// Similar to tendermint_rpc Response but serializes to nicer JSON.
#[derive(Serialize)]
struct PrintableBroadcastResponse {
    code: u32,
    data: Binary,
    log: String,
    hash: String,
}

impl From<tx_sync::Response> for PrintableBroadcastResponse {
    fn from(broadcast_res: tx_sync::Response) -> Self {
        Self {
            code: broadcast_res.code.into(),
            data: broadcast_res.data.to_vec().into(),
            log: broadcast_res.log,
            hash: broadcast_res.hash.to_string(),
        }
    }
}
