# New Server Setup

Step-by-step runbook for commissioning a new server into the fleet (mainnet, testnet, or devnet). Run each step in order from your local machine, verifying the outcome before proceeding.

## Prerequisites

Before you start:

- **SOPS access**: complete [Setup SOPS Secrets](README.md#setup-sops-secrets). Your local age identity must be able to decrypt the routine deploy files and, for provisioning, the root/debian files.
- **Keys in agent**: `eval $(ssh-agent -s) && just add-deploy-key && just add-debian-key`. Ansible authenticates outbound using whatever ssh-agent has loaded; both keys are required.
- **Plan the host's identity**: pick four values up front and write them down — you'll use them in steps 1 and 7.
  - `hostname` (e.g. `hetzner5`) — next free slot.
  - `wireguard_ip` (e.g. `10.99.0.14`) — next free `10.99.0.X`. Existing assignments are listed in the inventory table at the top of [`README.md`](README.md).
  - `reboot_time` (e.g. `"12:30"`) — UTC time at which unattended-upgrades may reboot the host after a kernel update. Stagger across the fleet so a kernel security patch doesn't take multiple validators offline simultaneously. Existing slots: see `host_vars/*.yml` (`grep reboot_time host_vars/*.yml`). For a host that's _replacing_ another (e.g. hetzner5 → inter1), inherit the predecessor's slot — the old host will be decommissioned before the new one starts validating, so they won't both reboot together.
  - `tailscale_ip` — assigned automatically in step 6; you'll fill it in afterwards.

All commands assume your working directory is `deploy/`. Ansible runs through `uv` (the project's Python environment manager), so every `ansible-playbook` invocation is prefixed `uv run`.

## Step 0. SSD health check

A healthy datacenter-class NVMe SSD with power-loss protection (PLP) is required for validator performance — consumer drives have caused multi-second consensus stalls under fsync load. Before configuring anything, verify the drives that shipped with the server by following [`SSD_HEALTH_CHECK.md`](SSD_HEALTH_CHECK.md).

Push back to the vendor (or refuse the server) if any of the following are true:

- A drive's model is not on the datacenter-class list.
- `media_errors` is non-zero on any drive.
- One drive shows tens of thousands of `power_on_hours` while the other is fresh (asymmetric — likely a returned drive shipped in a "new" order).

Only proceed to step 1 once SMART is clean across all drives.

## Step 1. Add the new server to inventory

Edit `inventory` and add the host's **public IP** under the appropriate group (typically `[full-app]`, plus `[traefik]`, `[clickhouse]`, `[db]`, `[cloudflared]`, etc., to match its peers):

```ini
[full-app]
...
$PUBLIC_IP    # hetzner5
```

Then create `host_vars/$PUBLIC_IP.yml` with the planned hostname, wireguard IP, and reboot time. Leave `tailscale_ip` out for now — you don't know it yet.

```yaml
---
hostname: "hetzner5"
wireguard_ip: "10.99.0.14"
reboot_time: "12:30"
```

The filename uses the public IP because that's the current `inventory_hostname`; you'll rename it in step 7 once tailscale assigns an IP.

**Verify**:

```bash
grep "$PUBLIC_IP" inventory
cat host_vars/$PUBLIC_IP.yml
```

Expected: the inventory line is present under the right group, and the host_vars file contains the planned `hostname` and `wireguard_ip`.

## Step 2. Add debian public key to root user

A fresh Hetzner box has only `root`. Hetzner's `installimage` puts your personal SSH key on `/root/.ssh/authorized_keys` (whichever you provided at order time). Push the team's debian-deploy public key onto root's authorized_keys so subsequent ansible runs (which use the team-shared debian private key from your agent) can authenticate as root.

```bash
ssh-copy-id -f -i roles/users/files/authorized_keys/debian.pub root@$PUBLIC_IP
```

The `-f` (force) flag is required because `ssh-copy-id` normally wants the matching private key alongside the `.pub` file, and the debian private key only exists encrypted in the vault — never on disk.

If `ssh-copy-id` isn't available, paste the contents of `roles/users/files/authorized_keys/debian.pub` into `/root/.ssh/authorized_keys` manually after `ssh root@$PUBLIC_IP`.

**Verify**:

```bash
ssh root@$PUBLIC_IP 'grep debian-deploy ~/.ssh/authorized_keys'
```

Expected: prints the line `ssh-ed25519 AAAA... debian-deploy`.

## Step 3. Create debian user

`init-debian-user.yml` connects as root, creates the `debian` user with NOPASSWD sudo, drops the team's debian pubkey onto its authorized_keys, replaces Hetzner's apt mirrors with Debian's, and refreshes the apt cache. Pass `-e ansible_user=root` to override the default `remote_user: debian` (which doesn't exist yet).

```bash
uv run ansible-playbook init-debian-user.yml --limit $PUBLIC_IP -e ansible_user=root
```

**Verify**:

```bash
ssh debian@$PUBLIC_IP 'whoami; sudo -n whoami'
```

Expected output:

```plain
debian
root
```

The first line confirms the `debian` user exists and the team key authenticates. The second confirms NOPASSWD sudo. (`sudo -n` fails if a password would be needed.)

## Step 4. Lock root password

The `debian` user now handles all root operations via sudo, so root's password is no longer needed for normal access. Lock it. Pubkey-based root SSH (used by ansible until step 9 disables root login entirely) is unaffected.

```bash
ssh root@$PUBLIC_IP 'passwd -l root'
```

Use `passwd -l` (lock), not `passwd -d` (delete). `-d` removes the hash entirely, which can allow passwordless login if PAM is misconfigured; `-l` puts a `!` prefix on the hash so no password can ever match while leaving the account otherwise functional.

**Verify**:

```bash
ssh debian@$PUBLIC_IP 'sudo passwd -S root'
```

Expected: a line beginning `root L ...` (the `L` = locked).

## Step 5. Run common provisioning

`common.yml` sets the system hostname (using `hostname` from host_vars), generates the en_US.UTF-8 locale, installs base packages (curl, fail2ban, ufw, htop, vim, mosh, etc.), and installs/configures `unattended-upgrades` (templating `/etc/apt/apt.conf.d/50unattended-upgrades` with `reboot_time` from host_vars). Running this _before_ tailscale ensures the host registers on the tailnet under its proper name rather than a generic Hetzner default.

```bash
uv run ansible-playbook common.yml --limit $PUBLIC_IP
```

**Verify**:

```bash
# hostname + base packages
ssh debian@$PUBLIC_IP 'hostname; dpkg -l fail2ban ufw mosh htop | grep ^ii'
# expected: first line is the planned hostname (e.g. hetzner5);
# next four lines all start with `ii` (Debian: installed/configured).

# unattended-upgrades reboot window
ssh debian@$PUBLIC_IP 'grep Reboot-Time /etc/apt/apt.conf.d/50unattended-upgrades'
# expected: Unattended-Upgrade::Automatic-Reboot-Time "<reboot_time>";
# matching the value you set in host_vars.
```

## Step 6. Set up tailscale

```bash
uv run ansible-playbook tailscale.yml --limit $PUBLIC_IP
```

This installs the tailscale package and joins the host to the team's tailnet using the auth key from the vault. The host registers under the hostname set in step 5.

**Verify** (run from your laptop, which must already be on the tailnet):

```bash
tailscale status | grep <hostname>
```

Expected: prints a line like `100.x.y.z   <hostname>   ...   linux   active`. **Record the `100.x.y.z` value — that's the tailscale IP you'll use in step 7 onwards.**

Also confirm reachability:

```bash
ssh debian@$TAILSCALE_IP hostname
```

Expected: prints `<hostname>`.

> [!TIP]
> **Common error: stale tailscale auth key**
>
> If the playbook fails on "Install | Bring Tailscale Up" (output suppressed by `no_log: true`), check the daemon's journal:
>
> ```bash
> ssh debian@$PUBLIC_IP 'sudo journalctl -u tailscaled -n 30'
> ```
>
> A repeating "Received error: invalid key: API key does not exist" means the `tailscale_authkey` in `group_vars/all/vault.sops.json` has been invalidated by Tailscale's control plane (revoked, or expired and pruned). Existing servers are unaffected — auth keys are only consulted during first-time registration.
>
> Fix: generate a fresh auth key at <https://login.tailscale.com/admin/settings/keys>. Then update the vault:
>
> ```bash
> just sops-edit-secrets
> ```
>
> Replace the `tailscale_authkey` value, save and exit. The file is re-encrypted automatically. Re-run step 6.

## Step 7. Swap public IP for tailscale IP in inventory

The host is now reachable over tailscale, so switch all references from public to tailscale IP. This isolates further ansible runs to the private mesh.

1. Edit `inventory`: replace `$PUBLIC_IP` with `$TAILSCALE_IP` everywhere it appears (typically once per group it belongs to).
2. Rename `host_vars/$PUBLIC_IP.yml` → `host_vars/$TAILSCALE_IP.yml`.
3. Add the `tailscale_ip:` line to that file:

```yaml
---
tailscale_ip: "$TAILSCALE_IP"
hostname: "hetzner5"
wireguard_ip: "10.99.0.14"
reboot_time: "12:30"
```

(Match the structure of any existing host_vars file, e.g. `host_vars/100.126.8.2.yml` for hetzner1.)

**Verify**:

```bash
grep -c "$PUBLIC_IP" inventory     # should print 0
grep "$TAILSCALE_IP" inventory     # should print the new entries
cat host_vars/$TAILSCALE_IP.yml    # should show all three fields
ls host_vars/$PUBLIC_IP.yml        # should fail: "No such file or directory"
```

## Step 8. Enable WireGuard

> [!WARNING]
> **Running this briefly disrupts existing services on the entire fleet.**
>
> Adding a new peer changes `wg0.conf` on every host, which fires the role's `restart wireguard` handler → `wg-quick@wg0` cycles the interface in parallel across the fleet. During the flap (typically a few seconds):
>
> - CometBFT P2P drops and reconnects — brief pause in block propagation, possibly a slow round.
> - Hyperlane validator / relayer messaging blips.
> - Prometheus scrapes from ovh3 miss a few intervals.
>
> CometBFT recovers automatically and mainnet keeps producing blocks. Watch Grafana for a few minutes after to confirm nothing sticks. To defer, the new host can sit in the inventory without WG until you're ready — nothing in steps 1–7 depends on WG being established.

WireGuard is a full mesh — every host needs every other host's public key and IP. The wireguard role generates keys on first run, distributes them across the fleet, and writes a `wg0.conf` listing all peers. Run **without `--limit`** so existing nodes pick up the new peer (and vice versa).

```bash
uv run ansible-playbook wireguard.yml
```

**Verify**:

```bash
ssh debian@$TAILSCALE_IP 'sudo wg show'
```

Expected: shows the new host's public key, listening port, and a `peer:` block for every other server in the fleet, each with an `endpoint` and `allowed ips: 10.99.0.X/32`.

Connectivity check from any existing peer (e.g. hetzner1 at `100.126.8.2`):

```bash
ssh debian@100.126.8.2 'ping -c 2 -W 2 <wireguard IP of new host>'
```

Expected: 2 packets transmitted, 2 received, 0% loss.

## Step 9. Run master playbook

`playbook.yml` applies the `common`, `users`, `docker`, `swap`, `node-exporter`, and `promtail` roles — installing Docker, creating the `deploy` user with the team's deploy key, pushing all `ssh_users` pubkeys, hardening sshd (disable root login, password auth, challenge-response), configuring a 32 GB swap file, and starting the metrics/log shippers.

```bash
uv run ansible-playbook playbook.yml --limit $TAILSCALE_IP
```

**Verify**:

```bash
# deploy user exists with the team key
ssh deploy@$TAILSCALE_IP whoami
# expected: deploy

# docker is installed and running
ssh debian@$TAILSCALE_IP 'docker --version && systemctl is-active docker'
# expected: Docker version 2x.y.z, ..., and "active"

# root SSH login is disabled
ssh -o BatchMode=yes root@$TAILSCALE_IP 2>&1 | grep -i 'permission denied'
# expected: a "Permission denied (publickey)" line

# node-exporter and promtail containers are running
ssh deploy@$TAILSCALE_IP 'docker ps --format "{{.Names}}" | sort'
# expected: includes node-exporter and promtail

# swap file exists and is mounted (the role creates /swapfile at 32 GiB)
ssh debian@$TAILSCALE_IP 'grep /swapfile /proc/swaps'
# expected: /swapfile  file  33554428  0  -2
# (total swap from `free -h | grep Swap` may exceed 32 GiB if Hetzner
#  installimage also created a swap partition — that's fine.)
```

## Step 10. Add to Prometheus scrape config

After step 9 the host is running three infra metric sources installed by `playbook.yml`: `node-exporter` (port 9100), `cadvisor` (port 9101), and `promtail` (port 9080). Prometheus on ovh3 can already resolve the new hostname over WireGuard — `roles/prometheus/templates/docker-compose.yml` builds the container's `extra_hosts:` dynamically from inventory + host_vars, so the inventory change in step 7 is enough — but its scrape target lists are hardcoded in `roles/prometheus/templates/prometheus.yml`. Append the new hostname to each of the three relevant jobs:

```yaml
- job_name: 'node-exporter'
  static_configs:
    - targets:
      - ...
      - '<hostname>:9100'   # add

- job_name: 'cadvisor'
  static_configs:
    - targets:
      - ...
      - '<hostname>:9101'   # add

- job_name: 'promtail'
  static_configs:
    - ...
    - targets: ['<hostname>:9080']   # add
```

Then redeploy the monitoring stack on ovh3 (this only restarts the prometheus container — no impact on the rest of the fleet):

```bash
just deploy-monitoring
```

**Verify**: open Prometheus at `http://ovh3:9091/targets` and confirm the three new entries each reach `state: UP` within a scrape interval or two (`scrape_interval: 30s`). In Grafana, `<hostname>` should now appear in the host selectors of the Node Exporter, cAdvisor, and Logs dashboards.

Application-level scrapes — `traefik`, `clickhouse`, `postgres`, `dango`, `cometbft`, `hyperlane-validator`, `perp-liquidator` — need their own entries in `prometheus.yml` once you deploy those services to the host. See the existing entries in the same file for the format and labels.

## Next steps

The host is now a fully provisioned member of the fleet but has no application services yet. From here:

- **Application deploy**: run the relevant deploy recipe from `Justfile` (`just deploy-mainnet`, `just deploy-testnet`, etc.). For mainnet, also update the `--limit` list in the recipe to include the new tailscale IP.
- **Hyperlane** (if applicable): see the `deploy-hyperlane-*` recipes — these need the new validator's KMS key set up in `group_vars/hyperlane/vault.sops.json`.
