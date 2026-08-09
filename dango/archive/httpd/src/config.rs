use serde::Deserialize;

/// Configuration for the archive's HTTP front door.
///
/// Parsed from the indexer's TOML config. Intentionally minimal for now; it
/// will grow CORS origins and per-route limits as those land.
#[derive(Clone, Debug, Deserialize)]
pub struct HttpdConfig {
    /// Socket address to bind, e.g. `"0.0.0.0:8080"`.
    pub bind: String,
}

impl Default for HttpdConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8080".to_string(),
        }
    }
}
