//! Compare private raw `read-history --raw` captures without assigning field semantics.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::common::{parse_u16, print_or_write_json};
use crate::{runtime, CliError};

const DEFAULT_REGISTERS: &[u16] = &[0x104f, 0x1050, 0xec20, 0xec3e, 0xedec];

#[derive(Debug, Args)]
pub struct AnalyzeHistory {
    /// Raw JSON captures created by `read-history --raw`, in chronological order.
    #[arg(value_name = "CAPTURE", required = true)]
    captures: Vec<PathBuf>,
    /// Register to compare; decimal or 0x-prefixed, repeatable.
    #[arg(long = "register", value_parser = parse_u16)]
    registers: Vec<u16>,
    /// Analyze every raw register present instead of the default research set.
    #[arg(long)]
    all: bool,
    /// Output JSON path; stdout is always emitted too.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug)]
struct Capture {
    source: String,
    started_at_ms: u64,
    completed_at_ms: u64,
    rows: BTreeMap<u16, Vec<u8>>,
}

impl AnalyzeHistory {
    pub fn run(&self) -> Result<(), CliError> {
        let captures = self
            .captures
            .iter()
            .map(|path| load_capture(path))
            .collect::<Result<Vec<_>, _>>()?;
        let registers = selected_registers(&captures, &self.registers, self.all);
        let rows = registers
            .into_iter()
            .map(|register| analyze_register(register, &captures))
            .collect::<Vec<_>>();
        let result = json!({
            "ok": true,
            "status": "raw-evidence-only",
            "semanticsConfirmed": false,
            "captureCount": captures.len(),
            "registerCount": rows.len(),
            "captureOrder": captures.iter().map(|capture| &capture.source).collect::<Vec<_>>(),
            "registers": rows,
            "notes": [
                "Changed offsets are observations, not decoded field meanings.",
                "Word views are emitted in both endian orders and do not imply byte order.",
                "Runtime captures and analysis output must remain outside the public repository."
            ]
        });
        print_or_write_json(&result, self.out.as_deref())
    }
}

fn load_capture(path: &Path) -> Result<Capture, CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| runtime(format!("failed to read capture: {error}")))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| runtime(format!("invalid capture JSON: {error}")))?;
    if value.get("mode").and_then(Value::as_str) != Some("vreg-fallback") {
        return Err(runtime("capture is not a VREG fallback result"));
    }
    let started_at_ms = required_u64(&value, "captureStartedAtUnixMs")?;
    let completed_at_ms = required_u64(&value, "captureCompletedAtUnixMs")?;
    if completed_at_ms < started_at_ms {
        return Err(runtime("capture completion precedes its start"));
    }
    let mut rows = BTreeMap::new();
    let source_rows = value
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| runtime("capture rows are missing"))?;
    for row in source_rows {
        let Some(register) = row.get("register").and_then(Value::as_str) else {
            continue;
        };
        let Some(raw) = row.get("raw").and_then(Value::as_str) else {
            continue;
        };
        let register = parse_u16(register).map_err(runtime)?;
        let raw = hex::decode(raw).map_err(|_| runtime("capture contains invalid raw hex"))?;
        rows.insert(register, raw);
    }
    if rows.is_empty() {
        return Err(runtime("capture has no raw VREG values; rerun with --raw"));
    }
    let source = capture_label(path);
    Ok(Capture {
        source,
        started_at_ms,
        completed_at_ms,
        rows,
    })
}

fn capture_label(path: &Path) -> String {
    let file = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("capture.json");
    if file == "history.json" {
        if let Some(parent) = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
        {
            return parent.to_owned();
        }
    }
    file.to_owned()
}

fn required_u64(value: &Value, field: &str) -> Result<u64, CliError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| runtime(format!("capture field {field} is missing")))
}

fn selected_registers(captures: &[Capture], requested: &[u16], all: bool) -> BTreeSet<u16> {
    if all {
        return captures
            .iter()
            .flat_map(|capture| capture.rows.keys().copied())
            .collect();
    }
    if requested.is_empty() {
        DEFAULT_REGISTERS.iter().copied().collect()
    } else {
        requested.iter().copied().collect()
    }
}

