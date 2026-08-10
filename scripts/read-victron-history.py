#!/usr/bin/env python3
"""Read Victron VE.Smart solar-charger history paths over BLE.

The script can be used offline with --dry-run while the device is not nearby:

  python3 scripts/read-victron-history.py --dry-run --days 30

When the device is nearby and already paired/bonded:

  uv run --with bleak python scripts/read-victron-history.py --target 'Solar Charger' --days 30 --out history.json

It uses the path-based VE.Smart protocol recovered from VictronConnect:

  0x0a getPathList(instance)
  0x0b getPathValues(instance, [pathIndex...])
  0x0d PathList(instance, qCompress(path-list))
  0x0e NewPath(instance, pathIndex, path)
  0x0f PathValue(instance, pathIndex, value)

No PIN/PUK/keys are read or logged by this script.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import math
import re
import struct
import time
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

CTRL_UUID_DFD0 = "306b0002-b081-4037-83dc-e59fcc3cdfd0"
LAST_DATA_UUID_DFD0 = "306b0003-b081-4037-83dc-e59fcc3cdfd0"
DATA_UUID_DFD0 = "306b0004-b081-4037-83dc-e59fcc3cdfd0"

CTRL_UUID_DFD1 = "306b0002-b081-4037-83dc-e59fcc3cdfd1"
LAST_DATA_UUID_DFD1 = "306b0003-b081-4037-83dc-e59fcc3cdfd1"
DATA_UUID_DFD1 = "306b0004-b081-4037-83dc-e59fcc3cdfd1"

VICTRON_MFG_ID = 0x02E1

HISTORY_SUFFIXES = [
    "Yield",
    "MaxPower",
    "MaxPvVoltage",
    "Consumption",
    "MinBatteryVoltage",
    "MaxBatteryVoltage",
    "TimeInBulk",
    "TimeInAbsorption",
    "TimeInFloat",
    "LastError1",
    "LastError2",
]

SUMMARY_PATHS = [
    "/CustomName",
    "/Description2",
    "/Yield/System",
    "/Yield/User",
    "/History/Overall/DaysAvailable",
]

# VREGs observed to be pushed by the tested MPPT after subscribe(instance=3).
# They include live dashboard values plus history/trend blocks.  PathList is
# rejected on this device/firmware, so these provide a practical fallback.
FALLBACK_HISTORY_VREGS = [
    0x104F,
    0x1050,
    0x2001,
    0x2007,
    0x2008,
    0x200B,
    0x2013,
    0x2027,
    0xEC20,
    0xEC5A,
    0xED8C,
    0xED8D,
    0xED8F,
    0xEDA9,
    0xEDBB,
    0xEDBC,
]

RESPONSE_NAMES = {
    0: "ok",
    1: "unknown-1",
    2: "rejected-or-unsupported",
}


def hx(data: bytes) -> str:
    return data.hex(" ")


def cbor_uint(n: int) -> bytes:
    if n < 0:
        raise ValueError("negative passed to cbor_uint")
    if n < 24:
        return bytes([n])
    if n <= 0xFF:
        return bytes([0x18, n])
    if n <= 0xFFFF:
        return bytes([0x19, (n >> 8) & 0xFF, n & 0xFF])
    if n <= 0xFFFFFFFF:
        return bytes([0x1A]) + n.to_bytes(4, "big")
    return bytes([0x1B]) + n.to_bytes(8, "big")


def cbor_int(n: int) -> bytes:
    if n >= 0:
        return cbor_uint(n)
    value = -1 - n
    if value < 24:
        return bytes([0x20 | value])
    if value <= 0xFF:
        return bytes([0x38, value])
    if value <= 0xFFFF:
        return bytes([0x39]) + value.to_bytes(2, "big")
    if value <= 0xFFFFFFFF:
        return bytes([0x3A]) + value.to_bytes(4, "big")
    return bytes([0x3B]) + value.to_bytes(8, "big")


def cbor_array_ints(values: list[int]) -> bytes:
    n = len(values)
    if n < 24:
        prefix = bytes([0x80 + n])
    elif n <= 0xFF:
        prefix = bytes([0x98, n])
    else:
        prefix = bytes([0x99]) + n.to_bytes(2, "big")
    return prefix + b"".join(cbor_int(v) for v in values)


def cbor_array_uints(values: list[int]) -> bytes:
    n = len(values)
    if n < 24:
        prefix = bytes([0x80 + n])
    elif n <= 0xFF:
        prefix = bytes([0x98, n])
    else:
        prefix = bytes([0x99]) + n.to_bytes(2, "big")
    return prefix + b"".join(cbor_uint(v) for v in values)


def get_path_list_request(instance: int) -> bytes:
    # VeSmartService::getPathList(instance): CBOR(opcode=0x0a), CBOR(instance).
    return cbor_uint(0x0A) + cbor_uint(instance)


def get_values_request(instance: int, registers: list[int]) -> bytes:
    return cbor_uint(0x05) + cbor_uint(instance) + cbor_array_uints(registers)


def get_path_values_request(instance: int, indexes: list[int]) -> bytes:
    # VeSmartService::getPathValues(instance, pathIndexes): opcode 0x0b.
    return cbor_uint(0x0B) + cbor_uint(instance) + cbor_array_ints(indexes)


def get_devices_request() -> bytes:
    return cbor_uint(0x01)


def subscribe_request(instance: int) -> bytes:
    return cbor_uint(0x03) + cbor_uint(instance)


def negotiate_control_writes() -> list[bytes]:
    # Required by the observed device before it returns data payloads.
    return [bytes.fromhex("fa80ff"), bytes.fromhex("f980")]


def decode_one(data: bytes, i: int = 0) -> tuple[Any, int]:
    if i >= len(data):
        raise EOFError
    ib = data[i]
    i += 1
    major, ai = ib >> 5, ib & 0x1F

    def readn(n: int) -> bytes:
        nonlocal i
        if i + n > len(data):
            raise EOFError
        out = data[i : i + n]
        i += n
        return out

    if ai < 24:
        value = ai
    elif ai == 24:
        value = readn(1)[0]
    elif ai == 25:
        value = int.from_bytes(readn(2), "big")
    elif ai == 26:
        value = int.from_bytes(readn(4), "big")
    elif ai == 27:
        value = int.from_bytes(readn(8), "big")
    elif ai == 31 and major in (2, 3, 4, 5):
        value = None
    else:
        raise ValueError(f"unsupported CBOR major={major} ai={ai}")

    if major == 0:
        return value, i
    if major == 1:
        return -1 - int(value), i
    if major == 2:
        if ai == 31:
            chunks = []
            while data[i] != 0xFF:
                chunk, i = decode_one(data, i)
                if not isinstance(chunk, dict) or "bytes" not in chunk:
                    raise ValueError("non-bytes chunk in indefinite bytes")
                chunks.append(bytes.fromhex(chunk["bytes"]))
            return {"bytes": b"".join(chunks).hex()}, i + 1
        return {"bytes": readn(int(value)).hex()}, i
    if major == 3:
        if ai == 31:
            chunks = []
            while data[i] != 0xFF:
                chunk, i = decode_one(data, i)
                chunks.append(str(chunk))
            return "".join(chunks), i + 1
        return readn(int(value)).decode(errors="replace"), i
    if major == 4:
        arr = []
        if ai == 31:
            while data[i] != 0xFF:
                item, i = decode_one(data, i)
                arr.append(item)
            return arr, i + 1
        for _ in range(int(value)):
            item, i = decode_one(data, i)
            arr.append(item)
        return arr, i
    if major == 5:
        obj: dict[str, Any] = {}
        count = math.inf if ai == 31 else int(value)
        n = 0
        while n < count:
            if ai == 31 and data[i] == 0xFF:
                return obj, i + 1
            key, i = decode_one(data, i)
            val, i = decode_one(data, i)
            obj[str(key)] = val
            n += 1
        return obj, i
    if major == 6:
        item, i = decode_one(data, i)
        return {"tag": value, "value": item}, i
    if major == 7:
        if ai == 20:
            return False, i
        if ai == 21:
            return True, i
        if ai == 22:
            return None, i
        if ai == 23:
            return {"undefined": True}, i
        if ai == 25:
            # Half-float is rare here; keep raw-ish value.
            return {"float16bits": value}, i
        if ai == 26:
            return struct.unpack(">f", int(value).to_bytes(4, "big"))[0], i
        if ai == 27:
            return struct.unpack(">d", int(value).to_bytes(8, "big"))[0], i
        return {"simple": value}, i
    raise ValueError(f"unsupported CBOR major={major}")


def decode_stream(data: bytes) -> list[Any]:
    out: list[Any] = []
    i = 0
    while i < len(data):
        try:
            item, i = decode_one(data, i)
            out.append(item)
        except Exception as exc:
            out.append({"error": type(exc).__name__, "offset": i, "tail": data[i:].hex()})
            break
    return out


def qt_uncompress(raw: bytes) -> bytes:
    """Decode Qt qCompress/qUncompress payloads.

    Qt qCompress stores a 4-byte big-endian uncompressed length followed by zlib.
    Some captures may contain zlib bytes directly, so try both.
    """
    attempts: list[tuple[str, bytes]] = []
    if len(raw) > 4:
        attempts.append(("qt", raw[4:]))
    attempts.append(("zlib", raw))
    errors = []
    for name, payload in attempts:
        try:
            return zlib.decompress(payload)
        except Exception as exc:
            errors.append(f"{name}:{type(exc).__name__}")
    raise ValueError("cannot qUncompress path list: " + ", ".join(errors))


def decode_path_blob(raw: bytes) -> list[str]:
    inflated = qt_uncompress(raw)
    text = None
    for enc in ("utf-8", "utf-16-le", "utf-16-be"):
        try:
            candidate = inflated.decode(enc)
        except Exception:
            continue
        printable = sum(1 for ch in candidate if ch.isprintable() or ch in "\r\n\t\0")
        if candidate and printable / max(len(candidate), 1) > 0.8:
            text = candidate
            break
    if text is None:
        text = inflated.decode(errors="replace")

    for delimiter in ("\0", "\n", "\r\n"):
        parts = [p.strip() for p in text.split(delimiter) if p.strip()]
        if len(parts) > 3 and any(p.startswith("/") for p in parts):
            return parts

    # Fallback: extract path-like substrings.
    paths = re.findall(r"/[A-Za-z0-9_./-]+", text)
    if paths:
        return paths
    return [text]


def decode_vreg_payload(register: int, raw: bytes) -> dict[str, Any]:
    def u16(offset: int = 0) -> int | None:
        return int.from_bytes(raw[offset : offset + 2], "little") if len(raw) >= offset + 2 else None

    def s16(offset: int = 0) -> int | None:
        return int.from_bytes(raw[offset : offset + 2], "little", signed=True) if len(raw) >= offset + 2 else None

    def u32(offset: int = 0) -> int | None:
        return int.from_bytes(raw[offset : offset + 4], "little") if len(raw) >= offset + 4 else None

    def s32(offset: int = 0) -> int | None:
        return int.from_bytes(raw[offset : offset + 4], "little", signed=True) if len(raw) >= offset + 4 else None

    def sentinel_hex(value: int, bits: int) -> str:
        if value < 0:
            value = (1 << bits) + value
        return f"0x{value:0{bits // 4}x}"

    def valid_u16(value: int | None) -> tuple[int | None, str | None]:
        if value is None:
            return None, None
        if value == 0xFFFF:
            return None, "0xffff"
        return value, None

    def valid_u32(value: int | None) -> tuple[int | None, str | None]:
        if value is None:
            return None, None
        if value == 0xFFFFFFFF:
            return None, "0xffffffff"
        return value, None

    def valid_s16(value: int | None) -> tuple[int | None, str | None]:
        if value is None:
            return None, None
        if value in (0x7FFF, -0x8000):
            return None, sentinel_hex(value, 16)
        return value, None

    def valid_s32(value: int | None) -> tuple[int | None, str | None]:
        if value is None:
            return None, None
        if value in (0x7FFFFFFF, -0x80000000):
            return None, sentinel_hex(value, 32)
        return value, None

    def with_invalid(decoded: dict[str, Any], sentinel: str | None) -> dict[str, Any]:
        if sentinel is not None:
            decoded["invalid"] = True
            decoded["sentinel"] = sentinel
        return decoded

    if register == 0xEDBB:
        value, sentinel = valid_u16(u16())
        return with_invalid({"name": "Solar voltage", "value": None if value is None else value / 100, "unit": "V", "decoder": "u16_le/100"}, sentinel)
    if register == 0xEDBC:
        value, sentinel = valid_u32(u32())
        return with_invalid({"name": "Solar power", "value": None if value is None else round(value / 100), "unit": "W", "decoder": "u32_le/100 rounded"}, sentinel)
    if register == 0xED8D:
        value, sentinel = valid_u16(u16())
        return with_invalid({"name": "Battery voltage", "value": None if value is None else value / 100, "unit": "V", "decoder": "u16_le/100"}, sentinel)
    if register in (0xED8C, 0x2013):
        value, sentinel = valid_s32(s32())
        return with_invalid({"name": "Battery current" if register == 0xED8C else "Trend/current-like value", "value": None if value is None else value / 1000, "unit": "A", "decoder": "s32_le/1000"}, sentinel)
    if register == 0xED8F:
        value, sentinel = valid_s16(s16())
        return with_invalid({"name": "Current flag/legacy current", "value": value, "unit": None, "decoder": "s16_le"}, sentinel)
    if register == 0xEDA9:
        value, sentinel = valid_u16(u16())
        return with_invalid({"name": "Load/output voltage-like value", "value": None if value is None else value / 10, "unit": "V", "decoder": "u16_le/10", "confidence": "candidate"}, sentinel)
    if register == 0x2027:
        value, sentinel = valid_s32(s32())
        return with_invalid({"name": "Power-like trend value", "value": None if value is None else round(value / 100), "unit": "W", "decoder": "s32_le/100", "confidence": "candidate"}, sentinel)
    if register == 0xEC20 and len(raw) % 8 == 0:
        slots = []
        for offset in range(0, len(raw), 8):
            chunk = raw[offset : offset + 8]
            reg = int.from_bytes(chunk[:2], "little")
            if reg != 0xFFFF:
                slots.append({"offset": offset, "register": f"0x{reg:04x}", "raw": chunk.hex()})
        return {"name": "Trend available-vregs block", "slots": slots, "decoder": "8-byte slots, first u16 register"}
    if register in (0x104F, 0x1050):
        words_le = [int.from_bytes(raw[i : i + 2], "little", signed=False) for i in range(0, len(raw) - 1, 2)]
        words_be = [int.from_bytes(raw[i : i + 2], "big", signed=False) for i in range(0, len(raw) - 1, 2)]
        return {
            "name": "MPPT history/trend block",
            "decoder": "raw 34-byte block; exact field layout still pending vregs.json/static translator extraction",
            "wordsLe": words_le,
            "wordsBe": words_be,
        }
    if len(raw) == 1:
        return {"value": raw[0], "decoder": "u8"}
    if len(raw) == 2:
        value, sentinel = valid_u16(u16())
        return with_invalid({"rawValue": value, "decoder": "raw_u16_le"}, sentinel)
    if len(raw) == 4:
        value, sentinel = valid_s32(s32())
        if sentinel is None and raw == b"\xff\xff\xff\xff":
            sentinel = "0xffffffff"
            value = None
        return with_invalid({"rawValue": value, "decoder": "raw_s32_le"}, sentinel)
    return {"decoder": "raw"}


def decode_path_records(items: list[Any]) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    i = 0
    while i < len(items):
        item = items[i]
        if item == 0x0D and i + 2 < len(items) and isinstance(items[i + 1], int) and isinstance(items[i + 2], dict) and "bytes" in items[i + 2]:
            raw = bytes.fromhex(items[i + 2]["bytes"])
            try:
                paths = decode_path_blob(raw)
                records.append({"type": "PathList", "instance": items[i + 1], "rawLength": len(raw), "paths": paths})
            except Exception as exc:
                records.append({"type": "PathListError", "instance": items[i + 1], "rawLength": len(raw), "error": str(exc), "rawHex": raw.hex()})
            i += 3
            continue
        if item == 0x0E and i + 3 < len(items) and isinstance(items[i + 1], int) and isinstance(items[i + 2], int):
            records.append({"type": "NewPath", "instance": items[i + 1], "pathIndex": items[i + 2], "path": items[i + 3]})
            i += 4
            continue
        if item == 0x0F and i + 3 < len(items) and isinstance(items[i + 1], int) and isinstance(items[i + 2], int):
            records.append({"type": "PathValue", "instance": items[i + 1], "pathIndex": items[i + 2], "value": items[i + 3]})
            i += 4
            continue
        if item == 0x10 and i + 3 < len(items) and isinstance(items[i + 1], int) and isinstance(items[i + 2], int):
            records.append({"type": "PathResponse", "instance": items[i + 1], "pathIndex": items[i + 2], "response": items[i + 3]})
            i += 4
            continue
        if item == 0x07 and i + 3 < len(items):
            response = items[i + 3]
            records.append({
                "type": "ResponseLike",
                "field1": items[i + 1],
                "field2": items[i + 2],
                "response": response,
                "responseName": RESPONSE_NAMES.get(response, f"unknown-{response}"),
            })
            i += 4
            continue
        if item == 0x02 and i + 1 < len(items):
            records.append({"type": "DeviceList", "value": items[i + 1]})
            i += 2
            continue
        if item == 0x08 and i + 3 < len(items) and isinstance(items[i + 1], int) and isinstance(items[i + 2], int) and isinstance(items[i + 3], dict) and "bytes" in items[i + 3]:
            raw = bytes.fromhex(items[i + 3]["bytes"])
            register = int(items[i + 2])
            records.append({
                "type": "Value",
                "instance": items[i + 1],
                "register": f"0x{register:04x}",
                "raw": raw.hex(),
                "decoded": decode_vreg_payload(register, raw),
            })
            i += 4
            continue
        i += 1
    return records


def candidate_history_paths(days: int, include_detail: bool) -> list[str]:
    paths: list[str] = list(SUMMARY_PATHS)
    suffixes = HISTORY_SUFFIXES if include_detail else ["Yield", "Consumption", "MaxPower", "MaxPvVoltage", "MinBatteryVoltage", "MaxBatteryVoltage"]
    for day in range(days):
        for suffix in suffixes:
            paths.append(f"/History/Daily/{day}/{suffix}")
    # Some QML blobs contain suffixes relative to /History/Daily; keep fallback forms.
    for suffix in suffixes:
        paths.append(f"/0/{suffix}")
    return sorted(dict.fromkeys(paths))


def build_dry_run(args: argparse.Namespace) -> dict[str, Any]:
    paths = candidate_history_paths(args.days, args.include_detail) + args.path
    path_list_req = get_path_list_request(args.instance)
    # Cannot know path indexes until runtime PathList is received. Include example request shape.
    example_indexes = list(range(min(5, len(paths))))
    path_values_req = get_path_values_request(args.instance, example_indexes)
    return {
        "ok": True,
        "mode": "dry-run",
        "target": args.target,
        "instance": args.instance,
        "days": args.days,
        "candidatePathCount": len(paths),
        "candidatePaths": paths,
        "requests": {
            "getPathList": {
                "opcode": "0x0a",
                "bytesHex": path_list_req.hex(),
                "meaning": "CBOR uint 0x0a, CBOR instance",
            },
            "getPathValuesExample": {
                "opcode": "0x0b",
                "exampleIndexes": example_indexes,
                "bytesHex": path_values_req.hex(),
                "meaning": "runtime path indexes are resolved from PathList first",
            },
        },
        "notes": [
            "Actual history values require the BLE device nearby.",
            "Path indexes are runtime-defined by the device PathList.",
            "History day 0 is expected to be today; confirm with runtime data.",
        ],
    }


async def find_device(target: str, scan_time: float):
    from bleak import BleakScanner

    devices = await BleakScanner.discover(timeout=scan_time, return_adv=True)
    candidates = []
    for _, (device, adv) in devices.items():
        name = device.name or adv.local_name or ""
        services = {s.lower() for s in (adv.service_uuids or [])}
        mfg = adv.manufacturer_data or {}
        score = 0
        if target.lower() in name.lower():
            score += 100
        if VICTRON_MFG_ID in mfg:
            score += 80
        if any(s.startswith("306b0001") or s.startswith("97580001") for s in services):
            score += 80
        if score:
            candidates.append((score, getattr(adv, "rssi", -999), device, adv))
    candidates.sort(key=lambda row: (-row[0], -row[1]))
    return candidates[0] if candidates else None


def resolve_char_set(service_suffix: str) -> tuple[str, str, str]:
    if service_suffix == "dfd1":
        return CTRL_UUID_DFD1, LAST_DATA_UUID_DFD1, DATA_UUID_DFD1
    return CTRL_UUID_DFD0, LAST_DATA_UUID_DFD0, DATA_UUID_DFD0


async def run_ble(args: argparse.Namespace) -> dict[str, Any]:
    from bleak import BleakClient

    ctrl_uuid, last_data_uuid, data_uuid = resolve_char_set(args.service_suffix)
    found = await find_device(args.target, args.scan_time)
    if not found:
        return {
            "ok": False,
            "error": "target not advertising/found",
            "target": args.target,
            "hint": "Device must be nearby, advertising, and not exclusively connected elsewhere.",
        }

    _, rssi, device, adv = found
    chunk_buffer = bytearray()
    events: list[dict[str, Any]] = []
    path_list: list[str] = []
    path_by_index: dict[int, str] = {}
    values_by_index: dict[int, Any] = {}
    values_by_register: dict[int, dict[str, Any]] = {}

    def apply_records(records: list[dict[str, Any]], source_uuid: str, payload_hex: str) -> None:
        nonlocal path_list, path_by_index
        for rec in records:
            rec = dict(rec)
            rec["sourceUuid"] = source_uuid
            rec["payloadHex"] = payload_hex if args.include_payload_hex else None
            events.append(rec)
            if rec["type"] == "PathList" and rec.get("instance") == args.instance:
                path_list = list(rec.get("paths", []))
                path_by_index = {idx: path for idx, path in enumerate(path_list)}
            elif rec["type"] == "NewPath" and rec.get("instance") == args.instance:
                path_by_index[int(rec["pathIndex"])] = str(rec["path"])
            elif rec["type"] == "PathValue" and rec.get("instance") == args.instance:
                values_by_index[int(rec["pathIndex"])] = rec.get("value")
            elif rec["type"] == "Value" and rec.get("instance") == args.instance:
                values_by_register[int(str(rec["register"]), 16)] = rec

    def on_notify(sender: Any, data: bytearray) -> None:
        nonlocal chunk_buffer
        uuid = str(getattr(sender, "uuid", sender)).lower()
        raw = bytes(data)
        if uuid == data_uuid.lower():
            chunk_buffer.extend(raw)
            if args.verbose:
                print(f"data chunk {len(raw)} bytes: {hx(raw)}")
            return
        if uuid == last_data_uuid.lower():
            chunk_buffer.extend(raw)
            payload = bytes(chunk_buffer)
            chunk_buffer.clear()
        else:
            payload = raw

        items = decode_stream(payload)
        records = decode_path_records(items)
        if args.verbose:
            print(f"notify {uuid} payload={hx(payload)} items={items} records={records}")
        apply_records(records, uuid, payload.hex())

    async with BleakClient(device, timeout=args.connect_timeout) as client:
        for uuid in (ctrl_uuid, last_data_uuid, data_uuid):
            await client.start_notify(uuid, on_notify)

        await asyncio.sleep(0.5)
        try:
            control = bytes(await client.read_gatt_char(ctrl_uuid))
            if args.verbose:
                print(f"control_initial={control.hex()}")
        except Exception as exc:
            if args.verbose:
                print(f"control_read_failed={type(exc).__name__}: {exc}")

        if not args.no_negotiate:
            for payload in negotiate_control_writes():
                if args.verbose:
                    print(f"writeControl bytes={payload.hex()}")
                await client.write_gatt_char(ctrl_uuid, payload, response=False)
                await asyncio.sleep(0.35)

        await client.write_gatt_char(last_data_uuid, get_devices_request(), response=False)
        await asyncio.sleep(0.8)

        if not args.no_subscribe:
            sub = subscribe_request(args.instance)
            if args.verbose:
                print(f"subscribe(instance={args.instance}) bytes={sub.hex()}")
            await client.write_gatt_char(last_data_uuid, sub, response=False)
            await asyncio.sleep(args.subscribe_wait)

        await client.write_gatt_char(last_data_uuid, get_path_list_request(args.instance), response=False)

        start = time.monotonic()
        while time.monotonic() - start < args.path_list_timeout and not path_list:
            await asyncio.sleep(0.2)

        if not path_by_index and not args.no_vreg_fallback:
            registers = list(dict.fromkeys(FALLBACK_HISTORY_VREGS + args.vreg))
            if args.verbose:
                print(f"PathList unavailable; requesting fallback VREGs={[hex(r) for r in registers]}")
            for batch_start in range(0, len(registers), args.batch_size):
                batch = registers[batch_start : batch_start + args.batch_size]
                await client.write_gatt_char(last_data_uuid, get_values_request(args.instance, batch), response=False)
                await asyncio.sleep(args.batch_delay)
            start = time.monotonic()
            while time.monotonic() - start < args.listen_time:
                await asyncio.sleep(0.2)

            fallback_rows = []
            for reg in sorted(values_by_register):
                rec = values_by_register[reg]
                fallback_rows.append({
                    "register": rec["register"],
                    "raw": rec.get("raw"),
                    "decoded": rec.get("decoded"),
                })
            return {
                "ok": bool(fallback_rows),
                "mode": "vreg-fallback",
                "target": device.name or adv.local_name,
                "address": device.address,
                "rssi": rssi,
                "instance": args.instance,
                "pathListError": "no PathList received/decoded; device appears to reject path API on this firmware",
                "valueRegisterCount": len(fallback_rows),
                "rows": fallback_rows,
                "events": events if args.json_events else None,
            }

        if not path_by_index:
            return {
                "ok": False,
                "target": device.name or adv.local_name,
                "address": device.address,
                "rssi": rssi,
                "error": "no PathList received/decoded",
                "events": events,
            }

        wanted_paths = candidate_history_paths(args.days, args.include_detail) + args.path
        wanted_paths = sorted(dict.fromkeys(wanted_paths))
        index_by_path = {path: idx for idx, path in path_by_index.items()}
        missing = [path for path in wanted_paths if path not in index_by_path]

        # Fallback: if full /History/Daily/N/Suffix paths are absent, try relative /N/Suffix.
        resolved: dict[str, int] = {}
        for path in wanted_paths:
            idx = index_by_path.get(path)
            if idx is None and path.startswith("/History/Daily/"):
                rel = path.removeprefix("/History/Daily")
                idx = index_by_path.get(rel)
            if idx is not None:
                resolved[path] = idx

        indexes = list(dict.fromkeys(resolved.values()))
        for batch_start in range(0, len(indexes), args.batch_size):
            batch = indexes[batch_start : batch_start + args.batch_size]
            if args.verbose:
                print(f"getPathValues indexes={batch}")
            await client.write_gatt_char(last_data_uuid, get_path_values_request(args.instance, batch), response=False)
            await asyncio.sleep(args.batch_delay)

        start = time.monotonic()
        while time.monotonic() - start < args.listen_time:
            await asyncio.sleep(0.2)

    rows = []
    for path, idx in resolved.items():
        rows.append({"path": path, "pathIndex": idx, "value": values_by_index.get(idx)})

    return {
        "ok": True,
        "target": device.name or adv.local_name,
        "address": device.address,
        "rssi": rssi,
        "instance": args.instance,
        "pathCount": len(path_by_index),
        "requestedPathCount": len(resolved),
        "missingPathCount": len(missing),
        "missingPaths": missing[:200],
        "rows": rows,
        "events": events if args.json_events else None,
    }


async def run(args: argparse.Namespace) -> int:
    if args.dry_run:
        result = build_dry_run(args)
    else:
        result = await run_ble(args)

    text = json.dumps(result, indent=2, sort_keys=True)
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(text + "\n", encoding="utf-8")
    print(text)
    return 0 if result.get("ok") else 2


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default="Solar Charger", help="BLE name/address search target")
    parser.add_argument("--instance", type=int, default=3, help="VE.Smart instance id; existing live reader uses 3 by default")
    parser.add_argument("--days", type=int, default=30, help="History days to request, where day 0 is expected to be today")
    parser.add_argument("--include-detail", action="store_true", help="include time-in-state and last-error history fields")
    parser.add_argument("--path", action="append", default=[], help="extra path to request; repeatable")
    parser.add_argument("--vreg", action="append", default=[], type=lambda value: int(value, 0), help="extra fallback VREG to request; repeatable, e.g. 0x104f")
    parser.add_argument("--no-vreg-fallback", action="store_true", help="fail if PathList is unavailable instead of reading observed fallback history VREGs")
    parser.add_argument("--dry-run", action="store_true", help="print candidate paths/request bytes without BLE access")
    parser.add_argument("--service-suffix", choices=["dfd0", "dfd1"], default="dfd0", help="VE.Smart service suffix to use")
    parser.add_argument("--scan-time", type=float, default=12)
    parser.add_argument("--connect-timeout", type=float, default=30)
    parser.add_argument("--path-list-timeout", type=float, default=12)
    parser.add_argument("--listen-time", type=float, default=10)
    parser.add_argument("--batch-size", type=int, default=12)
    parser.add_argument("--batch-delay", type=float, default=0.35)
    parser.add_argument("--no-negotiate", action="store_true", help="skip Control fa80ff/f980 negotiation writes")
    parser.add_argument("--no-subscribe", action="store_true", help="skip subscribe(instance) before getPathList")
    parser.add_argument("--subscribe-wait", type=float, default=2.0, help="seconds to wait after subscribe before getPathList")
    parser.add_argument("--json-events", action="store_true", help="include decoded protocol events in output")
    parser.add_argument("--include-payload-hex", action="store_true", help="include raw payload hex in events")
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("--out", type=Path, help="write JSON result to file")
    return asyncio.run(run(parser.parse_args()))


if __name__ == "__main__":
    raise SystemExit(main())
