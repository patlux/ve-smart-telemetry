# VE.Smart Telemetry

Read-only VE.Smart BLE telemetry collector for Linux and VictoriaMetrics.
Designed and verified for the original Raspberry Pi Zero W (ARMv6), with
BlueZ transport, durable SQLite state, outbound-only metric delivery, and a
progress-aware systemd watchdog.

> [!IMPORTANT]
> This is an independent interoperability project. It is not affiliated with,
> endorsed by, or supported by Victron Energy. VE.Smart, Victron and
> VictronConnect are trademarks of their respective owner.

## Capabilities

- discovers and reads one bonded VE.Smart device through BlueZ D-Bus
- implements a deliberately read-only protocol subset
- validates and derives bounded electrical measurements
- atomically persists acquisitions, energy state and an outbound retry spool
- pushes Prometheus text to VictoriaMetrics without opening an inbound port
- runs under systemd with READY notification and progress-aware watchdog
- produces ARMv6/Thumb-1/VFPv2 hard-float binaries for Raspberry Pi Zero W
- includes diagnostic CLI, deployment scripts and sanitized protocol fixtures

The collector intentionally exposes no settings, PIN/PUK, bonding-key, DFU or
other device-writing API. Pairing is performed separately with `bluetoothctl`;
pairing credentials never belong in this repository or its configuration.

## Workspace

| Path | Purpose |
|---|---|
| `apps/victron-collector` | production collector daemon |
| `apps/victron-cli` | diagnostics and connectivity checks |
| `crates/victron-bluez` | Linux BlueZ BLE transport |
| `crates/victron-protocol` | pure VE.Smart framing and decoding |
| `crates/victron-domain` | validated domain measurements |
| `crates/victron-storage` | SQLite state and delivery spool |
| `crates/victron-metrics` | Prometheus encoding and HTTP delivery |
| `crates/victron-service` | collection-cycle orchestration |
| `deploy` | systemd unit, configuration and lifecycle scripts |
| `release` | reproducible ARMv6 release build and verifier |
| `analysis` | sanitized interoperability notes and plans |
| `fixtures/protocol` | small sanitized or synthetic protocol fixtures |

Raw applications, decompiler output, runtime captures, generated analysis and
private infrastructure configuration are ignored and are not published.

## Build and test

The release container supplies the Linux dependencies needed by BlueZ/DBus:

```bash
docker build -t victron-armv6-release:phase0 -f release/Dockerfile release/

docker run --rm \
  -v "$PWD":/workspace -w /workspace \
  victron-armv6-release:phase0 \
  bash -lc 'unset PKG_CONFIG_SYSROOT_DIR PKG_CONFIG_LIBDIR PKG_CONFIG_ALLOW_CROSS; \
    cargo test --workspace --all-targets --locked'
```

Build and verify the Raspberry Pi Zero W release artifact:

```bash
release/build-armv6.sh
release/verify-armv6.sh target/arm-unknown-linux-gnueabihf/release/victron-collector
```

See [`ARMv6-BUILD.md`](ARMv6-BUILD.md), [`docs/configuration.md`](docs/configuration.md),
[`docs/operations.md`](docs/operations.md), and [`docs/security.md`](docs/security.md).

## Development hooks

Project tools are pinned in `mise.toml`. Install them and activate the local
Git hooks once per clone:

```bash
mise install
mise exec -- hk install --mise
```

The pre-commit hook checks staged changes for secrets, private keys, merge
markers, oversized files, whitespace, Rust formatting, shell syntax and Git
diff errors. The pre-push hook scans the complete Git history with Gitleaks and
runs workspace tests, Clippy, Rustdoc, full shell syntax and deployment tests.

```bash
mise exec -- hk run pre-commit --check
mise exec -- hk run pre-push --check
```

## Interoperability research

The protocol implementation and notes were developed for lawful
interoperability with owner-controlled hardware. The repository contains no
APK/XAPK files, decompiled application source, PINs, keys or proprietary raw
dumps. Small captured wire fixtures contain only protocol framing and ordinary
electrical measurements; provenance and sanitization are documented in
[`fixtures/protocol/README.md`](fixtures/protocol/README.md).

Start with [`analysis/victronconnect-protocol-reference.md`](analysis/victronconnect-protocol-reference.md)
and [`analysis/bluetooth-protocol-findings.md`](analysis/bluetooth-protocol-findings.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
