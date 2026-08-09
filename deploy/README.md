# deploy

## Architecture Overview

This directory contains Ansible playbooks and roles for deploying the Dango
blockchain platform across OVH, Hetzner, and Interserver. Servers communicate
over private networks (WireGuard `10.99.0.x` mesh and Tailscale) and are
exposed to the public internet through Cloudflare tunnels.

### Server Inventory

| Hostname | Tailscale IP   | WireGuard IP | Network        |
| -------- | -------------- | ------------ | -------------- |
| ovh1     | 100.96.253.40  | 10.99.0.1    | devnet         |
| ovh2     | 100.107.248.71 | 10.99.0.2    | devnet         |
| ovh3     | 100.122.37.57  | 10.99.0.3    | monitoring hub |
| hetzner6 | 100.66.152.68  | 10.99.0.15   | mainnet        |
| hetzner1 | 100.126.8.2    | 10.99.0.8    | mainnet        |
| hetzner2 | 100.90.163.19  | 10.99.0.9    | testnet        |
| hetzner3 | 100.76.197.30  | 10.99.0.10   | mainnet        |
| hetzner4 | 100.109.200.70 | 10.99.0.11   | testnet        |
| hetzner5 | 100.72.62.100  | 10.99.0.14   | mainnet        |

> ovh4-7 are GitHub Actions runners (not application servers).

### Services

#### Application services (per full-app server)

| Service        | Description                                                     | Notes                 |
| -------------- | --------------------------------------------------------------- | --------------------- |
| dango          | Blockchain node / API server, connects to postgres + clickhouse |                       |
| cometbft       | BFT consensus engine, P2P between nodes via WireGuard           |                       |
| dango-frontend | Web UI                                                          |                       |
| graphiql       | GraphQL IDE                                                     |                       |
| faucet-bot     | Token faucet                                                    | _testnet/devnet only_ |
| points-bot     | Points/achievements tracking                                    | runs on ovh2          |

#### Infrastructure services (per server)

| Service       | Description                                  |
| ------------- | -------------------------------------------- |
| traefik       | Reverse proxy, TLS termination, port routing |
| cloudflared   | Cloudflare Tunnel for secure ingress         |
| postgres      | Relational database                          |
| clickhouse    | Analytics/indexer database                   |
| promtail      | Ships Docker + system logs to Loki           |
| node-exporter | Exports host metrics for Prometheus          |
| dozzle        | Real-time Docker log viewer                  |

#### Centralized monitoring (ovh3)

| Service    | Description                                                                |
| ---------- | -------------------------------------------------------------------------- |
| grafana    | Dashboards (queries Prometheus, Loki, Tempo)                               |
| prometheus | Metrics collection + alerting (with VictoriaMetrics for long-term storage) |
| loki       | Log aggregation                                                            |
| tempo      | Distributed tracing                                                        |

#### Other services

| Service     | Description                               | Location                   |
| ----------- | ----------------------------------------- | -------------------------- |
| hyperlane   | Cross-chain message validators + relayers | hetzner1-6                 |
| uptimekuma  | Service health monitoring                 | ovh3                       |
| vaultwarden | Password manager                          | ovh3                       |
| metabase    | BI/analytics dashboards                   | ovh3                       |
| homer       | Service dashboard homepage                | ovh1                       |
| cosign      | Container image signature verification    |                            |

### Architecture Diagram

```mermaid
flowchart TD
    subgraph External
        User([User])
        CF[Cloudflare CDN]
        Discord([Discord])
    end

    subgraph FullApp["Per-Server (full-app)"]
        cloudflared[cloudflared]
        Traefik[traefik]
        Dango[dango]
        CometBFT[cometbft]
        Frontend[dango-frontend]
        GraphiQL[graphiql]
        FaucetBot[faucet-bot]
        PointsBot[points-bot]
        Postgres[(postgres)]
        ClickHouse[(clickhouse)]
        Promtail[promtail]
        NodeExp[node-exporter]
    end

    subgraph MonitoringHub["Monitoring Hub (ovh3)"]
        Grafana[grafana]
        Prometheus[prometheus]
        Loki[loki]
        Tempo[tempo]
        Alertmanager[alertmanager]
    end

    %% External ingress
    User --> CF --> cloudflared
    cloudflared --> Traefik
    Traefik --> Dango
    Traefik --> Frontend
    Traefik --> FaucetBot
    Traefik --> GraphiQL

    %% App interactions
    Dango <--> CometBFT
    Dango --> Postgres
    Dango --> ClickHouse
    PointsBot --> Dango

    %% P2P consensus
    CometBFT <-.->|WireGuard P2P| CometBFT

    %% Observability flows
    Dango -->|OTLP| Tempo
    Promtail -->|logs| Loki
    NodeExp -->|metrics| Prometheus
    Dango -->|metrics| Prometheus
    CometBFT -->|metrics| Prometheus

    %% Monitoring
    Grafana --> Prometheus
    Grafana --> Loki
    Grafana --> Tempo
    Prometheus --> Alertmanager --> Discord

    %% Styles
    classDef testnetOnly stroke-dasharray:5 5,fill:#fef3c7,stroke:#d97706
    classDef app fill:#d1fae5,stroke:#059669
    classDef infra fill:#dbeafe,stroke:#2563eb
    classDef monitoring fill:#ede9fe,stroke:#7c3aed
    classDef external fill:#f3f4f6,stroke:#6b7280

    class FaucetBot testnetOnly
    class Dango,CometBFT,Frontend,GraphiQL,PointsBot app
    class Traefik,cloudflared,Postgres,ClickHouse,Promtail,NodeExp infra
    class Grafana,Prometheus,Loki,Tempo,Alertmanager monitoring
    class User,CF,Discord external
```

