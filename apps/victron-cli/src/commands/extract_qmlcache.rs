//! Extract readable UTF-16LE strings from Qt QML cache symbols in an ELF32 file.

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Args;
use regex::Regex;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::common::write_json;
use crate::{runtime, CliError};

const DEFAULT_SYMBOL_REGEX: &str = r"QmlCacheGeneratedCode::_qml_.*::qmlData$";

#[derive(Debug, Args)]
pub struct ExtractQmlcache {
    /// Path to an ELF32 VictronConnect shared library.
    elf: PathBuf,
    /// Regex selecting dynamic symbols.
    #[arg(long, default_value = DEFAULT_SYMBOL_REGEX)]
    symbols: String,
    /// nm-compatible command.
    #[arg(long, default_value = "nm")]
    nm: String,
    /// Minimum readable UTF-16LE string length.
    #[arg(long, default_value_t = 3)]
    min_chars: usize,
    /// Output JSON path.
    #[arg(long)]
    out: PathBuf,
    /// Optional TSV output path.
    #[arg(long)]
    tsv: Option<PathBuf>,
    /// Optional directory receiving raw qmlData blobs.
    #[arg(long)]
    dump_blobs_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
struct LoadSegment {
    offset: usize,
    vaddr: u32,
    filesz: usize,
}

#[derive(Debug)]
struct Symbol {
    addr: u32,
    size: usize,
    name: String,
    module: String,
}

impl ExtractQmlcache {
    pub fn run(&self) -> Result<(), CliError> {
        if self.min_chars == 0 {
            return Err(runtime("min-chars must be positive"));
        }
        let elf = self
            .elf
            .canonicalize()
            .map_err(|error| runtime(format!("failed to resolve ELF path: {error}")))?;
        let data =
            std::fs::read(&elf).map_err(|error| runtime(format!("failed to read ELF: {error}")))?;
        let segments = parse_load_segments(&data)?;
        let symbol_regex = Regex::new(&self.symbols)
            .map_err(|error| runtime(format!("invalid symbol regex: {error}")))?;
        let symbols = read_symbols(&elf, &self.nm, &symbol_regex)?;

        if let Some(directory) = &self.dump_blobs_dir {
            std::fs::create_dir_all(directory)
                .map_err(|error| runtime(format!("failed to create blob directory: {error}")))?;
        }
        let mut modules = Vec::new();
        let mut tsv =
            vec!["module\tsymbol\tblob_offset\tstring_vma\tfile_offset\ttext".to_string()];
        for symbol in symbols {
            let Some(file_offset) = vma_to_offset(symbol.addr, &segments) else {
                eprintln!(
                    "victron-cli: warning: cannot map VMA 0x{:x} for {}",
                    symbol.addr, symbol.name
                );
                continue;
            };
            let end = file_offset
                .checked_add(symbol.size)
                .filter(|end| *end <= data.len())
                .ok_or_else(|| runtime("QML symbol extends beyond ELF file"))?;
            let blob = &data[file_offset..end];
            let strings = scan_utf16le(blob, self.min_chars)
                .into_iter()
                .map(|(offset, text)| {
                    let vma = symbol.addr as usize + offset;
                    let absolute_file = file_offset + offset;
                    tsv.push(format!(
                        "{}\t{}\t0x{offset:x}\t0x{vma:08x}\t0x{absolute_file:x}\t{}",
                        symbol.module,
                        symbol.name,
                        text.replace('\t', "\\t").replace('\n', "\\n")
                    ));
                    json!({
                        "offset": offset,
                        "vma": format!("0x{vma:08x}"),
                        "fileOffset": format!("0x{absolute_file:x}"),
                        "lengthChars": text.chars().count(),
                        "text": text,
                    })
                })
                .collect::<Vec<_>>();
            if let Some(directory) = &self.dump_blobs_dir {
                let path = directory.join(format!("qmldata_{}.bin", safe_module(&symbol.module)));
                std::fs::write(path, blob)
                    .map_err(|error| runtime(format!("failed to write QML blob: {error}")))?;
            }
            modules.push(json!({
                "module": symbol.module,
                "symbol": symbol.name,
                "vma": format!("0x{:08x}", symbol.addr),
                "size": symbol.size,
                "fileOffset": format!("0x{file_offset:x}"),
                "sha256": hex::encode(Sha256::digest(blob)),
                "stringCount": strings.len(),
                "strings": strings,
            }));
        }
        let string_count: usize = modules
            .iter()
            .filter_map(|module| module["stringCount"].as_u64())
            .map(|value| value as usize)
            .sum();
        let result = json!({
            "metadata": {
                "elf": elf,
                "symbolRegex": self.symbols,
                "minChars": self.min_chars,
                "moduleCount": modules.len(),
                "stringCount": string_count,
            },
            "modules": modules,
        });
        write_json(&result, &self.out)?;
        if let Some(path) = &self.tsv {
            write_text(path, &(tsv.join("\n") + "\n"))?;
        }
        eprintln!(
            "modules={} strings={} json={}",
            result["metadata"]["moduleCount"],
            string_count,
            self.out.display()
        );
        Ok(())
    }
}

fn parse_load_segments(data: &[u8]) -> Result<Vec<LoadSegment>, CliError> {
    if data.get(..4) != Some(b"\x7fELF") {
        return Err(runtime("not an ELF file"));
    }
    if data.get(4) != Some(&1) || data.get(5) != Some(&1) {
        return Err(runtime("only little-endian ELF32 is supported"));
    }
    let phoff = read_u32(data, 28)? as usize;
    let phentsize = read_u16(data, 42)? as usize;
    let phnum = read_u16(data, 44)? as usize;
    if phentsize < 32 {
        return Err(runtime("invalid ELF program-header size"));
    }
    let mut segments = Vec::new();
    for index in 0..phnum {
        let offset = phoff
            .checked_add(index.saturating_mul(phentsize))
            .ok_or_else(|| runtime("ELF program-header overflow"))?;
        if read_u32(data, offset)? != 1 {
            continue;
        }
        segments.push(LoadSegment {
            offset: read_u32(data, offset + 4)? as usize,
            vaddr: read_u32(data, offset + 8)?,
            filesz: read_u32(data, offset + 16)? as usize,
        });
    }
    Ok(segments)
}

fn read_symbols(path: &Path, nm: &str, wanted: &Regex) -> Result<Vec<Symbol>, CliError> {
    let output = Command::new(nm)
        .args(["-D", "-S", "-C", "--defined-only"])
        .arg(path)
        .output()
        .map_err(|error| runtime(format!("failed to execute nm: {error}")))?;
    if !output.status.success() {
        return Err(runtime(format!(
            "nm failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let line = Regex::new(r"^([0-9a-fA-F]+)\s+([0-9a-fA-F]+)\s+\S\s+(.+)$").expect("static regex");
    let module =
        Regex::new(r"QmlCacheGeneratedCode::_qml_(.+?)_qml::qmlData$").expect("static regex");
    let mut symbols = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|value| {
            let captures = line.captures(value.trim())?;
            let name = captures[3].to_string();
            if !wanted.is_match(&name) {
                return None;
            }
            Some(Symbol {
                addr: u32::from_str_radix(&captures[1], 16).ok()?,
                size: usize::from_str_radix(&captures[2], 16).ok()?,
                module: module
                    .captures(&name)
                    .map(|value| value[1].to_string())
                    .unwrap_or_else(|| name.clone()),
                name,
            })
        })
        .collect::<Vec<_>>();
    symbols.sort_by(|a, b| (a.addr, &a.name).cmp(&(b.addr, &b.name)));
    Ok(symbols)
}

fn scan_utf16le(blob: &[u8], minimum: usize) -> Vec<(usize, String)> {
    let mut strings = Vec::new();
    let mut index = 0;
    while index + 1 < blob.len() {
        let start = index;
        let mut text = String::new();
        while index + 1 < blob.len() {
            let code = u16::from_le_bytes([blob[index], blob[index + 1]]);
            if !matches!(code, 0x09 | 0x0a | 0x0d | 0x20..=0x7e) {
                break;
            }
            text.push(char::from_u32(u32::from(code)).expect("ASCII codepoint"));
            index += 2;
        }
        if text.chars().count() >= minimum {
            strings.push((start, text));
        } else {
            index = start + 1;
        }
    }
    strings
}

fn vma_to_offset(vma: u32, segments: &[LoadSegment]) -> Option<usize> {
    segments.iter().find_map(|segment| {
        let delta = vma.checked_sub(segment.vaddr)? as usize;
        (delta < segment.filesz).then(|| segment.offset + delta)
    })
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, CliError> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| runtime("truncated ELF header"))?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, CliError> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| runtime("truncated ELF header"))?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn safe_module(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn write_text(path: &Path, text: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| runtime(format!("failed to create output directory: {error}")))?;
    }
    std::fs::write(path, text).map_err(|error| runtime(format!("failed to write output: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_scanner_preserves_offsets() {
        let blob = b"\0A\0B\0C\0\0x";
        assert_eq!(scan_utf16le(blob, 3), vec![(1, "ABC".to_string())]);
    }

    #[test]
    fn module_names_are_safe_for_dump_paths() {
        assert_eq!(safe_module("Page/Name::x"), "Page_Name__x");
    }
}
