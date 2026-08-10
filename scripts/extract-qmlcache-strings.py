#!/usr/bin/env python3
"""Extract Qt QML cache string tables from a VictronConnect ELF.

The Android VictronConnect binary exposes symbols like:

  QmlCacheGeneratedCode::_qml_PageSolarCharger_qml::qmlData

Each points to a qv4cdata blob. This script slices those blobs from the ELF and
scans them for readable UTF-16LE strings. It does not execute the binary.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import struct
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

DEFAULT_SYMBOL_RE = r"QmlCacheGeneratedCode::_qml_.*::qmlData$"
MODULE_RE = re.compile(r"QmlCacheGeneratedCode::_qml_(?P<module>.+?)_qml::qmlData$")
NM_LINE_RE = re.compile(r"^([0-9a-fA-F]+)\s+([0-9a-fA-F]+)\s+\S\s+(.+)$")


@dataclass(frozen=True)
class LoadSegment:
    offset: int
    vaddr: int
    filesz: int
    memsz: int


@dataclass(frozen=True)
class Symbol:
    addr: int
    size: int
    name: str
    module: str


def parse_elf_load_segments(data: bytes) -> list[LoadSegment]:
    if data[:4] != b"\x7fELF":
        raise ValueError("not an ELF file")
    elf_class = data[4]
    endian = data[5]
    if elf_class != 1:
        raise ValueError("only ELF32 is supported by this script")
    if endian != 1:
        raise ValueError("only little-endian ELF is supported by this script")

    header = struct.unpack_from("<HHIIIIIHHHHHH", data, 16)
    _e_type, _e_machine, _e_version, _e_entry, e_phoff, _e_shoff, _e_flags, _e_ehsize, e_phentsize, e_phnum, *_ = header
    segments: list[LoadSegment] = []
    for idx in range(e_phnum):
        off = e_phoff + idx * e_phentsize
        p_type, p_offset, p_vaddr, _p_paddr, p_filesz, p_memsz, _p_flags, _p_align = struct.unpack_from("<IIIIIIII", data, off)
        if p_type == 1:  # PT_LOAD
            segments.append(LoadSegment(offset=p_offset, vaddr=p_vaddr, filesz=p_filesz, memsz=p_memsz))
    return segments


def vma_to_file_offset(vma: int, segments: list[LoadSegment]) -> int | None:
    for seg in segments:
        if seg.vaddr <= vma < seg.vaddr + seg.filesz:
            return seg.offset + (vma - seg.vaddr)
    return None


def run_nm(elf: Path, nm_cmd: str) -> str:
    cmd = [nm_cmd, "-D", "-S", "-C", "--defined-only", str(elf)]
    try:
        return subprocess.check_output(cmd, text=True, stderr=subprocess.PIPE)
    except subprocess.CalledProcessError as exc:
        sys.stderr.write(exc.stderr)
        raise


def parse_symbols(nm_output: str, symbol_re: re.Pattern[str]) -> list[Symbol]:
    out: list[Symbol] = []
    for line in nm_output.splitlines():
        m = NM_LINE_RE.match(line.strip())
        if not m:
            continue
        addr_s, size_s, name = m.groups()
        if not symbol_re.search(name):
            continue
        mm = MODULE_RE.search(name)
        module = mm.group("module") if mm else name.rsplit("::", 2)[-2]
        out.append(Symbol(addr=int(addr_s, 16), size=int(size_s, 16), name=name, module=module))
    out.sort(key=lambda s: (s.addr, s.name))
    return out


def is_ascii_printable_codepoint(cp: int) -> bool:
    return cp in (0x09, 0x0A, 0x0D) or 0x20 <= cp <= 0x7E


def scan_utf16le_ascii(blob: bytes, min_chars: int) -> list[dict[str, Any]]:
    """Scan for non-overlapping readable ASCII strings encoded as UTF-16LE."""
    strings: list[dict[str, Any]] = []
    i = 0
    n = len(blob)
    while i + 1 < n:
        j = i
        chars: list[str] = []
        while j + 1 < n:
            cp = blob[j] | (blob[j + 1] << 8)
            if is_ascii_printable_codepoint(cp):
                chars.append(chr(cp))
                j += 2
                continue
            break
        if len(chars) >= min_chars:
            text = "".join(chars)
            strings.append({"offset": i, "lengthChars": len(chars), "text": text})
            i = j
        else:
            i += 1
    return strings


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("elf", type=Path, help="Path to libVictronConnect_*.so")
    ap.add_argument("--symbols", default=DEFAULT_SYMBOL_RE, help="Regex for symbols to extract")
    ap.add_argument("--nm", default="nm", help="nm command to use")
    ap.add_argument("--min-chars", type=int, default=3, help="Minimum UTF-16 string length")
    ap.add_argument("--out", type=Path, required=True, help="Output JSON path")
    ap.add_argument("--tsv", type=Path, help="Optional TSV output path")
    ap.add_argument("--dump-blobs-dir", type=Path, help="Optional directory to dump qmlData blobs")
    args = ap.parse_args()

    elf = args.elf.resolve()
    data = elf.read_bytes()
    segments = parse_elf_load_segments(data)
    symbols = parse_symbols(run_nm(elf, args.nm), re.compile(args.symbols))

    modules: list[dict[str, Any]] = []
    tsv_rows: list[str] = ["module\tsymbol\tblob_offset\tstring_vma\tfile_offset\ttext"]

    if args.dump_blobs_dir:
        args.dump_blobs_dir.mkdir(parents=True, exist_ok=True)

    for sym in symbols:
        file_off = vma_to_file_offset(sym.addr, segments)
        if file_off is None:
            sys.stderr.write(f"warning: cannot map VMA 0x{sym.addr:x} for {sym.name}\n")
            continue
        blob = data[file_off : file_off + sym.size]
        strings = scan_utf16le_ascii(blob, args.min_chars)
        enriched_strings: list[dict[str, Any]] = []
        for s in strings:
            off = int(s["offset"])
            abs_vma = sym.addr + off
            abs_file = file_off + off
            item = {
                "offset": off,
                "vma": f"0x{abs_vma:08x}",
                "fileOffset": f"0x{abs_file:x}",
                "lengthChars": s["lengthChars"],
                "text": s["text"],
            }
            enriched_strings.append(item)
            escaped = str(s["text"]).replace("\t", "\\t").replace("\n", "\\n")
            tsv_rows.append(f"{sym.module}\t{sym.name}\t0x{off:x}\t0x{abs_vma:08x}\t0x{abs_file:x}\t{escaped}")

        if args.dump_blobs_dir:
            blob_name = f"qmldata_{sym.module}.bin"
            (args.dump_blobs_dir / blob_name).write_bytes(blob)

        modules.append(
            {
                "module": sym.module,
                "symbol": sym.name,
                "vma": f"0x{sym.addr:08x}",
                "size": sym.size,
                "fileOffset": f"0x{file_off:x}",
                "sha256": hashlib.sha256(blob).hexdigest(),
                "stringCount": len(enriched_strings),
                "strings": enriched_strings,
            }
        )

    result = {
        "metadata": {
            "elf": str(elf),
            "symbolRegex": args.symbols,
            "minChars": args.min_chars,
            "moduleCount": len(modules),
            "stringCount": sum(m["stringCount"] for m in modules),
        },
        "modules": modules,
    }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(result, indent=2, sort_keys=True), encoding="utf-8")
    if args.tsv:
        args.tsv.parent.mkdir(parents=True, exist_ok=True)
        args.tsv.write_text("\n".join(tsv_rows) + "\n", encoding="utf-8")

    print(f"modules={len(modules)} strings={result['metadata']['stringCount']} json={args.out}")
    if args.tsv:
        print(f"tsv={args.tsv}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