### Data Flows

- **Metrics**: app containers expose metrics on WireGuard IPs -> Prometheus scrapes (ovh3) -> Grafana dashboards
- **Logs**: Docker containers -> Promtail (per server) -> Loki (ovh3) -> Grafana
- **Traces**: dango emits OTLP -> Tempo (ovh3) -> Grafana
- **Alerts**: Prometheus -> Alertmanager -> Discord webhooks

## How to use

### Add a new user

- Add the username in `group_vars/all/main.yml` in the `ssh_users` section

- Add the public key(s) in `roles/users/files/authorized_keys/<username>/<key_name>.pub`. The single-file form `roles/users/files/authorized_keys/<name>.pub` is reserved for the keys used by Ansible itself (`deploy.pub`, `debian.pub`)

- Run `just provision-users`

### Install a new server

See [`NEW_SERVER_SETUP.md`](NEW_SERVER_SETUP.md).

### Setup SOPS Secrets

Install the local tools:

```bash
brew install sops age age-plugin-yubikey
```

Each teammate needs an age identity file that points to their YubiKey slot.
Keep that identity file local; only the public `age1yubikey1...` recipient is
committed in `.sops.yaml`.

Check the local SOPS setup:

```bash
just sops-check
just sops-audit
```

Make also sure you have ssh-agent and added your key with ssh-add before
running ansible-playbook, else you'll get `Permission denied (publickey)`.

You must rerun `ssh-add` after you rebooted.

Debian-only secrets live in `vaults/debian/root_vault.sops.json` and
`vaults/debian/debian_key.sops`. They are intentionally encrypted to the
root/debian recipients only; deploy CI is not a root/debian recipient in phase
1.

#### Root access

No one should need debian/sudo access to the servers, this is a critical
access. Only root/debian SOPS recipients can decrypt the debian SSH key and
root variables.

Make also sure you have ssh-agent and added your key with ssh-add before
running ansible-playbook, else you'll get `Permission denied (publickey)`.

You must rerun `ssh-add` after you rebooted.

### Using the deploy key

The private key is encrypted in `vaults/deploy/deploy_key.sops`, load it
directly into ssh-agent without writing to disk:

`just add-deploy-key`

Notes:

- Ensure `ssh-agent` is running in your shell (`eval $(ssh-agent -s)` if needed).

### Manual Cosign Verification

Run this after deployments if you need to validate an image digest manually:

```bash
cosign verify \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp "https://github.com/left-curve/left-curve/.github/workflows/rust.yml@refs/heads/main" \
  ghcr.io/left-curve/left-curve/dango@sha256:<digest>
```

### Cloudflare tunnels and load balancers

Those are deployed differently for testnet/devnet and PR review apps.

#### PR review apps

When `cloudflare_tunnel_enabled` is set to true, the review app docker compose
includes a cloudflare tunnel container. Then we create CNAME for each service,
to that specific "PR-container" tunnel.

The cloudflared container has a config, routing to containers based on host.

[user] -> (( cloudflare )) -> [cloudflared PR container] -> [destination PR container]

#### devnet/testnet

Each host has a specific cloudflare tunnel name with the hostname. A
`cloudflare` docker network is created. The host running traefik includes the
cloudflare network.

We add a new `traefik` config file, so :80 and :443 and connected to the PR
containers. It routes those port to proper container services based on
hostname.

The cloudflared container has a config, routing to containers based on host.

[user] -> (( cloudflare )) -> [cloudflared system container] -> [system traefik] -> [destination container]
