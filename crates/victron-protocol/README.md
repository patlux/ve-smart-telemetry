# victron-protocol

Pure, runtime-independent Rust implementation of the **read-only VE.Smart BLE
protocol** used by Victron Smart devices (tested against an owned MPPT solar
charger). No BlueZ, no async runtime, no filesystem, no HTTP — this crate
only speaks the wire protocol and works with any transport.

See the crate-level docs (`src/lib.rs`) for the full design, scope, and limits.

## Scope

| Area | Status |
|---|---|
| UUID constants (`dfd0`/`dfd1` service variants, characteristics) | ✅ plain strings, no BlueZ types |
| Control negotiation (`fa 80 ff`, `f9 80`, `f9`/`f8`/`f7`, initial 7-byte control read) | ✅ exact-length parsing |
| Requests: `getDevices`, `subscribe`, `getValues`, `getPathList`, `getPathValues` | ✅ exact bytes |
| Outbound chunking (Data for non-final chunks, LastData for final) | ✅ `outbound::split_request` |
| Bounded Data/LastData reassembly | ✅ 64 KiB default cap |
| Concatenated CBOR decoding (indefinite arrays/bytes, f16/32/64, tags) | ✅ via `minicbor` 2.x |
| Typed response records (DeviceList, Value, Response, PathList, NewPath, PathValue, ...) | ✅ |
| VREG scaling + sentinel rejection + confidence markers | ✅ (0xEDBB confirmed; 5 candidate decoders from the live reader) |
| Settings writes / PIN / PUK / DFU | ❌ intentionally absent |

## Usage

```toml
[dependencies]
victron-protocol = { path = "crates/victron-protocol" }
```

```rust
use victron_protocol::{Request, Response, Reassembler, VregValue};

// request
let req = Request::GetValues { instance: 3, registers: vec![0xedbb, 0xed8d] };
let bytes = req.encode()?;

// outbound: split into typed Data/LastData chunks (single-frame → LastData)
let chunks = victron_protocol::split_request(&bytes, 20)?;
for chunk in chunks {
    // write chunk.bytes to data_uuid()/last_data_uuid() per chunk.target
}

// inbound notification → payload → typed records
let mut ra = Reassembler::new();
let payload = ra.push_last_data(&notification)?;
let responses = Response::parse_stream(payload.as_deref().unwrap())?;
for r in responses {
    if let Some(vreg) = r.as_vreg_value() {
        let decoded = vreg.decode(); // Scaled::Number(28.03) for 0xedbb
    }
}
```

## Testing

```sh
cargo test            # unit + fixture-driven integration tests
cargo test --doc
cargo run --example crosscheck -- ../../fixtures/protocol   # fixture summary
```

Fixture tests read `fixtures/protocol/*.bin` (provenance in
`fixtures/protocol/README.md` — most are captured verbatim wire payloads,
`value-history-*` are captured raw payloads with synthetic wrappers, and the
path records are fully synthetic). Expected values preserve the previously
verified prototype outputs. The production parser and CLI are now Rust-only:

```sh
cargo run -p victron-cli -- decode-fixture ../../fixtures/protocol/value-history-104f.bin
cargo run --example crosscheck -- ../../fixtures/protocol
```

## Design notes

- **CBOR**: `minicbor` 2.x (`std` + `half` features) — bounds-checked,
  pure Rust, no C dependencies, verified against the observed concatenated
  streams (indefinite arrays, indefinite byte strings, f16/f32/f64). Generic
  value tree + limits live in `cbor.rs`; bounds arithmetic is checked/
  saturating and the depth/item semantics are documented exactly.
- **Bounds**: reassembler 64 KiB; CBOR depth 16, 4096 items/stream, 64 KiB
  per string item, arrays ≤ 65536; requests ≤ 512 registers/indexes.
- **Confidence**: solar voltage (`0xedbb`, u16/100) and panel power
  (`0xedbc`, u32/100 W) are `Confidence::Confirmed`. Both match live target
  captures; `0xedbc` is also specified by Victron's BlueSolar HEX protocol.
  Everything else is `Candidate` until independently verified — including
  the five decoders added from the live reader's `FIELDS` table (`0xedbd`,
  `0x0201`, `0xeda8`, `0xedad`, `0xedaa`). The lifetime-yield register
  `0xed8e` has **no** mapping yet (documented blocker, not invented).
- **Errors**: `ProtocolError` is typed and its `Display` never includes
  payload bytes or device data.
- **Transport integration**: map `ServiceVariant::characteristics()` onto
  your GATT layer; split outbound requests with `outbound::split_request`
  (single-frame requests go to LastData, the observed working pattern),
  feed Data notifications to `Reassembler::push_data` and LastData
  notifications to `push_last_data`.

## Unresolved protocol items

* **Lifetime yield (`0xed8e`)**: no decoder mapping — the live reader only
  lists it as an opaque generic-power fallback.
* **Live final-chunk behavior**: the exact characteristic alternation rule
  for multi-chunk outbound writes is pending live BLE confirmation;
  `outbound::split_request` commits only to the final-chunk rule
  (non-final → Data, final → LastData).

## Raspberry Pi Zero W

`cargo check --target arm-unknown-linux-gnueabihf` compiles cleanly (the
only dependency, `minicbor`, is pure Rust with no build scripts/C code). Full
cross-linking is part of the workspace Phase 0 toolchain spike.
