//! The archive's configuration.
//!
//! Parsed from `<home>/config/app.toml` by
//! [`dango_config_parser::parse_config`], which layers **environment
//! overrides** on top: any field is settable via `SECTION__FIELD`
//! (`__`-separated, e.g. `POSTGRES__URL`, `BLOCK_SOURCE__LIVE_URL`), mirroring
//! `dango-cli`. So a deployment can keep secrets (the Postgres URL) out of the
//! committed TOML and inject them from the environment.
//!
//! Only the **deployment-specific** choices live here. What projections run and
//! the block-source tuning knobs (buffer sizes, intervals, timeouts) are
//! compiled in — see `start`.

use {
    dango_archive_projection::{EventType, WhiteOrBlackList},
    serde::{Deserialize, Deserializer, de},
    std::path::PathBuf,
};

/// Top-level config. `postgres` and `block_source` are required; `log_level`
/// defaults to `info` and `log_format` to `json`; `httpd` defaults to an
/// enabled server on `0.0.0.0:8080`; `activity` to the projection's built-in
/// filters.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Tracing filter directive, e.g. `info`, `debug`, `warn,dango=debug`.
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Log output format. Default `json`.
    #[serde(default)]
    pub log_format: LogFormat,
    pub postgres: PostgresConfig,
    pub block_source: BlockSourceConfig,
    #[serde(default)]
    pub httpd: HttpdConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub activity: ActivitySettings,
}

/// How log events are written to stdout.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// One JSON object per event — the default, suited to log aggregators.
    #[default]
    Json,
    /// Human-readable plain text.
    Plain,
}

/// Optional overrides for the activity projection's **write-time** filters
/// (changing them is not retroactive — see the projection's `DESIGN.md`). Each
/// omitted filter falls back to the projection's built-in default;
/// `involvement_blacklist` is *merged* with the system contracts the cli reads
/// from the node's `app_config`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ActivitySettings {
    /// Extra addresses (hex `0x…`) excluded from participation, merged with the
    /// node's `app_config` system contracts.
    pub involvement_blacklist: Vec<String>,
    /// Which event types are kept at all. Default: blacklist the system noise.
    pub event_type_filter: Option<WhiteOrBlackList<EventType>>,
    /// Which event types' payload is stored. Default: whitelist priority types.
    pub event_data_filter: Option<WhiteOrBlackList<EventType>>,
    /// Which event types fan out by participant. Default: whitelist priority.
    pub involvement_filter: Option<WhiteOrBlackList<EventType>>,
}

/// Postgres connection for the committer (the projection cursors + the
/// projections' own tables); the same pool backs the read API's table queries.
/// Named explicitly `postgres` (not `database`) so a future `[clickhouse]`
/// section can sit beside it unambiguously once ClickHouse-backed projections
/// land.
#[derive(Debug, Clone, Deserialize)]
pub struct PostgresConfig {
    /// libpq URL, e.g. `postgres://user@host/db`. Secret — prefer injecting it
    /// via `POSTGRES__URL` rather than committing it.
    pub url: String,
    /// Connection-pool size. Default 10.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

/// Which block source to run, selected by `type`. The rest of the app is
/// agnostic to the choice — it only ever sees an `Arc<dyn BlockSource>`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockSourceConfig {
    /// Detached host: bounded backfill from a sentinel node, live tail from a
    /// node's `full_block` stream, persisted in a local RocksDB store.
    Remote(RemoteSourceConfig),
    /// Co-located with a dango node: reads its indexer cache and httpd.
    Local(LocalSourceConfig),
}

