//! `victron-cli read-history` — read VE.Smart history paths or VREG fallback.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use flate2::read::ZlibDecoder;
use serde_json::{json, Value};
use victron_protocol::{Request, Response, VregValue};

use super::common::{
    cbor_item_json, decoded_vreg_json, parse_u16, print_or_write_json, transport_config,
    unix_ms_now,
};
use crate::{runtime, CliError};

const SUMMARY_PATHS: &[&str] = &[
    "/CustomName",
    "/Description2",
    "/Yield/System",
    "/Yield/User",
    "/History/Overall/DaysAvailable",
];
const BASIC_SUFFIXES: &[&str] = &[
    "Yield",
    "Consumption",
    "MaxPower",
    "MaxPvVoltage",
    "MinBatteryVoltage",
    "MaxBatteryVoltage",
];
const DETAIL_SUFFIXES: &[&str] = &[
    "TimeInBulk",
    "TimeInAbsorption",
    "TimeInFloat",
    "LastError1",
    "LastError2",
];
const FALLBACK_REGISTERS: &[u16] = &[
    0x104f, 0x1050, 0x2001, 0x2007, 0x2008, 0x200b, 0x2013, 0x2027, 0xec20, 0xec3e, 0xec5a, 0xed8c,
    0xed8d, 0xed8f, 0xeda9, 0xedbb, 0xedbc, 0xedec,
];

#[derive(Debug, Args)]
pub struct ReadHistory {
    /// BlueZ alias of the bonded Victron device.
    #[arg(long, default_value = "Solar Charger")]
    device: String,
    /// BlueZ adapter.
    #[arg(long, default_value = "hci0")]
    adapter: String,
    /// VE.Smart instance.
    #[arg(long, default_value_t = 3)]
    instance: u16,
    /// Number of daily buckets to request.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u16).range(1..=365))]
    days: u16,
    /// Include time-in-state and last-error paths.
    #[arg(long)]
    include_detail: bool,
    /// Extra path to request; repeatable.
    #[arg(long = "path")]
    extra_paths: Vec<String>,
    /// Extra fallback VREG; decimal or 0x-prefixed, repeatable.
    #[arg(long = "vreg", value_parser = parse_u16)]
    extra_vregs: Vec<u16>,
    /// Fail when the path API is unavailable.
    #[arg(long)]
    no_vreg_fallback: bool,
    /// Print candidate paths and request bytes without BLE access.
    #[arg(long)]
    dry_run: bool,
    /// Discovery timeout in seconds.
    #[arg(long, default_value_t = 12)]
    discovery_timeout_seconds: u64,
    /// Connect timeout in seconds.
    #[arg(long, default_value_t = 30)]
    connect_timeout_seconds: u64,
    /// Per-request response timeout in seconds.
    #[arg(long, default_value_t = 12)]
    response_timeout_seconds: u64,
    /// Maximum VREG/path indexes per request.
    #[arg(long, default_value_t = 12)]
    batch_size: usize,
    /// Output JSON path; stdout is always emitted too.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Include individual VREG raw values. Never includes full BLE frames.
    #[arg(long)]
    raw: bool,
}

impl ReadHistory {
    pub async fn run(&self) -> Result<(), CliError> {
        if self.instance == 0 {
            return Err(runtime("instance 0 is the keep-alive pseudo-instance"));
        }
        if !(1..=512).contains(&self.batch_size) {
            return Err(runtime("batch-size must be between 1 and 512"));
        }
        let paths = candidate_paths(self.days, self.include_detail, &self.extra_paths);
        if self.dry_run {
            return print_or_write_json(&self.dry_run_json(paths)?, self.out.as_deref());
        }

        let capture_started_at_ms = unix_ms_now()?;
        let timeout = Duration::from_secs(self.response_timeout_seconds);
        let config = transport_config(
            &self.device,
            &self.adapter,
            Duration::from_secs(self.discovery_timeout_seconds),
            timeout,
            Duration::from_secs(self.connect_timeout_seconds),
        )?;
        let mut session = victron_client::VeSmartBleSession::new(config);
        let result = self.read(&mut session, paths, timeout).await;
        session.close_read_only().await;
        let mut result = result?;
        let capture_completed_at_ms = unix_ms_now()?;
        let object = result
            .as_object_mut()
            .ok_or_else(|| runtime("history result must be a JSON object"))?;
        object.insert(
            "captureStartedAtUnixMs".to_owned(),
            json!(capture_started_at_ms),
        );
        object.insert(
            "captureCompletedAtUnixMs".to_owned(),
            json!(capture_completed_at_ms),
        );
        print_or_write_json(&result, self.out.as_deref())
    }

