# victron-cli

Read-only diagnostic executable for VE.Smart Telemetry. The CLI shares the
same production session implementation as the collector; no Python runtime or
alternate BLE protocol stack is required.

## Commands

```text
victron-cli read-once --device <alias> [--adapter hci0] [--instance 3]
victron-cli read-history --device <alias> [--days 30] [--out history.json]
victron-cli read-history --dry-run --days 30 --out history-dry-run.json
victron-cli decode-fixture <path> [--instance 3] [--verbose]
victron-cli extract-qmlcache <elf32> --out qmlcache.json [--tsv strings.tsv]
victron-cli map-qml-fields <qmlcache.json> --out mapping.json [--md mapping.md]
victron-cli check-victoriametrics [--url ...] [--timeout-ms 3000]
```

`adapters`, `discover`, and `render-metrics` remain explicit placeholders and
exit with code 3 rather than reporting fake success.

## Live values

`read-once` performs the fixed negotiation, subscribes to the configured
instance, reads the bounded dashboard VREG set, and prints decoded JSON.
Individual VREG raw bytes are available only with `--raw`; complete BLE frames
are never included.

```bash
victron-cli read-once --device 'Solar Charger'
```

## Device history

`read-history` first attempts the read-only PathList/PathValue API. If the
device or firmware rejects that API, it requests the observed history/trend
VREG fallback. Registers `0x104f` and `0x1050` remain structured 34-byte word
blocks until their per-day field layout is independently verified; the CLI
does not invent history semantics.

```bash
victron-cli read-history \
  --device 'Solar Charger' \
  --days 30 \
  --out history.json
```

Use `--no-vreg-fallback` to require the path API. Extra read-only paths and
VREGs can be requested with repeated `--path` and `--vreg` arguments. On the
tested charger, `GetPathList` returns the bounded VE.Smart control error
`f7 code 3`; this is a device/firmware rejection, not connection contention,
and automatically selects the VREG fallback.

## Bounded BLE diagnostics

CLI diagnostics are quiet by default. Enable targeted stderr tracing for a
single command without exposing MAC addresses, aliases, raw BLE frames, D-Bus
messages, or payload bytes:

```bash
RUST_LOG='warn,victron_bluez=debug,victron_client=debug,victron_cli=debug' \
  victron-cli read-history --device 'Solar Charger'
```

The trace reports bounded stages, durations, request opcodes, error classes,
timeout operation labels, and VE.Smart `f7` control error codes. Remove the
environment override after investigation.

## Offline research tools

`extract-qmlcache` reads little-endian ELF32 files without executing them. It
invokes the configured `nm` command directly (no shell), maps PT_LOAD VMAs to
file offsets, extracts bounded UTF-16LE strings, and can write JSON, TSV, and
raw qmlData blobs.

`map-qml-fields` converts that JSON into the repository's static UI candidate
map and optional Markdown report. Static candidates always retain
`needsRuntimeConfirmation=true`.

## Read-only contract

Live commands accept only `victron_protocol::Request`, whose public enum has no
settings-write, path-write, PIN/PUK, bonding-key, or DFU variants. Negotiation
control bytes are fixed inside `victron-client`; callers cannot provide
arbitrary writes.

## Exit codes

| Code | Meaning |
|---:|---|
| 0 | success |
| 1 | operational failure |
| 2 | usage error from clap |
| 3 | explicitly not-wired command |
