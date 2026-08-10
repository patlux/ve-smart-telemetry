# Configuration — victron-collector

The collector reads one TOML file: `/etc/victron-collector/config.toml`
(root-owned, mode 0644). The service **never writes** to `/etc`; the systemd
unit hardens it to write only under `/var/lib/victron-collector`.

A commented reference file is deployed as
`deploy/systemd/victron-collector.example.toml`.

## File ownership and permissions

| Path | Owner | Mode | Purpose |
|---|---|---|---|
| `/etc/victron-collector/config.toml` | root:root | 0644 | read-only configuration |
| `/var/lib/victron-collector/state.sqlite3` | victron-collector:victron-collector | 0600 | SQLite state + spool |
| `/var/lib/victron-collector/` | victron-collector:victron-collector | 0750 | writable state directory |
| `/usr/local/bin/victron-collector` | root:root | 0755 | daemon binary |

The config is world-readable by design so it can be inspected without root;
it must therefore never contain secrets (see below).

## Reference

### `[device]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | — | stable Prometheus label value (`device="..."`); keep it short, lowercase, and identical across config/dashboards/alerts |
| `bluez_alias` | string | — | BlueZ identity to resolve (as shown by `bluetoothctl devices`); the MAC is **not** needed in the config |
| `instance` | integer | — | VE.Smart instance from VictronConnect (e.g. `3`) |
| `adapter` | string | `"hci0"` | Bluetooth controller to use |

### `[poll]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `active_interval_seconds` | integer | — | cycle interval while solar power is active |
| `idle_interval_seconds` | integer | — | cycle interval while idle/standby |
| `response_timeout_seconds` | integer | — | per-request BLE timeout before retry/backoff |
| `phase_timeout_seconds` | integer | — | per-phase BLE protocol timeout (discovery/negotiation/subscribe/request) |
| `maximum_energy_gap_seconds` | integer | — | max accepted gap for the local energy-integration fallback; larger gaps are never silently bridged |
| `spool_claim_ttl_seconds` | integer | — | spool claim TTL: a batch claimed for delivery is released back to the spool after this long if delivery never completes |
| `spool_max_attempts` | integer | — | maximum delivery attempts per batch before it is dropped |
| `backoff_base_seconds` | integer | — | exponential backoff base between failed cycles |
| `backoff_factor` | integer | — | backoff multiplier per consecutive failure |
| `backoff_cap_seconds` | integer | — | backoff cap |
| `active_window_utc_hours` | `[start, end]` | `[0, 24]` | UTC hour window in which the *active* interval applies (`0 <= start < end <= 24`); optional |

### `[victoria_metrics]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `url` | string | — | import endpoint, e.g. `http://100.64.0.2:8429/api/v1/import/prometheus` |
| `request_timeout_seconds` | integer | — | HTTP request timeout |

The URL is the **only** outbound destination the service uses. Today it is
plain HTTP on the tailnet CGNAT range without TLS or auth — internal
network only. If auth is added later, the collector's request code must be
extended; this file stays credential-free.

### `[storage]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | `/var/lib/victron-collector/state.sqlite3` | SQLite database path |
| `maximum_spool_batches` | integer | 10000 | bounded undelivered-batch queue |
| `maximum_spool_age_days` | integer | 7 | prune batches older than this |

`path` must stay under `/var/lib/victron-collector` — the systemd unit
allows writes only there (`ReadWritePaths=`). Moving it outside requires
also changing the unit.

## Validation

`verify-installation.sh` and `check_config()` in `lib.sh` enforce:

- no `pin` / `puk` / `passcode` / `password` / `secret` / bond-key keys
- binary parse validation: the collector CLI is `--config PATH
  --check-config`; when a collector binary is available it MUST accept the
  config (nonzero exit = hard failure). Textual sanity (a `[device]` and
  `[victoria_metrics]` section with a non-empty `url`) is used only when no
  binary is available at all
- `storage.path` inside `/var/lib/victron-collector`
- a strict plaintext HTTP URL: only `http://host:port/absolute/path` —
  https, userinfo, query/fragment, whitespace/control characters,
  missing/zero/invalid ports, and unsafe/non-absolute paths are rejected
- the URL host inside tailnet CGNAT `100.64.0.0/10` (numeric IPv4
  inclusion; warn otherwise)

`install-release.sh` validates the effective config against the candidate
binary before the unit is installed or the service is enabled/started;
`update-release.sh` validates the existing config against the new binary
before swapping; `rollback-release.sh` validates it against the restored
binary before restarting. An invalid config therefore never starts the
service.

The collector binary itself reports a parse error and exits nonzero on a
bad config; the unit will not stay up (crash loop), which the
`StartLimitBurst`/`StartLimitIntervalSec` settings bound.

## Editing

```bash
sudo cp /etc/victron-collector/config.toml /etc/victron-collector/config.toml.bak
sudoedit /etc/victron-collector/config.toml
sudo systemctl restart victron-collector
sudo deploy/scripts/verify-installation.sh --strict
```

Never restart the service as a test for every key: verify the file with
`sudo /usr/local/bin/victron-collector --config /etc/victron-collector/config.toml --check-config`
first if your build supports it.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `RUST_LOG` | `info` | tracing filter; `debug` adds BLE/D-Bus noise, use sparingly |

`RUST_LOG` is set in the unit; override per-command with
`sudo systemctl restart victron-collector` after adding a drop-in:

```bash
sudo mkdir -p /etc/systemd/system/victron-collector.service.d
printf '[Service]\nEnvironment=RUST_LOG=warn\n' \
  | sudo tee /etc/systemd/system/victron-collector.service.d/10-logging.conf
sudo systemctl daemon-reload && sudo systemctl restart victron-collector
```

## Non-secrets

The Victron **PIN is never stored**: not in this file, not in the unit, not
in any script. It exists only transiently inside bluetoothctl's prompt
during the one-time pairing. Bond material lives in BlueZ storage
(`/var/lib/bluetooth/`, root-only).
