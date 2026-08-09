mod config;
mod db;
mod home_directory;
mod indexer;
mod start;
mod telemetry;

use {
    crate::{db::DbCmd, home_directory::HomeDirectory, indexer::IndexerCmd, start::StartCmd},
    clap::{CommandFactory, FromArgMatches, Parser},
    config::Config,
    dango_config_parser::parse_config,
    opentelemetry::{KeyValue, trace::TracerProvider},
    opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig},
    opentelemetry_sdk::{Resource, trace as sdktrace},
    sentry::integrations::tracing::layer as sentry_layer,
    std::{
        path::PathBuf,
        sync::{Arc, LazyLock},
    },
    tracing_opentelemetry::layer as otel_layer,
    tracing_subscriber::{fmt::format::FmtSpan, prelude::*},
};

static VERSION_WITH_COMMIT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{} ({})",
        env!("CARGO_PKG_VERSION"),
        dango_primitives::GIT_COMMIT
    )
});

#[derive(Parser)]
#[command(author, about, next_display_order = None)]
struct Cli {
    /// Directory for the physical database [default: ~/.dango]
    #[arg(long, global = true)]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Parser)]
enum Command {
    /// Manage the database
    #[command(subcommand, next_display_order = None)]
    Db(DbCmd),

    /// Indexer related commands
    Indexer(IndexerCmd),

    /// Start the node
    Start(StartCmd),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse CLI arguments, overriding the version to include the git commit.
    let cli = {
        let cmd = Cli::command().version(VERSION_WITH_COMMIT.as_str());
        let matches = cmd.get_matches();
        Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())
    };

    // Find the home directory from the CLI `--home` flag.
    let app_dir = HomeDirectory::new_or_default(cli.home)?;

    // Parse the config file.
    let cfg: Config = parse_config(app_dir.config_file())?;

    // Common environment metadata shared between telemetry backends.
    let non_empty_env = |key: &str| std::env::var(key).ok().filter(|s| !s.is_empty());
    let service_instance_id = cfg.transactions.chain_id.clone();
    let service_namespace = non_empty_env("SERVICE_NAMESPACE")
        .or_else(|| non_empty_env("DEPLOY_ENV"))
        .or_else(|| (!cfg.sentry.environment.is_empty()).then(|| cfg.sentry.environment.clone()))
        .or_else(|| non_empty_env("DEPLOYMENT_NAME"));
    let deployment_environment = non_empty_env("DEPLOY_ENV")
        .or_else(|| (!cfg.sentry.environment.is_empty()).then(|| cfg.sentry.environment.clone()));
    let deployment_name = non_empty_env("DEPLOYMENT_NAME");
    let host_name = non_empty_env("HOSTNAME");
    let dango_network = non_empty_env("DANGO_NETWORK");