impl BlockSourceConfig {
    /// A short label for logs — never the URLs / paths.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Remote(_) => "remote",
            Self::Local(_) => "local",
        }
    }

    /// The backfill fetcher kind for a remote source (for logs); `None` for a
    /// local source, which has no fetcher.
    pub fn fetcher_kind(&self) -> Option<&'static str> {
        match self {
            Self::Remote(remote) => Some(remote.fetcher.kind()),
            Self::Local(_) => None,
        }
    }

    /// The node httpd base URL — `live_url` (remote) or `node_url` (local).
    /// The cli queries this node's `app_config` for the system-contract
    /// blacklist.
    pub fn node_url(&self) -> &str {
        match self {
            Self::Remote(remote) => &remote.live_url,
            Self::Local(local) => &local.node_url,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteSourceConfig {
    /// RocksDB directory for the persisted block store. A relative path resolves
    /// against `--home`.
    pub store_path: PathBuf,
    /// Base URL of the node httpd for the live `full_block` tail. The `sentinel`
    /// fetcher reuses this same URL for backfill (same node), so it carries no
    /// URL of its own; a different fetcher kind (e.g. `s3`) brings its own.
    pub live_url: String,
    /// Which backfill fetcher to use. Defaults to `sentinel`.
    #[serde(default)]
    pub fetcher: FetcherConfig,
}

/// Backfill fetcher for a remote source, selected by `type`. Only `sentinel`
/// exists today; an S3 archive fetcher is planned, hence the tagged enum.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FetcherConfig {
    /// Bounded backfill from a sentinel node's `/block/full/range`. Reuses the
    /// source's `live_url` (same node as the live tail), so it has no URL field.
    Sentinel {
        /// Concurrent range calls per backfill batch; omitted → the
        /// block-source crate's default. Overridable per instance as
        /// `BLOCK_SOURCE__FETCHER__PARALLELISM`.
        #[serde(default, deserialize_with = "de_opt_usize")]
        parallelism: Option<usize>,
    },
    // Future: `S3(S3FetcherConfig)` — backfill from an archive bucket with its
    // own bucket / region / prefix; the live tail still uses `live_url`.
}

impl Default for FetcherConfig {
    fn default() -> Self {
        Self::Sentinel { parallelism: None }
    }
}

impl FetcherConfig {
    /// A short label for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Sentinel { .. } => "sentinel",
        }
    }
}

/// Deserialize an optional integer that may arrive as a **string** — the shape
/// an environment override takes: the `config` crate merges env values as
/// strings, and inside the internally-tagged `block_source` table serde's
/// content buffering performs no string→int coercion (which is why the plain
/// TOML integer also has to be accepted here).
fn de_opt_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IntOrString {
        Int(usize),
        String(String),
    }

    match Option::<IntOrString>::deserialize(deserializer)? {
        None => Ok(None),
        Some(IntOrString::Int(value)) => Ok(Some(value)),
        Some(IntOrString::String(raw)) => raw.parse().map(Some).map_err(de::Error::custom),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LocalSourceConfig {
    /// The co-located node's indexer cache directory. A relative path resolves
    /// against `--home`.
    pub cache_path: PathBuf,
    /// Base URL of the co-located node's httpd.
    pub node_url: String,
}

/// The REST read API. `enabled = false` runs the indexer ingest-only.
#[derive(Debug, Clone, Deserialize)]
pub struct HttpdConfig {
    /// Serve the read API. Default `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Socket address to bind, e.g. `0.0.0.0:8080`.
    #[serde(default = "default_bind")]
    pub bind: String,
}

impl Default for HttpdConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            bind: default_bind(),
        }
    }
}

/// The Prometheus `/metrics` endpoint. Recording is always installed (cheap);
/// this only controls whether the scrape endpoint is served — `enabled = false`
/// keeps metrics internal and binds no port. Separate from the read-API `httpd`
/// so metrics stay available even when the read API runs ingest-only.
#[derive(Debug, Clone, Deserialize)]
pub struct MetricsConfig {
    /// Serve the `/metrics` endpoint. Default `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Address to bind — default `0.0.0.0`, so an external Prometheus on the
    /// detached host can scrape it.
    #[serde(default = "default_metrics_ip")]
    pub ip: String,
    /// Port to bind. Default `9191`, matching the dango node's metrics-port
    /// convention.
    #[serde(default = "default_metrics_port")]
    pub port: u16,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            ip: default_metrics_ip(),
            port: default_metrics_port(),
        }
    }
}

