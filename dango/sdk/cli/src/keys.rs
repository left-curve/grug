use {
    crate::prompt::{confirm, print_json_pretty, read_password, read_text},
    anyhow::{anyhow, ensure},
    bip32::{Language, Mnemonic},
    clap::Subcommand,
    colored::Colorize,
    dango_primitives::JsonDeExt,
    dango_sdk::{Keystore, Secp256k1, Secret},
    std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
    },
};

/// Use the the BIP-44 coin type of Ethereum:
/// <https://github.com/satoshilabs/slips/blob/master/slip-0044.md>
const DEFAULT_COIN_TYPE: usize = 60;

#[derive(Subcommand)]
pub enum KeysCmd {
    /// Create a new or recover an existing Secp256k1 private key
    Add {
        /// A human-readable name for the key
        name: String,
        /// Recover an existing seed phrase instead of generating a new one
        #[arg(long)]
        recover: bool,
        /// BIP-44 coin type for key derivation
        #[arg(long, default_value_t = DEFAULT_COIN_TYPE)]
        coin_type: usize,
    },
    /// Delete a key by name
    #[command(alias = "rm")]
    Delete {
        /// Name of the key to delete
        name: String,
    },
    /// Display details of a key by name
    Show {
        /// Name of the key to display
        name: String,
    },
    /// List all keys
    #[command(alias = "ls")]
    List,
}

impl KeysCmd {
    pub fn run(self, dir: PathBuf) -> anyhow::Result<()> {
        if dir.exists() {
            ensure!(dir.is_dir(), "path {dir:?} exists but is not a directory");
        } else {
            fs::create_dir_all(&dir)?;
        }

        match self {
            KeysCmd::Add {
                name,
                recover,
                coin_type,
            } => add(&dir, &name, recover, coin_type),
            KeysCmd::Delete { name } => delete(&dir, &name),
            KeysCmd::Show { name } => show(&dir, &name),
            KeysCmd::List => list(&dir),
        }
    }
}

fn add(dir: &Path, name: &str, recover: bool, coin_type: usize) -> anyhow::Result<()> {
    let filename = dir.join(format!("{name}.json"));
    ensure!(!filename.exists(), "file `{filename:?}` already exists");

    // Generate or recover mnemonic phrase.
    let mnemonic = if recover {
        let phrase = read_text("🔑 Enter your BIP-39 mnemonic".bold())?;
        Mnemonic::new(phrase, Language::English)?
    } else {
        // Generate the entropy ourselves instead of using bip32's `random`
        // constructor, which requires RNGs of the outdated rand_core version
        // pinned by bip32.
        let mut entropy = [0u8; 32];
        getrandom::fill(&mut entropy)?;
        Mnemonic::from_entropy(entropy, Language::English)
    };

    // Ask for password and save encrypted keystore.
    let password = read_password(
        format!("🔑 Enter a password to encrypt the keystore `{filename:?}`").bold(),
    )?;
    let sk = Secp256k1::from_mnemonic(&mnemonic, coin_type)?;
    let keystore = Keystore::write_to_file(&sk, &filename, password)?;

    println!();
    print_json_pretty(keystore)?;

    if !recover {
        println!(
            "\n{} write this mnemonic phrase in a safe place!",
            "Important:".bold()
        );
        println!("It is the only way to recover your account if you ever forget your password.");
        println!("\n{}", mnemonic.phrase());
    }

    Ok(())
}

fn delete(dir: &Path, name: &str) -> anyhow::Result<()> {
    let filename = dir.join(format!("{name}.json"));
    ensure!(filename.exists(), "file {filename:?} not found");

    if confirm(format!("🚨 Confirm deleting file {filename:?}").bold())? {
        fs::remove_file(filename)?;
        println!("🗑️  Deleted!");
    }

    Ok(())
}

fn show(dir: &Path, name: &str) -> anyhow::Result<()> {
    let filename = dir.join(format!("{name}.json"));
    ensure!(filename.exists(), "file {filename:?} not found");

    let keystore_str = fs::read_to_string(filename)?;
    let keystore: Keystore = keystore_str.deserialize_json()?;

    print_json_pretty(keystore)
}

fn list(dir: &Path) -> anyhow::Result<()> {
    let mut keystores = BTreeMap::new();
    for entry in dir.read_dir()? {
        let entry = entry?;
        let name = entry
            .path()
            .file_stem()
            .ok_or(anyhow!("failed to get filename of keystore file"))?
            .to_str()
            .ok_or(anyhow!("failed to convert keystore file path to string"))?
            .to_owned();
        let keystore_str = fs::read_to_string(entry.path())?;
        let keystore: Keystore = keystore_str.deserialize_json()?;
        keystores.insert(name, keystore);
    }

    print_json_pretty(keystores)
}
