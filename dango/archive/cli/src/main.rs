//! `archive` — the archive's command-line entry point.
//!
//! A small `clap` front end (structure inspired by `dango-cli`, kept minimal):
//! a global `--home` and one subcommand per mode of operation. For now the only
//! command is [`start`](start), which boots the ingest + read-API service.
//!
//! Logs are a single `fmt` layer honoring `RUST_LOG` (the OTLP / Sentry tracing
//! export `dango-cli` wires can be layered on here later); metrics are a global
//! Prometheus recorder installed in `start` and scraped at `/metrics` — see
//! `design/observability.md`.

mod activity;
mod config;
mod db;
mod home_directory;
mod metrics;
mod source;
mod start;

use {
    crate::{
        config::{Config, LogFormat},
        home_directory::HomeDirectory,
        start::StartCmd,
    },
    anyhow::Context,
    clap::{CommandFactory, FromArgMatches, Parser, Subcommand},
    dango_config_parser::parse_config,
    std::{path::PathBuf, sync::LazyLock},
    tracing_subscriber::{EnvFilter, prelude::*},
};

/// Crate version with the embedded git commit, surfaced by `--version`.
static VERSION_WITH_COMMIT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{} ({})",
        env!("CARGO_PKG_VERSION"),
        dango_primitives::GIT_COMMIT
    )
});

/// The archive: ingest blocks from a `BlockSource` and serve the
/// projections' REST read API.
#[derive(Parser)]
#[command(author, about, next_display_order = None)]
struct Cli {
    /// Directory for config and local state [default: ~/.archive]
    #[arg(long, global = true)]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the indexer: ingest blocks and serve the read API.
    Start(StartCmd),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse args, overriding `--version` to include the git commit.
    let cli = {
        let cmd = Cli::command().version(VERSION_WITH_COMMIT.as_str());
        let matches = cmd.get_matches();
        Cli::from_arg_matches(&matches).unwrap_or_else(|err| err.exit())
    };

    let home = HomeDirectory::new_or_default(cli.home)?;

    // Parse the config before installing tracing, so the log level and format
    // come from it (TOML + `SECTION__FIELD` env overrides) rather than
    // `RUST_LOG`. `start` reuses this same `cfg`.
    let cfg: Config = parse_config(home.config_file())?;
    init_tracing(&cfg)?;

    match cli.command {
        Command::Start(cmd) => cmd.run(home, cfg).await?,
    }

    Ok(())
}

/// Install the global tracing subscriber from config: a single `fmt` layer
/// filtered by `cfg.log_level` and rendered per `cfg.log_format`.
///
/// The OTLP / Sentry export layers can be composed in here later — but when
/// they are, this binary MUST also grow a SIGINT/SIGTERM handler that flushes
/// the tracer provider / Sentry on shutdown (per `.claude/rules/rust.md`), or
/// buffered spans/events are lost on exit. Today there is nothing to flush
/// (fmt is synchronous, metrics are pull-based) and the commit protocol is
/// crash-safe, so the default terminate-on-signal is safe; that stops being
/// true the moment a buffered exporter is added.
fn init_tracing(cfg: &Config) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(&cfg.log_level)
        .with_context(|| format!("invalid log_level `{}`", cfg.log_level))?;
    let registry = tracing_subscriber::registry().with(filter);
    match cfg.log_format {
        LogFormat::Json => registry
            .with(tracing_subscriber::fmt::layer().json())
            .init(),
        LogFormat::Plain => registry.with(tracing_subscriber::fmt::layer()).init(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `clap` derive sanity: the command tree is internally consistent (no
    /// conflicting args, valid subcommands). A cheap guard as commands grow.
    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }
}
