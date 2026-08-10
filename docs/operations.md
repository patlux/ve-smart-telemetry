# Operations — victron-collector on Raspberry Pi OS

Operational runbook for the Victron VE.Smart BLE collector. Target host: a
Raspberry Pi **Zero W (ARMv6, hard-float)** running Raspberry Pi OS
(Debian). Nothing here assumes ARMv7.

## Lifecycle

```bash
systemctl status victron-collector     # state + recent journal
systemctl restart victron-collector    # apply config/logging changes
systemctl stop victron-collector
systemctl disable --now victron-collector   # remove from boot
journalctl -u victron-collector -f     # follow logs (journald)
journalctl -u victron-collector --since "1 hour ago"
```

## Logging and BLE diagnostics

The default `RUST_LOG=info` is intentionally concise: startup, one result per
acquisition cycle, spool-drain summaries, shutdown, and warnings/errors. Cycle
results carry `cycle_id`, `device`, `elapsed_ms`, `energy_kind`, and `delivery`;
failures additionally carry the failed `phase`, a stable `error_kind`, and a
bounded `operation` such as `notification`. Sensitive values are deliberately absent:
no BLE address, PIN/bond material, raw payload, unrestricted D-Bus message, or
VictoriaMetrics URL is logged.

Enable detailed BLE timing temporarily with a systemd drop-in:

```bash
sudo mkdir -p /etc/systemd/system/victron-collector.service.d
printf '%s\n' \
  '[Service]' \
  'Environment=RUST_LOG=info,victron_bluez=debug,victron_collector::adapters::ble=debug,victron_collector::watchdog=debug,victron_service::cycle=debug' \
  | sudo tee /etc/systemd/system/victron-collector.service.d/10-logging.conf
sudo systemctl daemon-reload
sudo systemctl restart victron-collector
journalctl -u victron-collector --since "10 minutes ago" -o short-iso
journalctl -u victron-collector --since "10 minutes ago" -o cat \
  | grep -E 'acquisition cycle|phase=requesting|operation=(discovery-scan|get-values-response)'
```

Useful debug fields:

| Field | Meaning |
|---|---|
| `cycle_id` | monotonic process-local acquisition number |
| `phase` / `previous_phase` | state-machine phase and transition timing |
| `operation` | bounded BlueZ/protocol operation label |
| `elapsed_ms` / `timeout_ms` | observed duration and configured deadline |
| `attempt` / `error_class` | connect retry number and redacted failure class |
| `known_devices` / `unique_devices_seen` | discovery population counts, never addresses |
| `notifications` | total notifications while waiting for `getValues` |
| `control_notifications`, `data_notifications`, `last_data_notifications` | notification-source counts |
| `completed_payloads`, `unrelated_payloads` | reassembled responses and responses ignored by instance/value correlation |
| `clear_buffer_notifications` | peer buffer-reset controls observed while waiting |
| `response_bytes` / `payload_bytes` | bounded payload sizes; byte content is never logged |

For the noisiest per-payload correlation events, temporarily replace the
same drop-in with a trace directive only for the BLE adapter:

```bash
printf '%s\n' \
  '[Service]' \
  'Environment=RUST_LOG=info,victron_bluez=debug,victron_collector::adapters::ble=trace,victron_collector::watchdog=debug,victron_service::cycle=debug' \
  | sudo tee /etc/systemd/system/victron-collector.service.d/10-logging.conf
sudo systemctl daemon-reload
sudo systemctl restart victron-collector
```

Remove temporary verbosity after capture so the Pi Zero W does not retain
high-volume logs:

```bash
sudo rm -f /etc/systemd/system/victron-collector.service.d/10-logging.conf
sudo systemctl daemon-reload
sudo systemctl restart victron-collector
```

The unit restarts on failure with a 30 s delay and gives up after 5
failures in 600 s (no crash loop). It also uses `Type=notify` with a 180 s
progress watchdog: READY is sent only after full initialization, and the
collector feeds the watchdog only while its current phase remains inside its
explicit progress budget. A stuck process therefore becomes a watchdog
failure, is killed immediately with `WatchdogSignal=SIGKILL`, and is restarted
by `Restart=on-failure`. SQLite atomicity protects durable acquisition state
across this deliberately abrupt recovery. Reset start-limit counters
with `systemctl reset-failed victron-collector`.

## Install

Requires the two release binaries (`victron-collector`, optional
`victron-cli`) cross-compiled for `arm-unknown-linux-gnueabihf`. Copy
`deploy/` and the binaries onto the Pi, then as root:

```bash
sudo deploy/scripts/install-release.sh --binary ./victron-collector --cli ./victron-cli
```

A **first install** (no unit file present yet) never enables or starts the
service automatically: the collector needs a bonded Victron device before
it can do useful work, so install must not depend on a healthy paired
collector. The supported first-install flow is:

```bash
sudo deploy/scripts/install-release.sh --binary ./victron-collector --cli ./victron-cli
# installs binaries, user, config, unit; leaves the service disabled/stopped

deploy/scripts/pair-device.sh                    # one-time local pairing
sudo systemctl enable --now victron-collector    # enable at boot + start now
sudo deploy/scripts/verify-installation.sh --strict
```