    fn dry_run_json(&self, paths: Vec<String>) -> Result<Value, CliError> {
        let list = Request::GetPathList {
            instance: self.instance,
        }
        .encode()
        .map_err(|error| runtime(error.to_string()))?;
        let indexes = (0..paths.len().min(5)).map(|value| value as i64).collect();
        let values = Request::GetPathValues {
            instance: self.instance,
            path_indexes: indexes,
        }
        .encode()
        .map_err(|error| runtime(error.to_string()))?;
        Ok(json!({
            "ok": true,
            "mode": "dry-run",
            "device": self.device,
            "instance": self.instance,
            "days": self.days,
            "candidatePathCount": paths.len(),
            "candidatePaths": paths,
            "requests": {
                "getPathList": hex::encode(list),
                "getPathValuesExample": hex::encode(values),
            },
            "notes": [
                "Path indexes are runtime-defined by the device.",
                "Day 0 is expected to be today and remains runtime-verified per firmware.",
            ],
        }))
    }

    async fn read(
        &self,
        session: &mut victron_client::VeSmartBleSession,
        wanted_paths: Vec<String>,
        timeout: Duration,
    ) -> Result<Value, CliError> {
        session
            .open_read_only()
            .await
            .map_err(|error| runtime(error.to_string()))?;
        session
            .subscribe_read_only(self.instance)
            .await
            .map_err(|error| runtime(error.to_string()))?;

        let list_responses = session
            .request_read_only(
                &Request::GetPathList {
                    instance: self.instance,
                },
                timeout,
            )
            .await;
        let paths = match list_responses {
            Ok(responses) => decode_paths(&responses)?,
            Err(error) if path_api_unavailable(error.kind(), error.peer_control_code()) => {
                tracing::debug!(
                    operation = "history-path-list",
                    outcome = "vreg-fallback",
                    error_kind = error.kind(),
                    peer_control_code = error.peer_control_code().unwrap_or(0),
                    "path API unavailable; using observed read-only VREG fallback"
                );
                Vec::new()
            }
            Err(error) => return Err(runtime(error.to_string())),
        };
        if paths.is_empty() {
            if self.no_vreg_fallback {
                return Err(runtime("device did not provide a usable PathList"));
            }
            return self.read_vreg_fallback(session, timeout).await;
        }
        self.read_path_values(session, paths, wanted_paths, timeout)
            .await
    }