fn analyze_register(register: u16, captures: &[Capture]) -> Value {
    let observations = captures
        .iter()
        .filter_map(|capture| {
            capture
                .rows
                .get(&register)
                .map(|raw| Observation { capture, raw })
        })
        .collect::<Vec<_>>();
    let distinct_payloads = observations
        .iter()
        .map(|observation| observation.raw.clone())
        .collect::<BTreeSet<_>>()
        .len();
    let max_len = observations
        .iter()
        .map(|observation| observation.raw.len())
        .max()
        .unwrap_or(0);
    let stable_offsets = (0..max_len)
        .filter(|offset| byte_values(&observations, *offset).len() == 1)
        .collect::<Vec<_>>();
    let changed_offsets = (0..max_len)
        .filter(|offset| byte_values(&observations, *offset).len() > 1)
        .collect::<Vec<_>>();
    let mut previous: Option<&[u8]> = None;
    let snapshots = observations
        .iter()
        .map(|observation| {
            let changed = previous
                .map(|before| changed_byte_offsets(before, observation.raw))
                .unwrap_or_default();
            previous = Some(observation.raw);
            json!({
                "capture": observation.capture.source,
                "captureStartedAtUnixMs": observation.capture.started_at_ms,
                "captureCompletedAtUnixMs": observation.capture.completed_at_ms,
                "rawBytes": observation.raw.len(),
                "rawSha256": hex::encode(Sha256::digest(observation.raw)),
                "raw": hex::encode(observation.raw),
                "wordsLe": words(observation.raw, u16::from_le_bytes),
                "wordsBe": words(observation.raw, u16::from_be_bytes),
                "changedByteOffsetsFromPrevious": changed,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "register": format!("0x{register:04x}"),
        "observedCount": observations.len(),
        "missingCaptureCount": captures.len().saturating_sub(observations.len()),
        "distinctPayloadCount": distinct_payloads,
        "stableByteOffsets": stable_offsets,
        "changedByteOffsets": changed_offsets,
        "observations": snapshots,
    })
}

struct Observation<'a> {
    capture: &'a Capture,
    raw: &'a Vec<u8>,
}

fn byte_values(observations: &[Observation<'_>], offset: usize) -> BTreeSet<u8> {
    observations
        .iter()
        .filter_map(|observation| observation.raw.get(offset).copied())
        .collect()
}

fn changed_byte_offsets(before: &[u8], after: &[u8]) -> Vec<usize> {
    let max_len = before.len().max(after.len());
    (0..max_len)
        .filter(|offset| before.get(*offset) != after.get(*offset))
        .collect()
}

fn words(raw: &[u8], decode: fn([u8; 2]) -> u16) -> Vec<u16> {
    raw.chunks_exact(2)
        .map(|chunk| decode([chunk[0], chunk[1]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_capture(path: &Path, started: u64, first: &str, second: &str) {
        let value = json!({
            "mode": "vreg-fallback",
            "captureStartedAtUnixMs": started,
            "captureCompletedAtUnixMs": started + 10,
            "rows": [
                {"register": "0x104f", "raw": first},
                {"register": "0x1050", "raw": second}
            ]
        });
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    #[test]
    fn loads_raw_capture_and_ignores_device_identity() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("capture.json");
        write_capture(&path, 100, "01000200", "03000400");
        let capture = load_capture(&path).unwrap();
        assert_eq!(capture.source, "capture.json");
        assert_eq!(capture.started_at_ms, 100);
        assert_eq!(capture.rows[&0x104f], vec![1, 0, 2, 0]);
    }

    #[test]
    fn standard_capture_name_uses_parent_identifier() {
        assert_eq!(
            capture_label(Path::new("/private/20260811T210000Z/history.json")),
            "20260811T210000Z"
        );
    }

    #[test]
    fn changed_offsets_are_byte_exact() {
        assert_eq!(changed_byte_offsets(&[1, 2, 3], &[1, 4]), vec![1, 2]);
    }

    #[test]
    fn missing_raw_values_fail_closed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("capture.json");
        std::fs::write(
            &path,
            r#"{"mode":"vreg-fallback","captureStartedAtUnixMs":1,"captureCompletedAtUnixMs":2,"rows":[]}"#,
        )
        .unwrap();
        assert!(load_capture(&path).is_err());
    }
}
