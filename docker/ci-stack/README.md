# CI Stack

A minimal Docker Compose stack used by the "Testing ci-stack" job in [`.github/workflows/rust.yml`](../../.github/workflows/rust.yml) to smoke-test the published Docker images (`dango`, `cometbft`, `faucet`, `dango-frontend`) against a fresh single-node chain with the full indexer backend (PostgreSQL + ClickHouse).

This is _not_ a development environment. For writing and running tests, use the pure-Rust framework in [`dango/testing`](../../dango/testing).

## Booting manually (debugging)

```bash
cd docker/ci-stack
docker compose up -d --wait
curl localhost:8080/up
docker compose down -v
```

Useful environment variables: `DANGO_TAG` (default `latest`), `COMETBFT_TAG`, `FAUCET_BOT_TAG`, `PYTH__ACCESS_TOKEN` (needed for price feeds), and `DANGO_PORT`/`COMETBFT_PORT`/`FAUCET_PORT`/`POSTGRES_PORT`/`CLICKHOUSE_PORT` (set to `0` for a random free port).

The frontend container is behind the `frontend` profile and requires `DANGO_CONFIG_JSON` (see how the CI job constructs it):

```bash
DANGO_CONFIG_JSON='...' docker compose --profile frontend up -d --wait
docker compose --profile frontend down -v
```

## Config files

`configs/cometbft/config/genesis.json` (chain ID `dev-6`) and `configs/dango/config/app.toml` must be regenerated in lockstep with the templates in [`deploy/roles/full-app/templates/config/`](../../deploy/roles/full-app/templates/config/) whenever genesis or config code changes.

The CometBFT keys in `configs/cometbft/config/` are throwaway keys for this single-node network. Do not reuse them for any other purpose.
