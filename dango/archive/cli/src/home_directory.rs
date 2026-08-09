use {
    anyhow::anyhow,
    home::home_dir,
    std::{
        ops::Deref,
        path::{Path, PathBuf},
    },
};

/// Default home directory, relative to the user's home (`~`).
const DEFAULT_APP_DIR: &str = ".archive";

/// The indexer's home directory. Its config and local state (the block-store
/// RocksDB) live under here; projection cursors live in Postgres, not on disk.
/// Resolved from the `--home` flag or, absent it, `~/.archive`.
///
/// Derefs to the underlying [`PathBuf`]; the typed accessors (config file, data
/// dir, …) are added as the `start` wiring needs them.
pub struct HomeDirectory {
    home: PathBuf,
}

impl HomeDirectory {
    pub fn new_or_default(maybe_home: Option<PathBuf>) -> anyhow::Result<Self> {
        let home = match maybe_home {
            Some(home) => home,
            None => home_dir()
                .ok_or_else(|| anyhow!("failed to find the user home directory"))?
                .join(DEFAULT_APP_DIR),
        };
        Ok(Self { home })
    }

    /// The server configuration file, `<home>/config/app.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.home.join("config").join("app.toml")
    }

    /// Resolve a configured path: returned as-is if absolute, otherwise joined
    /// onto the home directory — so `store_path = "data/blocks"` lands under
    /// `--home` while an absolute path is honored verbatim.
    pub fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.home.join(path)
        }
    }
}

impl Deref for HomeDirectory {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.home
    }
}