    // Create the base environment filter
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| cfg.log_level.clone().into()); // Default to `cfg.log_level` if `RUST_LOG` not set.

    // Create the fmt layer based on the configured format
    let fmt_layer = match cfg.log_format {
        config::LogFormat::Json => tracing_subscriber::fmt::layer()
            .json()
            .with_span_events(FmtSpan::NONE)
            .with_target(true)
            .with_thread_ids(true)
            .with_file(true)
            .with_line_number(true)
            .boxed(),
        config::LogFormat::Text => tracing_subscriber::fmt::layer().boxed(),
    };

    // Optionally build an OpenTelemetry layer if tracing export is enabled.
    let otel_layer_opt = if cfg.trace.enabled {
        let mut attrs = vec![
            // Keep existing chain id for querying
            KeyValue::new("chain.id", cfg.transactions.chain_id.clone()),
            // Required: service.instance.id — default to chain_id
            KeyValue::new("service.instance.id", service_instance_id.clone()),
        ];

        // Required: service.namespace — priority: SERVICE_NAMESPACE > DEPLOY_ENV > sentry.environment > DEPLOYMENT_NAME
        if let Some(ns) = service_namespace.clone() {
            attrs.push(KeyValue::new("service.namespace", ns));
        }

        // Optional: deployment.environment — prefer DEPLOY_ENV, else sentry.environment
        if let Some(env) = deployment_environment.clone() {
            attrs.push(KeyValue::new("deployment.environment", env));
        }

        // Optional: host.name — read from HOSTNAME if provided
        if let Some(host) = host_name.clone() {
            attrs.push(KeyValue::new("hostname", host));
        }

        if let Some(dango_network) = dango_network.clone() {
            attrs.push(KeyValue::new("dango_network", dango_network));
        }

        let resource = Resource::builder()
            .with_service_name("dango")
            .with_attributes(attrs)
            .build();

        // Build exporter and tracer provider
        // Build exporter via selected OTLP protocol (gRPC or HTTP).
        let exporter = match cfg.trace.protocol {
            config::TraceProtocol::OtlpGrpc => SpanExporter::builder()
                .with_tonic()
                .with_endpoint(cfg.trace.endpoint.clone())
                .with_protocol(Protocol::Grpc)
                .build()?,
            config::TraceProtocol::OtlpHttp => SpanExporter::builder()
                .with_http()
                .with_endpoint(cfg.trace.endpoint.clone())
                .with_protocol(Protocol::HttpBinary)
                .build()?,
        };

        let provider = sdktrace::SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource)
            .build();

        // Register provider in a global OnceLock so signal handlers can shut it down.
        let tracer = provider.tracer("dango");
        crate::telemetry::set_provider(provider);
        Some(otel_layer().with_tracer(tracer))
    } else {
        None
    };

    let mut _sentry_guard: Option<sentry::ClientInitGuard> = None;
    let sentry_layer = if cfg.sentry.enabled {
        let guard = sentry::init((
            cfg.sentry.dsn,
            sentry::ClientOptions {
                environment: Some(cfg.sentry.environment.clone().into()),
                release: sentry::release_name!(),
                enable_logs: cfg.sentry.enable_logs,
                sample_rate: cfg.sentry.sample_rate,
                traces_sample_rate: cfg.sentry.traces_sample_rate,
                // Drop noisy exporter transport errors that surface as trace logs.
                before_send: Some(Arc::new(|event| {
                    if event.logger.as_deref() == Some("opentelemetry_sdk") {
                        return None;
                    }
                    Some(event)
                })),
                ..Default::default()
            },
        ));
        _sentry_guard = Some(guard);

        sentry::configure_scope(|scope| {
            scope.set_tag("chain-id", &cfg.transactions.chain_id);
            scope.set_tag("service.instance.id", &service_instance_id);
            if let Some(ns) = &service_namespace {
                scope.set_tag("service.namespace", ns);
            }
            if let Some(env) = &deployment_environment {
                scope.set_tag("deployment.environment", env);
            }
            if let Some(name) = &deployment_name {
                scope.set_tag("deployment_name", name);
            }
            if let Some(host) = &host_name {
                scope.set_tag("hostname", host);
            }
            if let Some(dango_network) = &dango_network {
                scope.set_tag("dango_network", dango_network);
            }
            scope.set_extra("version", VERSION_WITH_COMMIT.as_str().into());
        });
        Some(sentry_layer())
    } else {
        None
    };

    // Compose the subscriber with optional layers (Option implements Layer)
    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(sentry_layer)
        .with(otel_layer_opt)
        .init();

    match cli.command {
        Command::Db(cmd) => cmd.run(app_dir)?,
        Command::Indexer(cmd) => cmd.run(app_dir).await?,
        Command::Start(cmd) => cmd.run(app_dir).await?,
    }

    // Flush and shutdown the tracer provider (if set) to avoid losing spans.
    crate::telemetry::shutdown();
    // Flush Sentry transport too.
    crate::telemetry::shutdown_sentry();

    Ok(())
}