    async fn read_vreg_fallback(
        &self,
        session: &mut victron_client::VeSmartBleSession,
        timeout: Duration,
    ) -> Result<Value, CliError> {
        let registers = FALLBACK_REGISTERS
            .iter()
            .copied()
            .chain(self.extra_vregs.iter().copied())
            .collect::<BTreeSet<_>>();
        let mut values = BTreeMap::new();
        for batch in registers
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .chunks(self.batch_size)
        {
            let response = session
                .request_read_only(
                    &Request::GetValues {
                        instance: self.instance,
                        registers: batch.to_vec(),
                    },
                    timeout,
                )
                .await;
            let responses = match response {
                Ok(responses) => responses,
                Err(error) if error.kind() == "timeout" => continue,
                Err(error) => return Err(runtime(error.to_string())),
            };
            for response in responses {
                if let Response::Value { register, data, .. } = response {
                    values.insert(register, data);
                }
            }
        }
        if values.is_empty() {
            return Err(runtime("no history fallback VREGs received"));
        }
        let rows = values
            .into_iter()
            .map(|(register, data)| {
                let decoded = VregValue::new(register, data.clone()).decode();
                let mut value = decoded_vreg_json(&decoded, self.raw.then_some(data.as_slice()));
                if !self.raw {
                    value.as_object_mut().expect("object").remove("raw");
                }
                value
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "ok": true,
            "mode": "vreg-fallback",
            "device": self.device,
            "instance": self.instance,
            "pathListError": "device did not provide a usable PathList",
            "valueRegisterCount": rows.len(),
            "rows": rows,
        }))
    }

    async fn read_path_values(
        &self,
        session: &mut victron_client::VeSmartBleSession,
        paths: Vec<String>,
        wanted: Vec<String>,
        timeout: Duration,
    ) -> Result<Value, CliError> {
        let index_by_path = paths
            .iter()
            .enumerate()
            .map(|(index, path)| (path.as_str(), index as i64))
            .collect::<BTreeMap<_, _>>();
        let mut resolved = BTreeMap::new();
        let mut missing = Vec::new();
        for path in wanted {
            let relative = path.strip_prefix("/History/Daily");
            let index = index_by_path
                .get(path.as_str())
                .or_else(|| relative.and_then(|value| index_by_path.get(value)));
            if let Some(index) = index {
                resolved.insert(path, *index);
            } else {
                missing.push(path);
            }
        }
        let mut values = BTreeMap::new();
        let indexes = resolved.values().copied().collect::<BTreeSet<_>>();
        for batch in indexes
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .chunks(self.batch_size)
        {
            let responses = session
                .request_read_only(
                    &Request::GetPathValues {
                        instance: self.instance,
                        path_indexes: batch.to_vec(),
                    },
                    timeout,
                )
                .await
                .map_err(|error| runtime(error.to_string()))?;
            for response in responses {
                if let Response::PathValue {
                    path_index, value, ..
                } = response
                {
                    values.insert(path_index, cbor_item_json(&value));
                }
            }
        }
        let rows = resolved
            .into_iter()
            .map(|(path, index)| json!({"path": path, "pathIndex": index, "value": values.get(&index)}))
            .collect::<Vec<_>>();
        Ok(json!({
            "ok": true,
            "mode": "path-values",
            "device": self.device,
            "instance": self.instance,
            "pathCount": paths.len(),
            "requestedPathCount": rows.len(),
            "missingPathCount": missing.len(),
            "missingPaths": missing,
            "rows": rows,
        }))
    }
}

fn path_api_unavailable(error_kind: &str, peer_control_code: Option<u16>) -> bool {
    error_kind == "timeout" || peer_control_code.is_some()
}

fn candidate_paths(days: u16, detail: bool, extra: &[String]) -> Vec<String> {
    let mut paths = SUMMARY_PATHS
        .iter()
        .map(|value| (*value).to_string())
        .collect::<BTreeSet<_>>();
    let mut suffixes = BASIC_SUFFIXES.to_vec();
    if detail {
        suffixes.extend_from_slice(DETAIL_SUFFIXES);
    }
    for day in 0..days {
        for suffix in &suffixes {
            paths.insert(format!("/History/Daily/{day}/{suffix}"));
        }
    }
    for suffix in suffixes {
        paths.insert(format!("/0/{suffix}"));
    }
    paths.extend(extra.iter().cloned());
    paths.into_iter().collect()
}

