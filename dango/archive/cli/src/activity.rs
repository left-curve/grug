//! Build the activity projection's [`ActivityConfig`].
//!
//! The event-type filters come from config (each overriding the projection's
//! built-in default), and `involvement_blacklist` is the config addresses
//! merged with the deployment's **system contracts** — read from the node's
//! `app_config` and harvested with `Extractable`, so the per-network addresses
//! never have to be listed by hand. The same `app_config` also names the
//! deployment's **perps contract** (`addresses.perps`), injected as the anchor
//! of the read API's `/events/perps` shortcut.

use {
    crate::config::ActivitySettings,
    anyhow::{Context, bail},
    dango_archive_projection::ActivityConfig,
    dango_primitives::{Addr, Extractable, Json, JsonDeExt, QueryClientExt},
    dango_sdk::HttpClient,
    dango_types::config::AppConfig,
    std::{collections::HashSet, str::FromStr, time::Duration},
};

/// How many times to try the `app_config` query before giving up.
const APP_CONFIG_ATTEMPTS: usize = 10;
/// Backoff between `app_config` query attempts.
const APP_CONFIG_BACKOFF: Duration = Duration::from_secs(3);

/// Resolve the activity projection's [`ActivityConfig`]: the event-type filters
/// (config override, else the built-in default), `involvement_blacklist` = the
/// config addresses ∪ the node's system contracts (from `app_config`), and
/// `perps_contract` = the typed `addresses.perps` of the same `app_config`.
pub async fn config(settings: &ActivitySettings, node_url: &str) -> anyhow::Result<ActivityConfig> {
    let mut config = ActivityConfig::default();

    if let Some(filter) = settings.event_type_filter.clone() {
        config.event_type_filter = filter;
    }
    if let Some(filter) = settings.event_data_filter.clone() {
        config.event_data_filter = filter;
    }
    if let Some(filter) = settings.involvement_filter.clone() {
        config.involvement_filter = filter;
    }

    let app_config = app_config(node_url).await?;

    // involvement_blacklist = config addresses ∪ the node's system contracts —
    // the latter harvested generically with `Extractable`, so the per-network
    // addresses never have to be listed by hand.
    let mut blacklist = HashSet::new();
    for raw in &settings.involvement_blacklist {
        let addr = Addr::from_str(raw)
            .with_context(|| format!("invalid involvement_blacklist address `{raw}`"))?;
        blacklist.insert(addr);
    }
    let mut system_contracts = HashSet::new();
    app_config.extract_addresses(&mut system_contracts);
    tracing::info!(
        count = system_contracts.len(),
        "extracted system contracts from app_config"
    );
    blacklist.extend(system_contracts);
    tracing::info!(
        addresses = blacklist.len(),
        "activity involvement blacklist assembled"
    );
    config.involvement_blacklist = blacklist;

    // The perps contract anchors the read API's `/events/perps` shortcut. The
    // typed parse is best-effort: a deployment whose `app_config` no longer
    // matches [`AppConfig`] only loses the shortcut route (warn), never ingest
    // — the blacklist harvest above is shape-agnostic.
    match app_config.deserialize_json::<AppConfig>() {
        Ok(app_config) => {
            tracing::info!(
                perps = %app_config.addresses.perps,
                "resolved the perps contract from app_config"
            );
            config.perps_contract = Some(app_config.addresses.perps);
        },
        Err(err) => tracing::warn!(
            error = %err,
            "app_config does not deserialize as AppConfig; \
             the /events/perps shortcut will not be mounted"
        ),
    }

    Ok(config)
}

/// Query the node's `app_config`. The block source needs the same node, so one
/// that never answers is fatal: retry [`APP_CONFIG_ATTEMPTS`] times with a
/// backoff, then give up and let the caller abort.
async fn app_config(node_url: &str) -> anyhow::Result<Json> {
    let client =
        HttpClient::new(node_url).with_context(|| format!("invalid node url `{node_url}`"))?;

    let mut last_err = None;
    for attempt in 1..=APP_CONFIG_ATTEMPTS {
        match client.query_app_config::<Json>().await {
            Ok(app_config) => return Ok(app_config),
            Err(err) => {
                tracing::warn!(attempt, error = %err, "app_config query failed; retrying");
                last_err = Some(err);
                tokio::time::sleep(APP_CONFIG_BACKOFF).await;
            },
        }
    }

    bail!(
        "app_config query failed after {APP_CONFIG_ATTEMPTS} attempts: {}",
        last_err.expect("at least one attempt ran")
    );
}
