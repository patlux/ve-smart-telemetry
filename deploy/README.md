# deploy — victron-collector deployment assets (Raspberry Pi OS)

Production-oriented, **non-deploying** assets for the Victron VE.Smart BLE
collector on a Raspberry Pi Zero W (ARMv6 hard-float). Nothing in this
directory executes on any host by itself; the scripts run on the Pi only
when an operator invokes them with root.

```text
deploy/
  systemd/
    victron-collector.service        systemd unit (hardened, incremental)
    victron-collector.example.toml   reference config (no credentials)
  scripts/
    lib.sh                   shared helpers (ARMv6 verify, reachability, ...)
    install-release.sh       first install / reinstall (idempotent, --dry-run)
    update-release.sh        binary swap with auto-rollback on failure
    rollback-release.sh      restore previous binaries from snapshots
    verify-installation.sh   read-only full health report
    exposure-check.sh        read-only, network-only reachability probe (any host)
    pair-device.sh           one-time local bluetoothctl pairing (PIN in bluetoothctl)
  tests/
    run-tests.sh             non-mutating shell-level tests (mock curl/ss/systemctl)
```

Run the shell-level tests on any host (no root, no Pi, no network):

```bash
deploy/tests/run-tests.sh
```

Documentation lives in `../docs/`:

- `configuration.md` — every config key, defaults, validation rules
- `operations.md` — install/update/rollback/verify, pairing, logs, troubleshooting
- `metrics.md` — Prometheus metric contract and Grafana usage
- `security.md` — exposure statement, reachability, auth assumptions, hardening

## Quickstart (on the Pi)

First install never starts the service before pairing — the collector needs
a bonded device first. Supported flow:

```bash
# 1. copy the release binaries and this deploy/ directory onto the Pi
sudo deploy/scripts/install-release.sh --binary ./victron-collector --cli ./victron-cli
#    (first install: installs everything, leaves the service disabled/stopped)
# 2. pair the Victron device once (PIN is typed inside bluetoothctl)
deploy/scripts/pair-device.sh
# 3. enable/start, then verify everything (read-only)
sudo systemctl enable --now victron-collector
sudo deploy/scripts/verify-installation.sh --strict
```

## Contract

- The service exposes **no inbound port** and only sends outbound HTTP to the
  VictoriaMetrics URL in `/etc/victron-collector/config.toml`. "No listener"
  is an application contract verified at runtime (`verify-installation.sh`
  checks listening TCP/UDP sockets as root), not a property the hardening
  directives enforce by themselves.
- The binaries are **ARMv6 hard-float** (`arm-unknown-linux-gnueabihf`); all
  scripts reject ARMv7+ builds. No assumption of ARMv7 is made anywhere.
- Pairing is a one-time local `bluetoothctl` step; no PIN literal exists in
  this repository, and no script captures a PIN into the shell.
- `exposure-check.sh` is the only intended way to assert the endpoint is
  blocked from an off-tailnet host (`--unreachable URL`); it is read-only
  and network-only and runs on any host.