fn default_max_connections() -> u32 {
    10
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_metrics_ip() -> String {
    "0.0.0.0".to_string()
}

fn default_metrics_port() -> u16 {
    9191
}

fn default_true() -> bool {
    true
}

fn default_bind() -> String {
    "0.0.0.0:8080".to_string()
}

#[cfg(test)]
mod tests {
    use {super::*, dango_config_parser::parse_config, std::collections::HashSet};

    /// The committed `config.example.toml` (also the user-facing example).
    fn example_config() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.example.toml")
    }

    /// The shipped example config parses into the expected shape (the
    /// `block_source` tag selects the right variant, the fetcher defaults /
    /// resolves, `max_connections` falls back to its default), and an
    /// `SECTION__FIELD` environment variable overrides the TOML — the
    /// `dango-cli` behaviour we must preserve. Doubling as a guard that the
    /// documented example always stays valid.
    ///
    /// Both halves live in one test on purpose: `parse_config` reads the whole
    /// process environment, so a separate env-mutating test could race a
    /// parallel one. Here the override is set and removed within a single,
    /// serial body.
    #[test]
    fn config_parses_and_env_overrides() {
        let path = example_config();

        // Baseline: TOML values + defaults.
        let cfg: Config = parse_config(&path).unwrap();
        assert_eq!(cfg.postgres.url, "postgres://postgres@localhost/archive");
        assert_eq!(cfg.postgres.max_connections, 10); // default
        match &cfg.block_source {
            BlockSourceConfig::Remote(remote) => {
                assert_eq!(remote.store_path, PathBuf::from("data/blocks"));
                assert_eq!(remote.live_url, "http://node:8080");
                // The TOML-integer path through `de_opt_usize`.
                assert!(matches!(
                    remote.fetcher,
                    FetcherConfig::Sentinel {
                        parallelism: Some(4)
                    }
                ));
            },
            other => panic!("expected a remote source, got {other:?}"),
        }
        assert!(cfg.httpd.enabled);
        assert_eq!(cfg.httpd.bind, "0.0.0.0:8080");

        // `[metrics]`: the Prometheus scrape endpoint.
        assert!(cfg.metrics.enabled);
        assert_eq!(cfg.metrics.ip, "0.0.0.0");
        assert_eq!(cfg.metrics.port, 9191);

        // `[activity]`: a filter parses as a `WhiteOrBlackList`; omitted filters
        // stay `None` (the projection's built-in default applies later).
        assert!(cfg.activity.involvement_blacklist.is_empty());
        assert_eq!(
            cfg.activity.event_data_filter,
            Some(WhiteOrBlackList::Whitelist(HashSet::from([
                EventType::Transfer,
                EventType::ContractEvent,
            ])))
        );
        assert!(cfg.activity.event_type_filter.is_none());

        // Environment overrides — the ones the deploy role can inject (env.j2):
        // `POSTGRES__URL` wins over the TOML value, while `BLOCK_SOURCE__LIVE_URL`
        // and `BLOCK_SOURCE__FETCHER__PARALLELISM` reach *inside* the
        // internally-tagged `block_source` table (the fragile paths: the env
        // value must merge into the table without clobbering its `type` tag,
        // and a numeric field arrives as a string — the `de_opt_usize` path).
        // SAFETY: single-threaded within this test; set then immediately
        // cleared, and no other test reads these keys.
        unsafe {
            std::env::set_var("POSTGRES__URL", "postgres://override@host/db");
            std::env::set_var("BLOCK_SOURCE__LIVE_URL", "http://sentinel:9999");
            std::env::set_var("BLOCK_SOURCE__FETCHER__PARALLELISM", "6");
        }
        let overridden: Config = parse_config(&path).unwrap();
        unsafe {
            std::env::remove_var("POSTGRES__URL");
            std::env::remove_var("BLOCK_SOURCE__LIVE_URL");
            std::env::remove_var("BLOCK_SOURCE__FETCHER__PARALLELISM");
        }
        assert_eq!(overridden.postgres.url, "postgres://override@host/db");
        match &overridden.block_source {
            BlockSourceConfig::Remote(remote) => {
                assert_eq!(remote.live_url, "http://sentinel:9999");
                assert!(matches!(
                    remote.fetcher,
                    FetcherConfig::Sentinel {
                        parallelism: Some(6)
                    }
                ));
            },
            other => panic!("expected a remote source, got {other:?}"),
        }
    }
}
