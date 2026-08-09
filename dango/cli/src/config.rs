use {
    dango_primitives::{HexBinary, HttpdConfig},
    serde::{Deserialize, Serialize},
};

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    pub grug: GrugConfig,
    pub indexer: IndexerConfig,
    pub httpd: HttpdConfig,
    pub metrics_httpd: MetricsHttpdConfig,
    pub tendermint: TendermintConfig,
    pub transactions: TransactionsConfig,
    pub sentry: SentryConfig,
    pub trace: TraceConfig,
    pub log_level: String,
    pub log_format: LogFormat,
    pub pyth: PythLazerConfig,
}

#[derive(Serialize, Deserialize, Default)]
pub struct ClickhouseConfig {
    pub url: String,
    pub database: String,
    pub user: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

#[derive(Serialize, Deserialize)]
pub struct GrugConfig {
    pub wasm_cache_capacity: usize,
    pub query_gas_limit: u64,
    pub priority_range: Option<(HexBinary, HexBinary)>,
}

impl Default for GrugConfig {
    fn default() -> Self {
        Self {
            wasm_cache_capacity: 1000,
            query_gas_limit: 100_000_000,
            priority_range: None,
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct SentryConfig {
    pub enabled: bool,
    pub enable_logs: bool,
    pub dsn: String,
    pub environment: String,
    pub sample_rate: f32,
    pub traces_sample_rate: f32,
}

#[derive(Serialize, Deserialize, Default)]
pub struct IndexerConfig {
    pub keep_blocks: bool,
    pub database: IndexerDatabaseConfig,
    pub clickhouse: ClickhouseConfig,
}

#[derive(Serialize, Deserialize)]
pub struct IndexerDatabaseConfig {
    pub url: String,
    pub max_connections: u32,
}

impl Default for IndexerDatabaseConfig {
    fn default() -> Self {
        IndexerDatabaseConfig {
            url: "sqlite::memory:".to_string(),
            max_connections: 10,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct TendermintConfig {
    pub rpc_addr: String,
    pub abci_addr: String,

    /// Number of most-recent blocks for CometBFT to retain; older blocks are
    /// pruned. `0` retains all blocks.
    #[serde(default)]
    pub retain_recent_blocks: u64,
}

impl Default for TendermintConfig {
    fn default() -> Self {
        Self {
            rpc_addr: "http://localhost:26657".to_string(),
            abci_addr: "http://localhost:26658".to_string(),
            retain_recent_blocks: 0,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct TransactionsConfig {
    pub chain_id: String,
    pub gas_adjustment: f64,
}

impl Default for TransactionsConfig {
    fn default() -> Self {
        Self {
            chain_id: "dango-1".to_string(),
            gas_adjustment: 1.4,
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct PythLazerConfig {
    pub endpoints: Vec<String>,
    pub access_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsHttpdConfig {
    pub enabled: bool,
    pub ip: String,
    pub port: u16,
}

impl Default for MetricsHttpdConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ip: "127.0.0.1".to_string(),
            port: 9191,
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct TraceConfig {
    /// Whether to enable OpenTelemetry tracing export.
    pub enabled: bool,
    /// Collector endpoint (e.g. http://tempo:4317 for OTLP/gRPC).
    pub endpoint: String,
    /// Protocol used to export traces: "otlp_grpc" or "otlp_http".
    pub protocol: TraceProtocol,
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum TraceProtocol {
    #[default]
    OtlpGrpc,
    OtlpHttp,
}
