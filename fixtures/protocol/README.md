# fixtures/protocol

Sanitized VE.Smart wire-payload fixtures for the `victron-protocol` crate.

## Provenance — three categories

Fixtures are **not** all captured notifications. Each `*.bin` file falls into
exactly one of these categories, marked in the list below:

1. **Captured verbatim** — the full byte string arrived on one BLE
   notification (or was written for a request) on the user-owned Victron MPPT
   solar charger (BLE alias `Solar Charger`, VE.Smart service suffix `dfd0`),
   recorded by the analysis scripts in
   `analysis/generated/runtime/*.log|json` (2026-06-03). Values were
   sanitized: no PIN/PUK/advertisement keys (none are ever read by the
   readers), no MAC addresses, device identity, or timestamps in the
   payloads; register values are ordinary electrical measurements
   (volts/amps/watts) from the owner's device — no secrets.
2. **Captured raw payload, synthetically wrapped** — the *payload bytes* are
   captured verbatim, but the surrounding record wrapper was built with the
   crate's own CBOR encoder (byte-identical shape to captured value records).
   Only the `value-history-*` fixtures are in this category.
3. **Fully synthetic** — built entirely with the crate's encoders, no captured
   bytes. The path-API records (`0x0d`-`0x10`) are exercised only in
   `tests/response.rs` (marked `synthetic`), not shipped as fixture files,
   because the tested firmware rejects the path API with response code `2`.

## Format

* `*.bin` — raw payload bytes (the exact byte string that arrived on one
  BLE notification, or that should be written for a request).
* `*.hex` — same bytes as hex text for review/diffing.

## Fixture list and expected decodes

Control characteristic (NOT CBOR) — all **captured verbatim**:

| fixture | bytes | expected |
|---|---|---|
| `ctrl-info-initial` | `00040001de4a00` | initial control read; `ControlInfo` fields (candidate semantics) |
| `ctrl-ready-01` | `f901` | `ReadyToReceive { free_chunks: 1 }` |
| `ctrl-error-0300` | `f70300` | `Error { code: 0x0003 }` (LE) |
| `request-getdevices` | `01` | `Request::GetDevices` |
| `request-subscribe3` | `0303` | `Request::Subscribe { instance: 3 }` |
| `request-getvalues11` | `05038b19edbb...19ed8e` | `GetValues(3, 11 regs)` (captured exact request) |
| `request-pathlist3` | `0a03` | `GetPathList(3)` |

Data/LastData (concatenated CBOR) — **captured verbatim** unless noted:

| fixture | bytes (head) | expected |
|---|---|---|
| `notify-devices-indef` | `029f000001000301ff` | DeviceList, indefinite array → pairs `(0,0),(1,0),(3,1)` |
| `response-subscribe-ok` | `07000300` | Response(instance 0, opcode 3, code ok) |
| `response-subscribe-reject` | `07090302` | Response(instance 9, opcode 3, code rejected) |
| `response-pathlist-reject` | `070a0302` | Response(instance 10, opcode 3, code rejected) |
| `value-solar-voltage` | `080319edbb42f30a` | Value(3, 0xedbb, `f30a`) → **28.03 V, confirmed** |
| `value-battery-voltage` | `080319ed8d42b809` | Value(3, 0xed8d, `b809`) → 24.88 V, candidate |
| `value-load-voltage` | `080319eda9425b00` | Value(3, 0xeda9, `5b00`) → 9.1 V, candidate |
| `value-negative-current` | `080319ed8c44c4ffffff` | Value(3, 0xed8c, `c4ffffff`) → −0.06 A, candidate |
| `value-state-0200` | `08031902004101` | Value(3, 0x0200, `01`) → u8 1 |
| `value-device-0202` | `08031902024402000000` | Value(3, 0x0202, `02000000`) → s32 2 |
| `value-stat-2001` | `080319200142aa0a` | Value(3, 0x2001, `aa0a`) → 2730 |
| `value-trend-ec20` | `080319ec2058208d...` | Value(3, 0xec20, 32 B) → slots `0xed8d,0xedec,0xec3e,0xed8c` |
| `value-history-104f` | `080319104f582201...` | **captured 34 B payload, synthetic wrapper** → block words (see test) |
| `value-history-1050` | `0803191050582200...` | **captured 34 B payload, synthetic wrapper** → block words |
| `value-concat-two` | `080319ed8d42ba09080319edbb42000b` | two Value records in one stream → 24.90 V + 28.16 V |
| `value-concat-five` | `080319ed8f420100...` | five Value records in one stream |

Fully synthetic (device rejects path API on tested firmware):

* `0x0d`/`0x0e`/`0x0f`/`0x10` path records: exercised only in
  `tests/response.rs` (marked `synthetic`), not shipped as fixture files.

## Reference decode run

Expected values are pinned by running the proven Python decoders over the
fixtures:

```sh
python3 /tmp/xcheck.py fixtures/protocol
```

This prints items, records, and per-VREG decoded values that the Rust test
suite (`crates/victron-protocol/tests/fixtures.rs`) asserts.
