# Node Migration

Step-by-step runbook for moving an existing mainnet validator role from one server to another. The **source** server is currently running the chain (validating, hosting traefik/cloudflared/postgres/clickhouse, running its hyperlane validator instance, etc.). The **target** server has finished [`NEW_SERVER_SETUP.md`](NEW_SERVER_SETUP.md) — i.e. it's on the tailnet, in the WireGuard mesh, has the deploy user and Docker, and is running node-exporter + promtail, but has no application services yet.

The objective is to move all chain data, indexer state, and the validator identity from source to target with minimal disruption to mainnet. The other 3 mainnet validators are not touched during the migration — they keep running with their current config. Target inherits source's cometbft p2p identity (via the rsynced `node_key.json`), so once target comes online the rest of the fleet talks to it without any peer-list reconfiguration.

## Prerequisites

- **Source's tailscale IP** and **target's tailscale IP**.
- **Source and target hostnames** (e.g. `inter1`, `hetzner5`).
- **SOPS access** and keys in agent (`just add-deploy-key && just add-debian-key`).
- **Disk space on target**: no less than the sum of source's `~/mainnet/`, `~/psql/data/`, and `~/clickhouse/data/`.
- **Time window**: budget ~60–90 minutes. Source is offline from step 1 onward; the chain runs on 3/4 quorum during that window, with source's proposer slot timing out every ~4 blocks.

All commands assume `deploy/` is your working directory. Ansible runs through `uv`.

## Step 0. Define variables

```bash
SOURCE_IP=100.89.7.33    # source server's tailscale IP (in this example, inter1)
TARGET_IP=100.72.62.100  # target server's tailscale IP (in this example, hetzner5)
```

```bash
DEPLOY=$(ssh deploy@$SOURCE_IP 'jq -r .current_deployment ~/deployments/mainnet.json')
DATA_DIR=$(ssh deploy@$SOURCE_IP "dirname \$(grep '^COMETBFT_DIRECTORY=' ~/deployments/$DEPLOY/.env | cut -d= -f2)")

echo "$DEPLOY"    # orchestration timestamp, e.g. 20260430121659
echo "$DATA_DIR"  # data dir parent containing both cometbft/ and dango/, e.g. /home/deploy/mainnet/20260105173049
```

## Step 1. Stop all services on source

Source is no longer needed for consensus from this point — it just hosts data we're about to copy off. We do this before the inventory edit (step 2) so we can use `stop-services.yml` for the dango+cometbft stack: that play also pauses the Kuma uptime monitor, which silences alerts during the migration window. Postgres + ClickHouse have no equivalent stop playbook, so they get stopped via direct `docker compose`.

```bash
# Stop Hyperlane validator
just stop-hyperlane mainnet validator $SOURCE_IP

# Stop dango, cometbft, dango-frontend, etc.
uv run ansible-playbook stop-services.yml \
  -e dango_network=mainnet \
  --limit $SOURCE_IP

# Stop postgres and clickhouse
ssh deploy@$SOURCE_IP 'cd ~/psql && docker compose stop'
ssh deploy@$SOURCE_IP 'cd ~/clickhouse && docker compose stop'
```

**Verify**:

```bash
ssh deploy@$SOURCE_IP 'docker ps --format "{{.Names}}"'
```

Expected:

```bash
# Before stopping, should look like this:
$ ssh deploy@$SOURCE_IP 'docker ps --format "{{.Names}}"'
promtail
cadvisor
node-exporter
20260430121659-perp-liquidator-1
20260430121659-dango-frontend-1
20260430121659-cometbft-1
20260430121659-graphiql-1
20260430121659-dango-1
mainnet-validator-1
postgres-exporter
postgres
traefik-traefik-1
clickhouse
dozzle-dozzle-1
cloudflared

# After:
$ ssh deploy@$SOURCE_IP 'docker ps --format "{{.Names}}"'
promtail
cadvisor
node-exporter
traefik-traefik-1
dozzle-dozzle-1
cloudflared
```

The rest of the fleet is now on 3/4 validator quorum. Check Grafana: blocks still being produced, just slower because the proposer slot of `<source>` will time out every ~4 blocks.

> [!WARNING]
> After this point, if any of the remaining three (3) consensus nodes goes down, the chain will halt.

## Step 2. Update inventory, host_vars, and Justfile

Swap source out, target in. From here on, source is reached only by direct SSH; ansible no longer knows about it. The shell variables `$SOURCE_IP` and `$TARGET_IP` from your session refer to the actual IPs you'll be substituting in these files.

1. Edit `inventory`: in every group that lists source, replace source's IP with target's IP. If target is already present in that group (e.g. `[full-app]`, since `NEW_SERVER_SETUP.md` adds the new host there during commissioning), just remove source's line. **Position matters in `[perp-liquidator-mainnet]`** — instance index is derived from list position, so insert target where source was. **Don't skip `[withdrawal-approver-<network>]`** — it holds a single host, and a missed edit leaves the network with zero withdrawal guardians: no deploy step warns about it, user withdrawals just stop being approved. Also make sure source's stopped withdrawal-approver container is removed (step 11's wipe): it carries the guardian key and its `restart: always` policy revives it on a reboot of the source host.