fn decode_paths(responses: &[Response]) -> Result<Vec<String>, CliError> {
    let mut paths_by_index = responses
        .iter()
        .filter_map(|response| match response {
            Response::NewPath {
                path_index, path, ..
            } if *path_index >= 0 => Some((*path_index as usize, path.clone())),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    if let Some(compressed) = responses.iter().find_map(|response| match response {
        Response::PathList { compressed, .. } => Some(compressed.as_slice()),
        _ => None,
    }) {
        for (index, path) in split_paths(&inflate_path_list(compressed)?)
            .into_iter()
            .enumerate()
        {
            paths_by_index.entry(index).or_insert(path);
        }
    }
    let Some(last) = paths_by_index.keys().next_back().copied() else {
        return Ok(Vec::new());
    };
    Ok((0..=last)
        .map(|index| paths_by_index.get(&index).cloned().unwrap_or_default())
        .collect())
}

fn inflate_path_list(data: &[u8]) -> Result<Vec<u8>, CliError> {
    for candidate in [data.get(4..).unwrap_or_default(), data] {
        let mut decoder = ZlibDecoder::new(candidate);
        let mut out = Vec::new();
        if decoder.read_to_end(&mut out).is_ok() && !out.is_empty() {
            return Ok(out);
        }
    }
    Err(runtime("failed to decompress VE.Smart PathList"))
}

fn split_paths(data: &[u8]) -> Vec<String> {
    let text = decode_path_text(data);
    for delimiter in ['\0', '\n', '\r'] {
        let paths = text
            .split(delimiter)
            .map(str::trim)
            .filter(|value| is_valid_path(value))
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !paths.is_empty() {
            return paths;
        }
    }
    regex::Regex::new(r"/[A-Za-z0-9_./-]+")
        .expect("static regex")
        .find_iter(&text)
        .map(|value| value.as_str().to_string())
        .collect()
}

fn decode_path_text(data: &[u8]) -> String {
    let mut candidates = Vec::new();
    if let Ok(text) = String::from_utf8(data.to_vec()) {
        candidates.push(text);
    }
    let le = data
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    let be = data
        .chunks_exact(2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    candidates.push(String::from_utf16_lossy(&le));
    candidates.push(String::from_utf16_lossy(&be));
    candidates
        .into_iter()
        .max_by_key(|text| path_text_score(text))
        .unwrap_or_default()
}

fn path_text_score(text: &str) -> usize {
    let delimited_paths = text
        .split(['\0', '\n', '\r'])
        .filter(|part| is_valid_path(part.trim()))
        .count();
    let regex_paths = regex::Regex::new(r"/[A-Za-z0-9_./-]+")
        .expect("static regex")
        .find_iter(text)
        .count();
    let printable = text
        .chars()
        .filter(|character| character.is_ascii_graphic() || character.is_ascii_whitespace())
        .count();
    delimited_paths
        .saturating_mul(10_000)
        .saturating_add(regex_paths.saturating_mul(1_000))
        .saturating_add(printable)
}

fn is_valid_path(value: &str) -> bool {
    value.len() > 1
        && value.starts_with('/')
        && value
            .bytes()
            .skip(1)
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    #[test]
    fn peer_control_and_timeout_enable_fallback_but_contention_does_not() {
        assert!(path_api_unavailable("peer_control", Some(3)));
        assert!(path_api_unavailable("timeout", None));
        assert!(!path_api_unavailable("contention", None));
    }

    #[test]
    fn candidate_paths_include_daily_and_relative_forms() {
        let paths = candidate_paths(2, false, &[]);
        assert!(paths.contains(&"/History/Daily/0/Yield".to_string()));
        assert!(paths.contains(&"/History/Daily/1/MaxPower".to_string()));
        assert!(paths.contains(&"/0/Yield".to_string()));
        assert!(!paths.iter().any(|path| path.ends_with("TimeInBulk")));
    }

    #[test]
    fn qt_compressed_path_list_round_trips() {
        let raw = b"/Yield/System\0/History/Daily/0/Yield\0";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(raw).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut qt = (raw.len() as u32).to_be_bytes().to_vec();
        qt.extend(compressed);
        assert_eq!(inflate_path_list(&qt).unwrap(), raw);
        assert_eq!(split_paths(raw).len(), 2);
    }

    #[test]
    fn utf16_path_lists_prefer_the_correct_endianness() {
        let text = "/Yield/System\0/History/Daily/0/Yield\0";
        let le = text
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let be = text
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<_>>();
        assert_eq!(split_paths(&le).len(), 2);
        assert_eq!(split_paths(&be).len(), 2);
    }
}
