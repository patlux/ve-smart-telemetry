# victron-cli

Diagnostic executable for the Victron VE.Smart BLE collector, replacing
one-off production debugging scripts.

## Command tree

```text
victron-cli adapters
victron-cli discover --device <alias> [--adapter hci0] [--timeout-seconds 10]
victron-cli read-once --device <alias> [--instance 3] [--timeout-seconds 8] [--raw]
victron-cli decode-fixture <path> [--instance 3] [--verbose]
victron-cli render-metrics <fixture> [--device <name>] [--instance 3]
victron-cli check-victoriametrics [--url ...] [--timeout-ms 3000]
```

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | operational failure (probe failed, device unreachable) |
| 2 | usage error (clap) |
| 3 | command not wired yet |

## Status

Commands that depend on sibling crates being built in parallel
(`victron-bluez`, `victron-protocol`, `victron-domain`, `victron-metrics`)
exit `3` with a precise "not wired" message — never a fake success.
`check-victoriametrics` is already real but **transport-only**: it resolves
the host and opens a TCP connection. Import-path validation
(`POST /api/v1/import/prometheus`, retry classification) awaits
`victron-metrics` and is reported as such.

## Wiring checklist (parent pass, when sibling crates land)

- [ ] `adapters` — implement with `victron-bluez` (adapter enumeration,
      discovery) + `victron-bluez`/`victron-protocol`/`victron-domain`
      (`read-once`: one acquisition cycle, print normalized values; `--raw`
      must stay opt-in debug output with nothing sensitive).
- [ ] `decode-fixture` — implement with `victron-protocol` (CBOR reassembly +
      VREG decoding against captured fixtures).
- [ ] `render-metrics` — implement with `victron-domain` + `victron-metrics`
      (golden Prometheus output with explicit timestamps).
- [ ] `check-victoriametrics` — extend the transport probe with the real
      import-path validation once the metrics HTTP client exists.

Keep raw notification output behind an explicit debug flag and redact or
avoid protected material (no PIN, PUK, bond keys, raw captures).
