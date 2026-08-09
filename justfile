set positional-arguments

# List available recipes
default:
  @just --list

# ------------------------------------ Git -------------------------------------

# Sync the `main` branch with the origin
git-fetch-main:
  git fetch origin main:main

# Delete all local git branches except for main
git-clear-branches:
  git branch | grep -v "main" | xargs git branch -D

# Create a tag at the given commit and push only that tag to origin
create-and-push-tag commit-hash tag:
  git tag {{tag}} {{commit-hash}}
  git push origin {{tag}}

# Create a branch off the given commit and push only that branch to origin
create-and-push-branch commit-hash branch:
  git branch {{branch}} {{commit-hash}}
  git push origin {{branch}}

# ------------------------------------ Rust ------------------------------------

# Compile and install the Dango node software
install-node:
  cargo install --path dango/cli --locked

# Compile and install the Dango client CLI
install-client:
  cargo install --path dango/sdk/cli --locked

# Run all tests
test:
  RUST_BACKTRACE=1 cargo test --all-features --tests -- --nocapture

# Run all perp-related tests specifically
test-perps:
  RUST_BACKTRACE=1 cargo test --all-features --tests -p dango-types perps::tests -- --nocapture
  RUST_BACKTRACE=1 cargo test --all-features --tests -p dango-order-book -- --nocapture
  RUST_BACKTRACE=1 cargo test --all-features --tests -p dango-perps -- --nocapture
  RUST_BACKTRACE=1 cargo test --all-features -p dango-testing --test perps -- --nocapture

# Run all dango-related tests specifically
test-dango:
  RUST_BACKTRACE=1 cargo test --all-features -p dango-testing -- --nocapture

# Check whether the code compiles
check:
  cargo check --bins --tests --benches --examples --all-features --all-targets

# Perform linting
lint:
  cargo clippy --bins --tests --benches --examples --all-features --all-targets -- -D warnings

# Perform linting but with `--no-default-features` enabled for each crate
lint-without-features:
  #!/usr/bin/env bash
  set -euo pipefail
  crates=($(cargo metadata --format-version=1 --no-deps | jq -r '.packages[].name'))
  total=${#crates[@]}
  for i in "${!crates[@]}"; do
    crate="${crates[$i]}"
    echo "[$((i+1))/$total] Checking $crate..."
    cargo clippy -p "$crate" --bins --tests --benches --examples --no-default-features --all-targets -- -D warnings
  done

# Perform formatting
fmt:
  cargo fmt --all

# Build schema
build-graphql-schema:
  cargo run -p dango-indexer-httpd --bin build_graphql_schema -- \
    ./dango/indexer/graphql-types/src/schemas/schema.graphql

# Build the Dango Book
book:
  mdbook build --open

# Update wasm artifacts used in tests
update-testdata:
  cp -v artifacts/dango_tester.wasm dango/core/vm/wasm/testdata/

# ---------------------------------- Frontend ----------------------------------

run-website:
  pnpm i
  pnpm dev:portal-web

# --------------------------------- Optimizer ----------------------------------

OPTIMIZER_NAME := "leftcurve/bob-arm64"
OPTIMIZER_VERSION := "0.2.0"

# Compile and optimize contracts
optimize:
  docker run --rm -v "$(pwd)":/code \
    --mount type=volume,source="$(basename "$(pwd)")_cache",target=/target \
    --mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
    {{OPTIMIZER_NAME}}:{{OPTIMIZER_VERSION}}