A **reinstall** (unit already present) keeps the old default: enable and
start the service and require it to stay active, unless `--no-enable` /
`--no-start` are given. `--no-start` means "do not enable or start" and
implies `--no-enable`. Idempotent; `--dry-run` prints every mutation (and
needs no root); an existing `/etc/victron-collector/config.toml` is
preserved (use `--force-config` to overwrite). The script verifies the
ARMv6 identity/linkage of both binaries, creates the dedicated user, and
**validates the effective config against the candidate binary**
(`--config PATH --check-config`) before the unit is installed or the
service is enabled/started — an invalid config never starts the service
and never lands on disk.

## Pairing (one-time, local)

The PIN is entered **inside bluetoothctl's own prompt** — never into the
shell, never into a script, never into config. Run:

```bash
deploy/scripts/pair-device.sh
```

It preflights the Bluetooth stack and hands over to an interactive
`bluetoothctl`. Inside: `power on`, `agent on`, `default-agent`,
`scan on`, then `pair <MAC>`, `trust <MAC>`, `connect <MAC>`, `scan off`,
`exit`. When bluetoothctl prints its own `Enter PIN code:` prompt, type the
PIN there.

If a shell script must ever capture hidden input (not needed here, and not
recommended), use zsh-safe syntax — never `read -p` (zsh treats `-p` as
coprocess input):

```zsh
IFS= read -r -s 'PIN?PIN: ' && printf '\n'
```

Re-pairing after device replacement: inside bluetoothctl run
`remove <MAC>`, then pair again with the steps above.

## Update

```bash
sudo deploy/scripts/update-release.sh --binary ./victron-collector-new
# with a CLI change:  sudo deploy/scripts/update-release.sh --binary ./victron-collector-new --cli ./victron-cli-new
```

Snapshots the current collector **and** the installed `victron-cli` (newest
3 kept, collector and CLI snapshots share a timestamp), installs the new
binaries, restarts, and verifies service state, BLE adapter, database, and
VictoriaMetrics reachability. Before anything is changed, the existing
config is validated against the **new** binary (`--config PATH
--check-config`): if the new release rejects the current config, the
update fails immediately while the old binary is still in place and the
service is untouched (no rollback needed). If verification fails after the
swap it **rolls back automatically** to that snapshot (CLI included when
the snapshot has one). A CLI-only change is applied too — the "nothing to
do" early exit only fires when every provided binary is already in place
and identical. `--no-verify` skips the deeper checks; `--dry-run` prints
everything.

## Rollback

```bash
sudo deploy/scripts/rollback-release.sh            # newest snapshot
sudo deploy/scripts/rollback-release.sh --index 2  # second-newest
sudo deploy/scripts/rollback-release.sh --list
```

Restores the chosen collector snapshot and, when a CLI snapshot with the
same timestamp exists, the CLI snapshot too; otherwise `victron-cli` is
left as-is (diagnostic tool, never removed). Before the restart, the
restored binary must accept the existing config (`--config PATH
--check-config`); if it rejects it, the rollback fails and the service is
left stopped rather than restarted into a crash loop. Backups live in
`/usr/local/lib/victron-collector/backups/` (root-only).

## Verification

```bash
sudo deploy/scripts/verify-installation.sh          # full read-only report
sudo deploy/scripts/verify-installation.sh --strict # warnings count as failures
```

Checks: host arch, ARMv6 binary identity/linkage, `--version`, CLI,
accounts, directories and permissions, unit syntax (`systemd-analyze
verify`) and enable/active state, config (forbidden credential keys,
binary parse validation, `storage.path` confinement, and a **strict
plaintext HTTP URL check**: only `http://host:port/absolute/path`, no
https/userinfo/query/fragment/whitespace, valid port; the host is then
classified numerically against tailnet CGNAT `100.64.0.0/10`), SQLite
database, BLE adapter visibility (`bluetoothctl list`, rfkill),
VictoriaMetrics reachability (empty POST — no data written), and
**absence of listening TCP/UDP sockets owned by the collector** (root
required for process attribution; skipped with a warning otherwise). Exit
codes: `0` pass, `1` failure, `2` warnings only.

### Reachability — intended vs unintended paths

Intended path (on the Pi, tailnet up):

```bash
curl -sS -o /dev/null -w '%{http_code}\n' --noproxy '*' --max-time 8 \
  -X POST --data-binary '' http://100.64.0.2:8429/api/v1/import/prometheus
# expect 2xx/4xx (any HTTP response) — NOT 000
```

`--noproxy '*'` forces a direct connection so the probe measures
reachability of the endpoint itself, not of an HTTP(S)_PROXY/ALL_PROXY
proxy.

Unintended path (a machine **not** on the tailnet, e.g. a laptop with the
tailnet off — the endpoint must be unreachable): use the read-only,
network-only probe, which performs only the reachability assertion and
runs on any host without root:

```bash
# from the external (off-tailnet) host:
deploy/scripts/exposure-check.sh \
  --unreachable http://100.64.0.2:8429/api/v1/import/prometheus
# expect [PASS] unreachable as expected ... — if you get an HTTP response,
# the VM endpoint is reachable from outside the tailnet: stop and fix the
# firewall/route
```

`verify-installation.sh` is not suitable as the external probe: it
verifies the local installation (binaries, unit, service, BLE, database)
first, which requires root on the Pi.

### No inbound listener

```bash
sudo systemctl show -p MainPID --value victron-collector   # e.g. 1234
sudo ss -lntup | grep "pid=1234"                           # expect NO output
```

The check covers listening **TCP and UDP** sockets and attributes them to
the collector process **by PID**, not by process name: Linux task comm is
truncated to 15 characters, so `victron-collector` (17 characters) shows up
as `victron-collecto` in `ss -p` output and a name grep can miss it.
`verify-installation.sh` reads the MainPID from `systemctl show` and greps
`pid=<PID>` in the root `ss -lntup` output. If the service is inactive
(MainPID 0) the check is skipped with a warning. `ss -p` only shows
processes as root; without root the check is skipped with a warning
instead of claiming certainty. verify-installation.sh performs this check
(root recommended).

## BLE adapter operations

```bash
systemctl status bluetooth
bluetoothctl list                # controller visible?
rfkill list bluetooth            # soft/hard blocked?
sudo rfkill unblock bluetooth    # if soft-blocked
sudo systemctl restart bluetooth # bluetoothd restart; collector reconnects
```

BlueZ owns the bond (`/var/lib/bluetooth/<adapter>/<device>/`, root-only).
The collector reconnects on its own cycle; no manual action after a
bluetoothd restart.

## Database / spool maintenance

- Bounded by config: `maximum_spool_batches`, `maximum_spool_age_days`.
- After a long VictoriaMetrics outage the spool replays oldest-first; watch
  `victron_spool_batches` and `victron_spool_oldest_age_seconds`.
- To inspect safely:
  `sudo sqlite3 /var/lib/victron-collector/state.sqlite3
  'PRAGMA integrity_check;'` (install `sqlite3` if absent).
- Back up state before manual surgery:
  `sudo cp /var/lib/victron-collector/state.sqlite3 /root/`.

## Reboot recovery

```bash
sudo reboot
# after boot:
systemctl status victron-collector
journalctl -u victron-collector -b
sudo deploy/scripts/verify-installation.sh --strict
```

The unit is enabled and starts after `dbus`, `bluetooth`, and
`network-online`. Expect the first BLE cycle to occur shortly after boot.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| unit `failed`, restarting | config parse error | `sudo /usr/local/bin/victron-collector --config /etc/victron-collector/config.toml --check-config`; fix config |
| crash loop (StartLimit hit) | recurring start failure | `journalctl -u victron-collector -n 100`, fix, `systemctl reset-failed` |
| `Result=watchdog` / watchdog timeout | process or collector phase made no bounded progress | inspect the last `STATUS=` phase and journal; systemd restarts automatically unless the start limit is hit |
| `victron_ble_up 0` / connect failures | device out of range, VictronConnect holding the connection, bond lost | check range; close VictronConnect; re-pair via `pair-device.sh` |
| no controller in `bluetoothctl list` | bluetoothd down / rfkill | `systemctl restart bluetooth`, `rfkill unblock bluetooth` |
| VM unreachable (000) | tailnet down on Pi, wrong URL, firewall | `tailscale status` on the Pi; verify URL; see reachability section |
| spool growing | VM down | deliver or prune; check `victron_spool_batches` |
| stale data at night | expected zero-power night | suppress with daylight-aware alerts, not by raising intervals |

## Hardening — incremental enablement

The unit ships a baseline block safe with BlueZ over the system D-Bus
socket, and a commented **extended** block. Enable **one line at a time**,
then after each change:

```bash
sudo systemctl daemon-reload && sudo systemctl restart victron-collector
sudo deploy/scripts/verify-installation.sh --strict
/usr/local/bin/victron-cli read-once --device <alias>   # live BLE read still works?
```

Recommended order: `IPAddressAllow=100.64.0.0/10` **plus**
`IPAddressDeny=any` (tailnet-only egress, deny-by-default — per
systemd.resource-control(5) the allow list is evaluated before the deny
list, so Allow has precedence over Deny and the textual order of the lines
does not matter; the pair also blocks loopback and DNS unless allowed
explicitly; see docs/security.md), then `PrivateDevices=yes`, then
`ProtectProc=invisible` / `ProcSubset=pid`, then
`SystemCallArchitectures=native`. If a line breaks BLE, remove it and
re-verify; D-Bus access itself is preserved by the baseline (AF_UNIX to
`/run/dbus/system_bus_socket`, no user namespaces, no `PrivateUsers=yes`
— that option would break the system bus and is deliberately absent).

## Operational alerts (enable after one week of stable data)

- no successful sample for 10 min during daylight
- BLE connect failures above threshold
- spool growing for 15 min
- native energy counter decreasing
- impossible voltage/current/power
- zero power during high solar radiation (warning)

Use the existing Open-Meteo daylight series to suppress night-time noise.