2. Create `host_vars/<target IP>.yml` (named after target's actual IP) with the mainnet flags (mirror `host_vars/<source IP>.yml`):

   ```yaml
   dango_networks:
     - mainnet
   cloudflare_lb_enabled: true
   ```

3. Delete `host_vars/<source IP>.yml`.

4. Edit `Justfile`: replace source's IP with target's IP everywhere it appears. This includes:

   - The four mainnet recipes' `--limit` lists (`deploy-mainnet`, `stop-mainnet`, `restart-mainnet`, `remove-deploy-lock-mainnet`).
   - The hardcoded validator-1 IP in `deploy-hyperlane-mainnet` and `stop-hyperlane-mainnet-validators`.
   - `# Usage:` example comments above `stop-hyperlane` and `start-dango-httpd`.

   The verify below catches any stragglers.

5. Swap traefik per-host template symlinks. The traefik role looks up `docker-compose-{{ hostname }}.yml` and `traefik-{{ hostname }}.yml`; existing dango validator hosts use symlinks to the shared `docker-compose-dango.yml` / `traefik-dango.yml`. Add target's pair and remove source's. From `deploy/`:

   ```bash
   cd roles/traefik/templates && \
     ln -s docker-compose-dango.yml docker-compose-<target hostname>.yml && \
     ln -s traefik-dango.yml traefik-<target hostname>.yml && \
     rm docker-compose-<source hostname>.yml traefik-<source hostname>.yml && \
     cd -
   ```

**Verify**:

```bash
grep -c "$SOURCE_IP" inventory Justfile host_vars/  # should print 0 each
grep "$TARGET_IP" inventory  # should print one line per group
```

## Step 3. Set up temporary SSH from source to target

We use `debian` for the rsync (not `deploy`) because we need root on both ends: source's cometbft `config/` and `data/` are 0700 root-owned (cometbft's docker container ran as root), and target's freshly-init'd ones from step 4 are the same. `debian` has passwordless `sudo`; `deploy` doesn't. Generate a one-shot ed25519 keypair as root on source (via `sudo` from `debian@source`), placed under `/root/.ssh/migrate_key` — all subsequent transfers run through `sudo rsync`, whose internal `ssh` subprocess runs as root and looks in root's home for both the key and the `known_hosts` entry, so keeping the key there avoids cross-user permission and host-key-verification issues. The comment `node-migrate-temp` makes it easy to remove from target's `authorized_keys` in step 11.

```bash
ssh debian@$SOURCE_IP 'sudo ssh-keygen -t ed25519 -N "" -f /root/.ssh/migrate_key -C node-migrate-temp && sudo cat /root/.ssh/migrate_key.pub'

# copy the printed pubkey, then append it to debian's authorized_keys on target:
ssh debian@$TARGET_IP 'cat >> ~/.ssh/authorized_keys' <<< '<paste pubkey>'
```

