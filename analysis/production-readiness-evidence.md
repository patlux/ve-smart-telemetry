# Production-readiness evidence

Captured 2026-08-10 against the workspace and original Pi Zero W (`ssh pizero`).

## Confirmed

| Requirement | Evidence |
|---|---|
| Workspace integrity | No `/tmp` path dependencies; only root `Cargo.lock`; no `target/`; `git diff --check` passed. |
| File size | Production Rust files ≤500 lines. Deployment config helpers were split from `lib.sh`; production scripts are ≤500 lines. |
| Linux quality gates | Format, workspace tests, Clippy with `-D warnings`, and Rustdoc with `-D warnings` passed in the release container. |
| Deployment gates | `deploy/tests/run-tests.sh`: **118 passed, 0 failed**. Shell syntax and Dockerfile build checks passed. |
| ARMv6 artifact | Installed collector and CLI pass the fail-closed verifier: ARM EABI5, `/lib/ld-linux-armhf.so.3`, ARMv6, Thumb-1, VFPv2, hard-float VFP registers, max referenced GLIBC 2.30 ≤ 2.31. |
| Concrete implementation | Real BlueZ, VE.Smart protocol/domain, SQLite, Prometheus renderer and VictoriaMetrics delivery adapters are wired. |
| Read-only protocol | No public settings, PIN/PUK, bond-key or DFU operations. Intermediate chunks use Data; final/single chunks use LastData. |
| Pairing | `Solar Charger` (`AA:BB:CC:DD:EE:FF`) is Paired, Bonded and Trusted. The PIN was entered only through a transient BlueZ agent and is not stored in the workspace. |
| Live reads | Ten successful reconnect/read cycles produced ten unique acquisition/spool timestamps. Failed attempts produced no acquisition rows. Later live values included battery 25.55 V, PV 44.26 V and `ble_up=1`. |
| SQLite semantics | `PRAGMA integrity_check` is `ok`; acquisition persists energy state, last-success and spool atomically/idempotently. Restart retained state. |
| Failure spool policy | While VM was unreachable, persisted batches were retried and batches reaching attempt 5 were dropped without being counted delivered. |
| Successful spool replay | After Tailscale connectivity, two persisted batches drained oldest-first within 16 seconds: rows 2 → 0 and `spool.delivered_total` 0 → 2; integrity remained `ok`. The following live acquisition was delivered directly, raising delivered total to 3 with the spool still empty. |
| VictoriaMetrics import | Empty POST returned HTTP 204. Collector's real Prometheus batches were accepted by `/api/v1/import/prometheus`. |
| VictoriaMetrics query | Historical queries returned real battery, PV and BLE values; series API listed 14 bounded `device="solar-charger"` series. Later dashboard validation showed PV power 288 W, battery 25.68 V / 10.97 A and successful BLE state. |
| Grafana dashboard | Provisioned `Energy / Victron Solar Charger` from `nomad/jobs/monitoring/grafana/dashboards/energy/victron-solar-charger.json`. All PromQL targets passed against VictoriaMetrics; desktop and 430 px mobile renders were inspected with real data. |
| Installation | Collector and CLI are installed under `/usr/local/bin`; config and DB use documented paths. Candidate and rollback config compatibility is checked before mutation. |
| systemd | Unit verifies, is enabled and active. `Type=notify` reports READY after initialization and feeds a 180 s progress-aware watchdog. A controlled `SIGSTOP` test with a temporary 15 s watchdog produced `Result=watchdog`, immediate SIGKILL, and an automatic restart with a new PID; the temporary override was removed and production returned to 180 s. Normal heartbeat timestamps advance every 30 s. `TimeoutStopSec=150` still covers graceful operator stops; a restart during BLE work completed gracefully in 12 seconds. |
| Rollback/update | Real rollback changed installed hash; final update restored the verified production binary and active service. |
| Collector listener contract | PID-based `sudo ss -lntup` returned no TCP or UDP listener for the collector. |
| Config/security | Credential keys are forbidden. Storage path is section-scoped/constrained. Shell and Rust URL policies reject HTTPS, userinfo, query/fragment and request-line injection. |
| Tailscale ARMv6/resource fit | Official Raspbian armhf Tailscale 1.102.2 runs on the Pi Zero W. Steady RSS ~45 MiB, 311 MiB RAM available, zero swap, no daemon restart. |
| Tailnet policy | Node `pizero` has `100.64.0.3`; `accept-routes=false`, DNS acceptance false, Tailscale SSH false, no exit node, no advertised routes/services, shields-up false as requested. |
| Tailnet reachability | Another tailnet device successfully pinged `100.64.0.3` and connected to SSH TCP 22. SSH allows public keys only; password/root login are disabled. |
| Direct VM route | Pi routes `100.64.0.2` over `tailscale0`; Tailscale ping reaches `metrics-node` directly via `192.0.2.10:41641`. |

## Network exposure

Approved change: install Tailscale and make the Pi reachable from permitted tailnet peers.

Observed listeners:

- `sshd`: TCP 22 on all host interfaces, including Tailscale; public-key authentication only, no root/password login.
- `tailscaled`: UDP 41641 on host interfaces for encrypted peer transport and dynamic TCP peer/proxy listeners bound only to the Pi's Tailscale IPv4/IPv6 addresses.
- `victron-collector`: no incoming TCP or UDP listener.

No exit-node advertisement, subnet routing, Tailscale SSH, Serve, Funnel, DNS takeover or router port-forward was enabled. Internet reachability of the existing globally-bound SSH service remains dependent on the router/IPv6 firewall and is separate from the tailnet change.

## Remaining application limitation

The protocol's native lifetime-yield VREG remains unconfirmed; `0xED8E` is deliberately not mapped. The collector exports a truthful local integrated yield only from confirmed PV power and does not invent missing health or energy values.

The historical replayed batch reported `victron_spool_batches=2`, correctly describing its acquisition-time state. A later successful acquisition will emit the current zero-spool gauge.

## Current Pi snapshot

```text
pizero Tailscale IPv4: 100.64.0.3
Tailscale: enabled, active, 1.102.2
Tailscale RSS: ~45 MiB
RAM available: ~311 MiB
Swap used: 0
collector: enabled, active, Type=notify
watchdog: 180 s, heartbeat observed, controlled hang recovery verified
collector listeners: none
bond: Paired=yes, Bonded=yes, Trusted=yes
SQLite integrity: ok
spool rows: 0
spool delivered total: 3
VictoriaMetrics empty probe: HTTP 204
```

## Verdict

The ARMv6 BLE collector, ten-read hardware loop, SQLite recovery and atomic spool, real replay into VictoriaMetrics, provisioned Grafana dashboard, systemd lifecycle, deployment rollback/update, tailnet transport and no-collector-listener contract are demonstrated on real hardware.
