---
paths:
  - "**/*.rs"
---
# Rust Guidelines

Rust 2024 edition. Write idiomatic, Rustacean code. Prioritize clarity,
modularity, and zero-cost abstractions.

## Workspace Dependencies

Add new crates to root `Cargo.toml` `[workspace.dependencies]`, reference with
`{ workspace = true }`. Never add versions directly in a crate's `Cargo.toml`.
Prefer latest stable crates.io version.

```toml
# Root Cargo.toml
[workspace.dependencies]
some-crate = "1.2"

# dango/cli/Cargo.toml
[dependencies]
some-crate = { workspace = true }
```

Run `cargo fetch` after dependency changes, verify with `cargo fetch --locked`.

## Cargo Features

Enable by default unless explicitly asked otherwise (opt-out, not opt-in).
This prevents features working in isolation but missing from the main binary.

## Structure

- Flat structure: no nested crates, no sub-directories in `src/`.
  If a crate needs a subdirectory, break it into multiple crates.
- No submodules inside files (except `#[cfg(test)] mod tests`).
  Use separator comments instead:

```rust
// ---- implement display trait ----

impl Display for MyType {
    // ...
}
```

- Trait bounds always use `where` syntax:

```rust
// BAD
fn new_error(msg: impl ToString) -> Error { /* ... */ }
fn new_error<M: ToString>(msg: M) -> Error { /* ... */ }

// GOOD
fn new_error<M>(msg: M) -> Error
where
    M: ToString,
{
    // ...
}
```

- Group all imports in a single `use { ... }` block:

```rust
// BAD
use crate::{Uint128, Uint256};
use serde::{de, ser};
use std::str::FromStr;

// GOOD
use {
    crate::{Uint128, Uint256},
    serde::{de, ser},
    std::str::FromStr,
};
```

- Error messages lowercase
- Comments in markdown, 80 char max width; prefer above-line over trailing
- Feature-gated imports go in the file header, not inline in functions

## Traits and Generics

- Traits for behavior boundaries — enables alternative implementations and mocking
- Generic parameters (`fn foo<T: MyTrait>(t: T)`) for hot paths (monomorphization)
- `dyn Trait` behind `Arc`/`Box` for heterogeneous collections or runtime dispatch
- `Derive Default` when all fields have sensible defaults

## Typed Data Over Loose JSON

Use concrete Rust types (`struct`, `enum`) instead of `serde_json::Value`
wherever the shape is known. Reserve `serde_json::Value` for truly dynamic data.

## Type System

Always use types for comparisons — never convert to strings:
- Exhaustiveness checking: compiler warns on missed variants
- Refactoring safety: renaming a variant updates all match arms
- No typos: `Database::Rdis` won't compile, `"rdis"` will

Only convert to strings at boundaries: serialization, database storage, logging.

## Type Conversions

- `From`/`Into`/`TryFrom`/`TryInto` for conversions, not ad-hoc match blocks
- Typed structs + serde over raw `serde_json::Value` in production
- Untyped JSON maps are test-only scaffolding
- Ask user before adding manual conversion paths

## Concurrency

- Streaming over non-streaming API calls
- `tokio::join!`/`futures::join_all`/`FuturesUnordered` for independent async work
- Sequential `.await` only when each step depends on the previous
- Never `block_on` inside async context
- `Mutex` must guard actual state, never `Mutex<()>`

## Error Handling

- `anyhow::Result` for app-level, `thiserror` for library-level
- Propagate with `?`; no `.unwrap()` outside tests

## Date, Time, and Crate Reuse

Use the `time` crate (workspace dependency), never manual epoch math:

```rust
// GOOD
time::Duration::days(30).unsigned_abs()
time::OffsetDateTime::now_utc().date()

// BAD
days * 86400
days * 24 * 60 * 60
```

If a workspace crate provides a clear one-liner, use it instead of reimplementing.

## General Style

- Iterators and combinators (`.map()`, `.filter()`, `.collect()`) over manual loops
- `Cow<'_, str>` when allocation is conditional
- Small public API surfaces: `pub use` re-exports in `lib.rs`
- `#[must_use]` on functions whose return value matters

## Tracing & Metrics (required on all crates)

Gate with `#[cfg(feature = "tracing")]` and `#[cfg(feature = "metrics")]`.
Features must be enabled by default.

```rust
#[cfg(feature = "tracing")]
use tracing::{debug, instrument, warn};

#[cfg(feature = "metrics")]
use moltis_metrics::{counter, histogram, labels};

#[cfg_attr(feature = "tracing", instrument(skip(self)))]
pub async fn process_request(&self, req: Request) -> Result<Response> {
    #[cfg(feature = "metrics")]
    let start = std::time::Instant::now();

    // ... do work ...

    #[cfg(feature = "metrics")]
    {
        counter!("my_crate_requests_total").increment(1);
        histogram!("my_crate_request_duration_seconds")
            .record(start.elapsed().as_secs_f64());
    }

    Ok(response)
}
```

Telemetry details (OTLP + Sentry, graceful shutdown): see `telemetry.md`.

## Runtime & Signals

- `dango/cli/src/start.rs` listens for SIGINT/SIGTERM
- Global resources (tracer provider, Sentry) must flush on signal
- Shared shutdown helpers in `dango/cli/src/telemetry.rs`

## Version Alignment

- OpenTelemetry: `0.31`, tracing-opentelemetry: `0.32`, sentry: `0.38`, tokio: `1.x`
- Keep aligned across crates to avoid trait/type mismatches
- Avoid reassigning tracing subscribers; compose with `.with(optional_layer)`
- Prefer OTLP path over Jaeger (Tempo supports OTLP)

## Config Changes

1. Add serde structs/fields in the relevant `config.rs`
2. Update both templates: `docker/ci-stack/configs/` and `deploy/roles/`
3. Parse with `dango_config_parser::parse_config` in the target binary