**Verify** (run as root via `sudo` — this is also what populates `/root/.ssh/known_hosts` with target's host key, so every later `sudo rsync` finds it):

```bash
ssh debian@$SOURCE_IP "sudo ssh -i /root/.ssh/migrate_key -o StrictHostKeyChecking=accept-new debian@$TARGET_IP hostname"
```

Expected: prints target's hostname.

## Step 4. Initialize target's cometbft dir

Creates a non-validator `priv_validator_key.json` + a fresh `priv_validator_state.json` (height 0) — both of which the subsequent rsync deliberately leaves in place via excludes.

`node_key.json` (the p2p identity) **is** overwritten by the rsync: target inherits source's network identity so the other validators' `addrbook` keeps trusting the same `cometbft_node_id`.

```bash
ssh deploy@$TARGET_IP "mkdir -p $DATA_DIR/cometbft && \
  docker run --rm \
  -v $DATA_DIR/cometbft:/root/.cometbft \
  ghcr.io/left-curve/left-curve/cometbft:v0.39.3 \
  cometbft init --home /root/.cometbft"
```

After this, target has:

- `config/node_key.json` — target's, will be overwritten by rsync below.
- `config/priv_validator_key.json` — target's, **non-validator** key. Stays through rsync; gets overwritten by source's actual validator key in step 8.
- `config/genesis.json`, `config/config.toml` — target's defaults; rsync overwrites both.
- `data/priv_validator_state.json` — `{"height":"0",...}`. Stays through rsync; gets overwritten in step 8.

**Verify** (the docker container ran `cometbft init` as root, so `config/` and `data/` end up root-owned 0700 — `deploy` can't `ls` them; we go in via `debian` + `sudo`):

```bash
ssh debian@$TARGET_IP "sudo ls $DATA_DIR/cometbft/config/ $DATA_DIR/cometbft/data/"
```

Expected: `config/` lists `config.toml`, `genesis.json`, `node_key.json`, `priv_validator_key.json`; `data/` lists `priv_validator_state.json`. Example:

```bash
$ ssh debian@$TARGET_IP "sudo ls $DATA_DIR/cometbft/config/ $DATA_DIR/cometbft/data/"
/home/deploy/mainnet/20260105173049/cometbft/config/:
config.toml
genesis.json
node_key.json
priv_validator_key.json

/home/deploy/mainnet/20260105173049/cometbft/data/:
priv_validator_state.json
```

## Step 5. Rsync data from source to target

Six transfers run via `sudo rsync` on `debian@source`.

- The rsync of mainnet data deliberately excludes `dango/indexer/blocks/` — hundreds of GB of small files used only for old-block index queries. The chain comes back online without it. We transfer it as the sixth rsync below; it'll likely still be running through later steps, which is fine — the chain doesn't need it to validate. Just don't run step 11 (wipe) until that rsync has completed.

- The four large ones (mainnet data, postgres, clickhouse, and the indexer block archive) are `nohup`'d so they survive an SSH disconnect — the rsyncs continue on the server, output streams to per-transfer log files in `/home/debian/`, and you can reconnect any time and `tail -f` to see live progress. The two small ones (the orchestration dir and the `mainnet.json` pointer) run in the foreground since they finish in seconds.

- `--rsync-path='sudo rsync'` makes the receiving rsync also run as root, so it can write into target's root-owned cometbft dirs (and source's `-a` preserves root ownership of the rsynced files). Paths are absolute (`/home/deploy/...` rather than `~`) because sudo resets `HOME` to root's, and the SSH user on target is debian — neither expands to `/home/deploy`.

- `--mkpath` lets rsync create missing parent directories on target. Without it, transfers fail because `/home/deploy/deployments/`, `/home/deploy/mainnet/<deploy>/`, `/home/deploy/psql/`, and `/home/deploy/clickhouse/` don't exist on a fresh host yet — they're only created during the infra deploys in step 6, which run _after_ this rsync.

- `--info=progress2` shows a per-rsync progress line: `<bytes> <pct>%  <speed>  <eta>  (xfr#N, ir-chk=K/M)` — `xfr#N` is files transferred so far, `ir-chk=K/M` is remaining-to-check / total-discovered-so-far (the total grows as rsync recurses). For an accurate total upfront, add `--no-inc-recursive` (rsync enumerates everything before transferring, which adds wall time on large dirs). With non-TTY stdout (we're writing to a log), rsync emits one line per progress update instead of `\r`-overwriting, so the log scrolls cleanly.

```bash
# 1. mainnet.json pointer.
ssh debian@$SOURCE_IP "sudo rsync -aH --mkpath \
  -e 'ssh -i /root/.ssh/migrate_key' \
  --rsync-path='sudo rsync' \
  /home/deploy/deployments/mainnet.json debian@$TARGET_IP:/home/deploy/deployments/mainnet.json"

# 2. orchestration dir for the current deployment (compose file + .env).
ssh debian@$SOURCE_IP "sudo rsync -aHv --mkpath --delete \
  -e 'ssh -i /root/.ssh/migrate_key' \
  --rsync-path='sudo rsync' \
  /home/deploy/deployments/$DEPLOY/ debian@$TARGET_IP:/home/deploy/deployments/$DEPLOY/"

# 3. mainnet data: cometbft state + dango app state. Excludes the indexer block archive
# and the validator-identity files.
ssh debian@$SOURCE_IP "
  nohup sudo rsync -aH --mkpath --info=progress2 --delete \
    -e 'ssh -i /root/.ssh/migrate_key -o StrictHostKeyChecking=accept-new' \
    --rsync-path='sudo rsync' \
    --exclude=cometbft/config/priv_validator_key.json \
    --exclude=cometbft/data/priv_validator_state.json \
    --exclude=dango/indexer/blocks/ \
    $DATA_DIR/ debian@$TARGET_IP:$DATA_DIR/ \
    > /home/debian/rsync-mainnet.log 2>&1 </dev/null &
"

# 4. dango indexer block archive — hundreds of GB of small files. Slow due to per-file
# rsync overhead. Does not block restarting the chain — can be done at a later time.
ssh debian@$SOURCE_IP "
  nohup sudo rsync -aH --mkpath --info=progress2 --delete \
    -e 'ssh -i /root/.ssh/migrate_key' \
    --rsync-path='sudo rsync' \
    $DATA_DIR/dango/indexer/blocks/ debian@$TARGET_IP:$DATA_DIR/dango/indexer/blocks/ \
    > /home/debian/rsync-blocks.log 2>&1 </dev/null &
"

# 5. postgres: full dir (data/ + config/ + docker-compose.yml). Ansible's db.yml needs
# the deploy-managed config/ and compose file alongside the data dir.
ssh debian@$SOURCE_IP "
  nohup sudo rsync -aH --mkpath --info=progress2 --delete \
    -e 'ssh -i /root/.ssh/migrate_key' \
    --rsync-path='sudo rsync' \
    /home/deploy/psql/ debian@$TARGET_IP:/home/deploy/psql/ \
    > /home/debian/rsync-psql.log 2>&1 </dev/null &
"

# 6. clickhouse: full dir (data/ + config/ + docker-compose.yml). Same reasoning as #5.
ssh debian@$SOURCE_IP "
  nohup sudo rsync -aH --mkpath --info=progress2 --delete \
    -e 'ssh -i /root/.ssh/migrate_key' \
    --rsync-path='sudo rsync' \
    /home/deploy/clickhouse/ debian@$TARGET_IP:/home/deploy/clickhouse/ \
    > /home/debian/rsync-clickhouse.log 2>&1 </dev/null &
"
```

Sanity-check that the `nohup`'d jobs actually launched:

```bash
ssh debian@$SOURCE_IP "pgrep -af 'rsync.*sudo' && echo OK || echo NOTHING RUNNING"
```

If `NOTHING RUNNING`, peek at the log files for errors (e.g. `cat /home/debian/rsync-mainnet.log`).

Watch live progress:

```bash
ssh debian@$SOURCE_IP "tail -f /home/debian/rsync-mainnet.log"
# Ctrl+C on tail just exits tail; rsync keeps running on the server.
```

Or all four interleaved:

```bash
ssh debian@$SOURCE_IP "tail -f /home/debian/rsync-{mainnet,psql,clickhouse,blocks}.log"
```

Wait for completion:

```bash
ssh debian@$SOURCE_IP "pgrep -af 'rsync.*sudo' || echo all rsync jobs done"
```

To abort cleanly (sends SIGINT so rsync flushes buffers and writes a final log line):

```bash
ssh debian@$SOURCE_IP "sudo pkill -INT -f 'rsync.*sudo'"
```

If a rsync gets interrupted (network blip, server restart, etc.), re-run the same launch command — rsync's size+mtime check skips already-transferred files.

**Verify**:

1. Confirm that source's data is on target, and that target's freshly-init'd validator-identity files survived:

    ```bash
    ssh debian@$TARGET_IP "sudo ls $DATA_DIR/cometbft/config/ $DATA_DIR/cometbft/data/"
    ```

    Expected: `config/` includes `node_key.json`, `priv_validator_key.json`, `genesis.json`, `app.toml`, `config.toml`, `addrbook.json`. `data/` includes `priv_validator_state.json` plus the cometbft block-store files (`blockstore.db`, `state.db`, `cs.wal/`, etc.).

2. Confirm the target's `node_key.json` matches source's. We use plain `docker run` (not `docker compose run`) because the compose service has an `entrypoint: sh -c '... cometbft start'` that ignores any appended command — `cometbft show-node-id` would silently turn into `cometbft start`. `docker run` against the image directly only inherits the image's `CMD` (which `cometbft show-node-id` cleanly overrides).

   ```bash
   ssh deploy@$SOURCE_IP "docker run --rm -v $DATA_DIR/cometbft:/root/.cometbft ghcr.io/left-curve/left-curve/cometbft:v0.39.3 cometbft show-node-id --home /root/.cometbft"
   ssh deploy@$TARGET_IP "docker run --rm -v $DATA_DIR/cometbft:/root/.cometbft ghcr.io/left-curve/left-curve/cometbft:v0.39.3 cometbft show-node-id --home /root/.cometbft"
   ```

   Expected: same id printed by both.

3. Confirm content integrity by re-running each transfer with `--checksum --dry-run --itemize-changes`. Rsync reads every byte on both ends, recomputes content checksums, and prints any mismatches without transferring. Empty output (or just a single `.d..t...... ./` line for the directory mtime) means everything matches. Any drift shows as `>fcst......` lines — the `c` in column 4 is the checksum-mismatch indicator. Budget 10–30 minutes total; the cometbft blockstore + postgres + clickhouse easily exceed 50 GB combined.

   ```bash
   ssh debian@$SOURCE_IP "sudo rsync -aH --checksum --dry-run --itemize-changes \
     -e 'ssh -i /root/.ssh/migrate_key' \
     --rsync-path='sudo rsync' \
     /home/deploy/deployments/$DEPLOY/ debian@$TARGET_IP:/home/deploy/deployments/$DEPLOY/"

   ssh debian@$SOURCE_IP "sudo rsync -aH --checksum --dry-run --itemize-changes \
     -e 'ssh -i /root/.ssh/migrate_key' \
     --rsync-path='sudo rsync' \
     --exclude=cometbft/config/priv_validator_key.json \
     --exclude=cometbft/data/priv_validator_state.json \
     --exclude=dango/indexer/blocks/ \
     $DATA_DIR/ debian@$TARGET_IP:$DATA_DIR/"

   ssh debian@$SOURCE_IP "sudo rsync -aH --checksum --dry-run --itemize-changes \
     -e 'ssh -i /root/.ssh/migrate_key' \
     --rsync-path='sudo rsync' \
     /home/deploy/psql/ debian@$TARGET_IP:/home/deploy/psql/"

   ssh debian@$SOURCE_IP "sudo rsync -aH --checksum --dry-run --itemize-changes \
     -e 'ssh -i /root/.ssh/migrate_key' \
     --rsync-path='sudo rsync' \
     /home/deploy/clickhouse/ debian@$TARGET_IP:/home/deploy/clickhouse/"
   ```

**Fix parent directory ownership**:

The rsyncs of `/home/deploy/psql/` and `/home/deploy/clickhouse/` (transfers #5 and #6) sync the parent dir's own metadata too, so those end up `deploy:deploy` automatically. But `/home/deploy/deployments/` and `/home/deploy/mainnet/` only got created by `--mkpath` as intermediate paths for transfers #1–#4 (which target subdirs, not these parents) — so they're left as root-owned. Step 7's full-app deploy will fail trying to create a new `~/deployments/<new-timestamp>/` under a root-owned parent. Chown them (non-recursive — leaves the timestamp subdirs alone, since cometbft expects its data files to stay root-owned):

```bash
ssh debian@$TARGET_IP "sudo chown deploy:deploy /home/deploy/deployments /home/deploy/mainnet"
```

## Step 6. Deploy infrastructure services on target

Postgres, ClickHouse, Traefik, Cloudflared, and Dozzle each have their own playbook. Run them all with `--limit $TARGET_IP` so the existing fleet hosts are left alone.

Postgres and ClickHouse start against the data dirs rsynced in step 5 — their entrypoints detect existing PG_VERSION / clickhouse data and skip initialization, so source's databases come up intact. The `db` and `clickhouse` roles only manage `~/psql/{config,docker-compose.yml,.env}` and `~/clickhouse/{config,docker-compose.yml,.env}`; they never touch `data/`. Traefik/Cloudflared/Dozzle have no rsynced state and start fresh.

```bash
uv run ansible-playbook db.yml          --limit $TARGET_IP
uv run ansible-playbook clickhouse.yml  --limit $TARGET_IP
uv run ansible-playbook traefik.yml     --limit $TARGET_IP
uv run ansible-playbook cloudflared.yml --limit $TARGET_IP
uv run ansible-playbook dozzle.yml      --limit $TARGET_IP
```

**Verify**:

```bash
ssh deploy@$TARGET_IP 'docker ps --format "{{.Names}}" | sort'
```

Expected: includes `postgres`, `postgres-exporter`, `clickhouse`, `traefik`, `dozzle`, and one `cloudflared-…` container. (The cloudflared container name varies by tunnel ID.)

_Postgres_ — confirm the rsynced dango database is present:

```bash
ssh deploy@$TARGET_IP "docker exec postgres psql -U postgres -lqt | grep dango_$(basename $DATA_DIR)"
```

Expected: lists `dango_<data-dir-timestamp>` (rsynced from source). The dango database name is keyed off the data-dir timestamp (`$(basename $DATA_DIR)`, e.g. `20260105173049`) — not the orchestration timestamp `$DEPLOY` — because dango's database lifecycle follows `DANGO_DIRECTORY` from the original deploy that created the data. If this prints nothing, postgres re-initialized instead of picking up the rsynced data — check that `~/psql/data/PG_VERSION` exists on target.

_ClickHouse_ — confirm the rsynced indexer database is present. The default user has a password set in the rsynced `users.xml`, sourced at template-time from `~/clickhouse/.env`'s `CLICKHOUSE_PASSWORD`; we re-read it inline so it never lands in your local shell:

```bash
ssh deploy@$TARGET_IP "docker exec clickhouse clickhouse-client --user default --password \"\$(grep '^CLICKHOUSE_PASSWORD=' ~/clickhouse/.env | cut -d= -f2-)\" --query \"SHOW DATABASES LIKE 'dango%'\""
```

Expected: prints multiple `dango_<timestamp>` rows. The active one is `dango_$(basename $DATA_DIR)` (matches the postgres dango DB — both follow `DANGO_DIRECTORY`); the rest are leftover databases from prior deploys, same as the postgres listing. If you get _no_ dango rows, clickhouse re-initialized instead of loading the rsynced data. If you get `AUTHENTICATION_FAILED`, the rsynced `users.xml` and the .env's password are out of sync — fall back to `docker inspect --format '{{.State.Health.Status}}' clickhouse` (should print `healthy`, since the healthcheck uses the same password and would tell you).

_Traefik_ — confirm the API is up and routers loaded from config:

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://$TARGET_IP:8888/ping
```

Expected: `200`. If `000` or connection refused, traefik isn't binding to the tailscale IP — check `docker logs traefik-traefik-1`.

_Cloudflared_ — confirm the tunnel registered with Cloudflare:

```bash
ssh deploy@$TARGET_IP "docker logs \$(docker ps --format '{{.Names}}' | grep cloudflared) 2>&1 | grep -iE 'registered tunnel connection' | tail -5"
```

Expected: recent (post-deploy timestamp) "Registered tunnel connection" lines. If the output is empty, the tunnel didn't authenticate — check the full logs and verify the `CLOUDFLARED_TOKEN` in `~/cloudflared/.env`.

_Dozzle_ — confirm the web UI responds:

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://$TARGET_IP:9090/
```

Expected: `200`.

## Step 7. First mainnet deploy on target

Run the full-app play scoped to target only, with `-e cometbft_peers=<the 3 healthy IPs>` so target's `persistent_peers` lists them by their actual node IDs. Target dials them; they accept inbound (cometbft's addrbook is non-strict) and PEX gossips target's identity through the cluster.

**Pin to the in-prod images.** `full-app.yml` defaults `dango_image_tag` and `frontend_image_tag` to the local checkout's `git rev-parse HEAD`. The other validators were deployed with `DANGO_TAG=latest` / `FRONTEND_TAG=latest`, but `latest` is mutable — by the time of a migration, it usually points at a newer commit, and the old image's manifest may have been removed from ghcr (`docker manifest inspect` returns `manifest unknown`). The reliable path is to copy the exact images from source's local docker cache — source ran the same version before step 1 stopped it, and its docker still has them.

1. Capture the digests from source. Source's containers are stopped, so use `docker ps -a` (not `docker ps`):

   ```bash
   DANGO_DIGEST=$(ssh deploy@$SOURCE_IP 'docker image inspect $(docker ps -a --filter name=dango-1 --format "{{.Image}}" | head -1) --format "{{ index .RepoDigests 0 }}"' | cut -d@ -f2)
   FRONTEND_DIGEST=$(ssh deploy@$SOURCE_IP 'docker image inspect $(docker ps -a --filter name=dango-frontend-1 --format "{{.Image}}" | head -1) --format "{{ index .RepoDigests 0 }}"' | cut -d@ -f2)
   echo "$DANGO_DIGEST"     # sha256:<64-char hex>
   echo "$FRONTEND_DIGEST"  # sha256:<64-char hex>
   ```

2. Cross-check with another running validator to confirm source wasn't stale relative to the cluster:

   ```bash
   for host in 100.126.8.2 100.66.234.16 100.76.197.30; do
     echo "===== $host =====" && ssh deploy@$host "sudo -u deploy bash -c 'cd ~/deployments && DEPLOY=\$(jq -r .current_deployment mainnet.json) && docker compose -p \$DEPLOY exec dango dango --version'"
   done
   ```

   The 40-char commit hash printed by each healthy validator's `dango --version` should be the same. (No automatic mapping from commit hash → image digest, but if the commits all match, source's digest is the right one to use.)

3. Save the source images and load them on target via the migrate_key from step 3. Bytes flow source → target directly over tailscale; nothing routes through your laptop. `sudo` because `debian` isn't in the docker group; `debian` because the migrate_key is registered for that user.

   ```bash
   ssh debian@$SOURCE_IP "sudo docker save ghcr.io/left-curve/left-curve/dango@$DANGO_DIGEST | sudo ssh -i /root/.ssh/migrate_key debian@$TARGET_IP 'sudo docker load'"
   ssh debian@$SOURCE_IP "sudo docker save ghcr.io/left-curve/left-curve/dango-frontend@$FRONTEND_DIGEST | sudo ssh -i /root/.ssh/migrate_key debian@$TARGET_IP 'sudo docker load'"
   ```

   Each prints `Loaded image ID: sha256:<digest>` matching the one we passed.

4. `docker save image@digest` strips the repo+tag binding, so on target the images land as `<untagged>` (note: newer docker hides untagged images from `docker images` without `-a`). Re-tag them with `:latest` so docker-compose can find them by name, using the 12-char IDs from step 1's digests:

   ```bash
   ssh debian@$TARGET_IP "sudo docker tag ${DANGO_DIGEST#sha256:} ghcr.io/left-curve/left-curve/dango:latest && sudo docker tag ${FRONTEND_DIGEST#sha256:} ghcr.io/left-curve/left-curve/dango-frontend:latest"
   ```

   (`${VAR#sha256:}` strips the prefix; what's left is the 64-char hex, of which docker accepts the leading 12 chars as a short-ID.) Verify:

   ```bash
   ssh deploy@$TARGET_IP "docker images | grep -E 'dango\b|dango-frontend'"
   ```

   Both should show TAG `latest`.

5. Run the deploy with four flags that together short-circuit every registry-side check (manifests for the in-prod images are gone from ghcr) and the post-deploy sync wait (target will be many hours behind):

   - `verify_signatures: false` — skips `docker manifest inspect` for cosign verification (would fail on a deleted manifest).
   - `pull_images: false` — skips the role's explicit `docker compose pull` step, which would also try to resolve digests against ghcr. The actual `docker compose up` task already runs with `pull: never`, so once the explicit pull is gated, no registry call is made.
   - `cosign_verified_images: {}` — forces the fact that `resolve_digests.yml` would otherwise build to be empty, which makes the `when: ... | length > 0` guard on the override-template task evaluate false → no `docker-compose.override.yml` is written → compose uses the base file with `image: ghcr.io/...:${DANGO_TAG}` (tag-based). Extra-vars beat `set_fact` in ansible's variable precedence, so this override sticks even though `resolve_digests.yml` runs.
   - `skip_cometbft_sync: true` — skips `check_cometbft_sync.yml` entirely (sync-wait + peer-count assert + indexer-lag assert). Target is starting from the chain height where source was stopped (step 1) and has hours of blocks to catch up; the role's hardcoded `retries: 30 × delay: 10s = 5 min` window won't be enough, and a timeout would trigger `full-app.yml`'s rescue block which stops the just-started stack. With the skip on, we verify sync manually in sub-step 6.

   With the first three set, the version target ends up running is determined entirely by what's tagged locally as `ghcr.io/.../dango:latest` on target — which we deliberately pinned to source's image-ID in sub-step 4.

   The flags must go inside the JSON `-e` blob (booleans + objects); the `-e key=value` form passes strings, and newer ansible refuses to evaluate string-typed values in `when:` conditionals.

   This depends on small role changes: `pull_images` requires gating the pull task in `roles/full-app/tasks/deploy_standalone.yml`; `skip_cometbft_sync` requires gating the `check_cometbft_sync.yml` import in `roles/full-app/tasks/main.yml` and the inline sync-wait task in `restart-services.yml`. All three gates use `when: not (<flag> | default(false) | bool)`.

   ```bash
   mkdir -p logs && uv run ansible-playbook full-app.yml \
     -e '{"traefik_enabled": true, "cometbft_generate_keys": true, "github_deployments_enabled": false, "expose_ports": false, "delete_postgres_database_at_merge": false, "delete_clickhouse_database_at_merge": false, "deploy_includes_postgres": false, "deploy_includes_clickhouse": false, "chain_id": "dango-1", "dango_network": "mainnet", "system_wide_directories": true, "deploy_env": "production", "verify_signatures": false, "pull_images": false, "cosign_verified_images": {}, "skip_cometbft_sync": true}' \
     -e dango_image_tag=latest \
     -e frontend_image_tag=latest \
     -e cometbft_peers=100.126.8.2,100.66.234.16,100.76.197.30 \
     --limit $TARGET_IP \
     2>&1 | tee logs/$(date -u +%Y%m%d%H%M%S)-deploy-target.log
   ```

6. Wait for target to finish catching up. The playbook completes immediately (no sync wait), but target's cometbft is still syncing — the chain only becomes safe to take over the validator role in step 8 once target has reached the current chain head. Poll periodically:

   ```bash
   ssh deploy@$TARGET_IP 'docker exec $(docker ps -q --filter label=service_name=cometbft) curl -s http://localhost:26657/status | jq ".result.sync_info | {catching_up, latest_block_height}"'
   ```

   Expected eventually: `catching_up: false` and `latest_block_height` matches the rest of the fleet (compare against any healthy validator's `/status`). Until then, target is just a non-validator full node syncing in.

**Verify** (post-sync, before moving to step 8):

```bash
ssh deploy@$TARGET_IP \
  'docker exec $(docker ps -q --filter label=service_name=cometbft | head -1) \
  curl -s http://localhost:26657/net_info' \
  | jq .result.n_peers
```

Expected: `3` (the three healthy mainnet validators).

## Step 8. Validator key + state handover

This is the slashable step — the validator key must exist on **exactly one** running cometbft. Sequence:

1. Move `priv_validator_key.json` AND `priv_validator_state.json` from source to target. Both are required: the state file records the last height/round/step the key signed; without it, target would reset to height 0 and could double-sign blocks source already signed.
2. Delete the key file on source so even if source is started later, it can't sign.

> [!WARNING]
> **Do not skip `priv_validator_state.json`.** Skipping it is the textbook double-sign mistake.

**Pre-checks**:

1. **Source's cometbft must NOT be running.** It was stopped in step 1, but verify — if both source and target have the validator key in memory at the same time, both could sign and the cluster will slash you.

   ```bash
   ssh deploy@$SOURCE_IP 'docker ps --format "{{.Names}}" | grep cometbft || echo "no cometbft on source"'
   ```

   Expected: `no cometbft on source`. If a cometbft container is listed, stop it before proceeding (`uv run ansible-playbook stop-services.yml -e dango_network=mainnet --limit $SOURCE_IP` — same as step 1).

2. **Target's cometbft can stay running** — no need to stop it. cometbft loads `priv_validator_key.json` only at startup, not on every block, so replacing the file on disk leaves target's running process operating with its in-memory non-validator key (which can't double-sign because that pubkey isn't in the active validator set). The actual switchover happens in step 9 when cometbft restarts and loads the new key. `priv_validator_state.json` is updated only when cometbft signs, and a non-validator never signs — so the rsync overwrite below has no race.

```bash
# 8a. priv_validator_key.json: source → target via the migrate_key from step 3 so the
# validator private key never transits through your laptop. sudo on both ends because the
# files are root-owned 0600. -a preserves the 0600 file mode.
ssh debian@$SOURCE_IP "sudo rsync -aH \
  -e 'ssh -i /root/.ssh/migrate_key' \
  --rsync-path='sudo rsync' \
  $DATA_DIR/cometbft/config/priv_validator_key.json \
  debian@$TARGET_IP:$DATA_DIR/cometbft/config/priv_validator_key.json"

# 8b. priv_validator_state.json: source → target.
ssh debian@$SOURCE_IP "sudo rsync -aH \
  -e 'ssh -i /root/.ssh/migrate_key' \
  --rsync-path='sudo rsync' \
  $DATA_DIR/cometbft/data/priv_validator_state.json \
  debian@$TARGET_IP:$DATA_DIR/cometbft/data/priv_validator_state.json"

# 8c. Delete the key on source (so source can never sign again, even if started).
ssh debian@$SOURCE_IP "sudo rm $DATA_DIR/cometbft/config/priv_validator_key.json"
```

> [!NOTE]
> Target's `$DATA_DIR/cometbft` is the same path as source's — target inherited source's `~/deployments/$DEPLOY/.env` via the step 5 rsync, so the cometbft data path encoded in `COMETBFT_DIRECTORY` resolves identically on both sides.

**Verify**:

```bash
ssh debian@$SOURCE_IP "sudo ls $DATA_DIR/cometbft/config/priv_validator_key.json" 2>&1
# expected: "No such file or directory"

ssh deploy@$TARGET_IP 'docker exec $(docker ps -q --filter label=service_name=cometbft | head -1) cometbft show-validator'
# expected: prints the public key your validator slot has historically used.
# Note: cometbft is still loaded with the non-validator key in memory at this point —
# show-validator reads from disk so it'll already show the handed-over pubkey.

# Confirm the pubkey is in the chain's active validator set. Query a healthy validator
# (hetzner1 here) so this cross-checks against what the rest of the cluster sees, not just
# target's own view.
TARGET_PUBKEY=$(ssh deploy@$TARGET_IP 'docker exec $(docker ps -q --filter label=service_name=cometbft | head -1) cometbft show-validator' | jq -r .value)

ssh deploy@100.126.8.2 'docker exec $(docker ps -q --filter label=service_name=cometbft | head -1) curl -s "http://localhost:26657/validators?per_page=100"' | jq -r '.result.validators[].pub_key.value' | grep -F "$TARGET_PUBKEY" && echo "✓ in validator set" || echo "✗ NOT in validator set"
# expected: ✓ in validator set
```

## Step 9. Restart target's cometbft and verify signing

CometBFT now has the new key on disk but needs to be restarted to load it. `just restart-mainnet`'s `--limit` is hardcoded for the full validator set, so we run the restart play directly to scope to target only.

```bash
uv run ansible-playbook restart-services.yml \
  -e dango_network=mainnet \
  --limit $TARGET_IP
```

**Verify** target is signing recent blocks. Query a peer (e.g. hetzner1 at `100.126.8.2`):

```bash
ssh deploy@100.126.8.2 \
  'docker exec $(docker ps -q --filter label=service_name=cometbft | head -1) \
  curl -s http://localhost:26657/block' \
  | jq '.result.block.last_commit.signatures[] | select(.block_id_flag == 2) | .validator_address'
```

Expected: target's validator address appears in the signatures list within ~10–30 seconds. Repeat the query a few times to confirm it consistently signs.

Also check Grafana's "validator missed blocks" panel — your slot should drop from 100% missed (during steps 1–8) back to ~0%.

## Step 10. Migrate hyperlane validator role to target

Source ran `mainnet-validator-1` (per its position in `[hyperlane]`). Stop it on source via direct `docker compose` — `just stop-hyperlane` won't work here because source is no longer in inventory. Then deploy on target via the parameterized just recipe (which targets by IP).

```bash
ssh deploy@$SOURCE_IP 'cd ~/hyperlane-agents/mainnet-validator-1 && docker compose down'
just deploy-hyperlane mainnet validator $TARGET_IP 1
```

The KMS key and dango signer secrets are vaulted by validator index, not by host, so target reuses the same identity — no secret rotation needed.

**Verify**:

```bash
ssh deploy@$TARGET_IP 'docker logs mainnet-validator-1 --tail 50 2>&1 | grep -i "checkpoint\|posted\|started"'
```

Expected: a "successfully posted" or "starting validator" line within ~1 minute of starting.

## Step 11. Wipe source

The host is no longer used by the fleet. Either repurpose it (re-run [`NEW_SERVER_SETUP.md`](NEW_SERVER_SETUP.md) from step 5 onwards after wiping) or hand it back to the vendor.

Before running this step, confirm step 5's blocks rsync (#4) has finished — `ssh debian@$SOURCE_IP "pgrep -af 'rsync.*blocks' || echo done"` should print `done`. Otherwise the wipe will tear out the blocks dir from under the still-running rsync.

```bash
# Stop residual systemd-managed compose stacks
ssh debian@$SOURCE_IP 'sudo systemctl disable --now \
  postgres-compose.service \
  clickhouse-compose.service \
  traefik-compose.service \
  cloudflared-compose.service' || true

# Wipe application and identity state
ssh debian@$SOURCE_IP 'sudo rm -rf /home/deploy/{mainnet,deployments,psql,clickhouse,traefik,hyperlane-agents} /root/.ssh/migrate_key /root/.ssh/migrate_key.pub'

# Remove the temporary migrate-key grant from target's authorized_keys (the corresponding
# private key on source is gone, but clean up the trust for hygiene).
ssh deploy@$TARGET_IP 'sed -i.bak "/node-migrate-temp$/d" ~/.ssh/authorized_keys && rm ~/.ssh/authorized_keys.bak'
```

Remove the host from tailscale (admin console: <https://login.tailscale.com/admin/machines> → select source → Remove). Re-running `wireguard.yml` after step 2's inventory edit already pulled source out of every other host's `wg0.conf`.

## Done

Target is the new mainnet validator in source's slot, taking over hyperlane validator-1 and source's perp-liquidator instance. Watch Grafana for ~24h to confirm: steady block-proposal rate, indexer lag near zero, no log spikes.
