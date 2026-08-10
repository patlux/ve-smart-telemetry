#!/usr/bin/env python3
"""Read Victron VE.Smart dashboard values over BLE.

Run with:
  uv run --with bleak python scripts/read-victron-live-values.py

Requires the device to be paired/bonded already. On macOS/CoreBluetooth the
peripheral must be advertising so Bleak can resolve the device.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import struct
import time
from dataclasses import dataclass
from typing import Any

from bleak import BleakClient, BleakScanner

CTRL_UUID = "306b0002-b081-4037-83dc-e59fcc3cdfd0"
DATA1_UUID = "306b0003-b081-4037-83dc-e59fcc3cdfd0"
DATA2_UUID = "306b0004-b081-4037-83dc-e59fcc3cdfd0"
VICTRON_MFG_ID = 0x02E1

STATE_NAMES = {
    0: "Off",
    2: "Fault",
    3: "Bulk",
    4: "Absorption",
    5: "Float",
    6: "Storage",
    7: "Equalize",
    245: "Starting-up",
    247: "Auto equalize/Recondition",
    252: "External control",
}

LOAD_STATE_NAMES = {0: "Off", 1: "On"}


@dataclass(frozen=True)
class Field:
    section: str
    label: str
    register: int
    decoder: str
    unit: str
    decimals: int
    confidence: str


FIELDS = [
    Field("Solar", "Voltage", 0xEDBB, "u16_100", "V", 2, "confirmed"),
    Field("Solar", "Current", 0xEDBD, "u16_10", "A", 1, "candidate"),
    Field("Solar", "Power", 0xEDBC, "u32_100", "W", 0, "candidate/fallback"),
    Field("Battery", "Voltage", 0xED8D, "u16_100", "V", 2, "candidate"),
    Field("Battery", "Current", 0xED8C, "s32_1000", "A", 2, "candidate"),
    Field("Battery", "State", 0x0201, "state_enum", "", 0, "candidate"),
    Field("Load output", "State", 0xEDA8, "load_state_enum", "", 0, "candidate"),
    Field("Load output", "Current", 0xEDAD, "u16_10", "A", 1, "candidate"),
    Field("Load output", "Power", 0xEDAA, "u16", "W", 0, "candidate"),
]

PRIMARY_REGS = [f.register for f in FIELDS]
FALLBACK_REGS = [0xED8F, 0xED8E]  # legacy current, generic power


def hx(data: bytes) -> str:
    return data.hex(" ")


def cbor_uint(n: int) -> bytes:
    if n < 0:
        raise ValueError("negative not supported")
    if n < 24:
        return bytes([n])
    if n <= 0xFF:
        return bytes([0x18, n])
    if n <= 0xFFFF:
        return bytes([0x19, (n >> 8) & 0xFF, n & 0xFF])
    return bytes([0x1A]) + n.to_bytes(4, "big")


def cbor_array_uints(values: list[int]) -> bytes:
    n = len(values)
    if n < 24:
        prefix = bytes([0x80 + n])
    elif n <= 0xFF:
        prefix = bytes([0x98, n])
    else:
        prefix = bytes([0x99]) + n.to_bytes(2, "big")
    return prefix + b"".join(cbor_uint(v) for v in values)


def get_values_request(instance: int, registers: list[int]) -> bytes:
    # VeSmartService::getValues: CBOR(opcode=5), CBOR(instance), CBOR(array(registers)).
    return cbor_uint(5) + cbor_uint(instance) + cbor_array_uints(registers)


def get_devices_request() -> bytes:
    return cbor_uint(1)


def subscribe_request(instance: int) -> bytes:
    # VeSmartService::subscribe(instance): CBOR(opcode=3), CBOR(instance).
    return cbor_uint(3) + cbor_uint(instance)


def negotiate_control_writes() -> list[bytes]:
    # Static analysis of VeSmartService::processControlData() showed that the
    # app writes fa 80 ff for chunk-size negotiation, then reports receive
    # credit with f9 80. The device replies with f9 01 credit notifications.
    return [bytes.fromhex("fa80ff"), bytes.fromhex("f980")]


def decode_one(data: bytes, i: int = 0) -> tuple[Any, int]:
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
    elif ai == 31 and major == 4:
        arr = []
        while data[i] != 0xFF:
            item, i = decode_one(data, i)
            arr.append(item)
        return arr, i + 1
    else:
        raise ValueError(f"unsupported CBOR major={major} ai={ai}")

    if major == 0:
        return value, i
    if major == 1:
        return -1 - value, i
    if major == 2:
        return {"bytes": readn(value).hex()}, i
    if major == 3:
        return readn(value).decode(errors="replace"), i
    if major == 4:
        arr = []
        for _ in range(value):
            item, i = decode_one(data, i)
            arr.append(item)
        return arr, i
    if major == 7:
        return {"simple": value}, i
    raise ValueError(f"unsupported CBOR major={major}")


def decode_stream(data: bytes) -> list[Any]:
    out = []
    i = 0
    while i < len(data):
        try:
            item, i = decode_one(data, i)
            out.append(item)
        except Exception as exc:  # keep raw tail for analysis
            out.append({"error": type(exc).__name__, "offset": i, "tail": data[i:].hex()})
            break
    return out


def extract_value_records(items: list[Any]) -> list[tuple[int, int, int, bytes]]:
    records = []
    i = 0
    while i + 3 < len(items):
        if (
            isinstance(items[i], int)
            and items[i] in (7, 8)
            and isinstance(items[i + 1], int)
            and isinstance(items[i + 2], int)
            and isinstance(items[i + 3], dict)
            and "bytes" in items[i + 3]
        ):
            records.append((items[i], items[i + 1], items[i + 2], bytes.fromhex(items[i + 3]["bytes"])))
            i += 4
        else:
            i += 1
    return records


def decode_raw(raw: bytes, decoder: str) -> Any:
    if decoder == "u16_100":
        return int.from_bytes(raw[:2], "little", signed=False) / 100
    if decoder == "u16_10":
        return int.from_bytes(raw[:2], "little", signed=False) / 10
    if decoder == "u16":
        return int.from_bytes(raw[:2], "little", signed=False)
    if decoder == "u32_100":
        return int.from_bytes(raw[:4], "little", signed=False) / 100
    if decoder == "s32_1000":
        return int.from_bytes(raw[:4], "little", signed=True) / 1000
    if decoder == "state_enum":
        code = raw[0]
        return STATE_NAMES.get(code, f"Unknown({code})")
    if decoder == "load_state_enum":
        code = raw[0]
        return LOAD_STATE_NAMES.get(code, f"Unknown({code})")
    if decoder == "s16_10":
        return int.from_bytes(raw[:2], "little", signed=True) / 10
    return raw.hex()


def format_value(value: Any, field: Field) -> str:
    if isinstance(value, str):
        return value
    if field.unit:
        return f"{value:.{field.decimals}f}{field.unit}"
    return str(value)


async def find_device(target: str, scan_time: float):
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


async def run(args: argparse.Namespace) -> int:
    found = await find_device(args.target, args.scan_time)
    if not found:
        print(json.dumps({
            "ok": False,
            "error": "target not advertising/found",
            "hint": "Close VictronConnect/other clients, keep the device awake/nearby, then retry.",
        }, indent=2))
        return 2

    _, rssi, device, adv = found
    print(f"target={device.name or adv.local_name!r} address={device.address} rssi={rssi}")

    latest: dict[tuple[int, int], bytes] = {}
    raw_events: list[dict[str, Any]] = []

    def on_notify(sender: Any, data: bytearray) -> None:
        uuid = getattr(sender, "uuid", str(sender))
        raw = bytes(data)
        items = decode_stream(raw)
        records = extract_value_records(items)
        for _, instance, register, value in records:
            latest[(instance, register)] = value
        raw_events.append({
            "uuid": uuid,
            "hex": raw.hex(),
            "items": items,
            "records": [
                {"type": typ, "instance": inst, "register": f"0x{reg:04x}", "raw": val.hex()}
                for typ, inst, reg, val in records
            ],
        })
        if args.verbose and records:
            print("notify", ", ".join(f"i{inst} 0x{reg:04x}={val.hex()}" for _, inst, reg, val in records))

    async with BleakClient(device, timeout=args.connect_timeout) as client:
        for uuid in (CTRL_UUID, DATA1_UUID, DATA2_UUID):
            await client.start_notify(uuid, on_notify)

        await asyncio.sleep(0.5)
        try:
            control = bytes(await client.read_gatt_char(CTRL_UUID))
            if args.verbose:
                print(f"control_initial={control.hex()}")
        except Exception as exc:
            if args.verbose:
                print(f"control_read_failed={type(exc).__name__}: {exc}")

        if not args.no_negotiate:
            for payload in negotiate_control_writes():
                if args.verbose:
                    print(f"writeControl bytes={payload.hex()}")
                await client.write_gatt_char(CTRL_UUID, payload, response=False)
                await asyncio.sleep(0.35)

        await client.write_gatt_char(DATA1_UUID, get_devices_request(), response=False)
        await asyncio.sleep(0.8)

        if not args.no_subscribe:
            sub = subscribe_request(args.instance)
            print(f"subscribe(instance={args.instance}) bytes={sub.hex()}")
            await client.write_gatt_char(DATA1_UUID, sub, response=False)
            await asyncio.sleep(args.subscribe_wait)

        registers = PRIMARY_REGS + ([] if args.no_fallbacks else FALLBACK_REGS)
        request = get_values_request(args.instance, registers)
        print(f"getValues(instance={args.instance}, regs={[hex(r) for r in registers]}) bytes={request.hex()}")
        await client.write_gatt_char(DATA1_UUID, request, response=False)

        start = time.monotonic()
        while time.monotonic() - start < args.listen_time:
            await asyncio.sleep(1)
            if args.repeat:
                await client.write_gatt_char(DATA1_UUID, request, response=False)

    rows = []
    by_register = {field.register: field for field in FIELDS}
    for field in FIELDS:
        raw = latest.get((args.instance, field.register))
        decoded = None
        formatted = None
        if raw is not None:
            decoded = decode_raw(raw, field.decoder)
            formatted = format_value(decoded, field)
        rows.append({
            "section": field.section,
            "label": field.label,
            "register": f"0x{field.register:04x}",
            "instance": args.instance,
            "raw": None if raw is None else raw.hex(),
            "value": formatted,
            "confidence": field.confidence,
        })

    # Derived fallbacks.
    solar_v = latest.get((args.instance, 0xEDBB))
    solar_p = latest.get((args.instance, 0xEDBC))
    if solar_v and solar_p and latest.get((args.instance, 0xEDBD)) is None:
        volts = decode_raw(solar_v, "u16_100")
        watts = decode_raw(solar_p, "u32_100")
        amps = watts / volts if volts else None
        rows.append({"section": "Solar", "label": "Current derived", "register": "0xEDBC/0xEDBB", "raw": f"{solar_p.hex()}/{solar_v.hex()}", "value": None if amps is None else f"{amps:.1f}A", "confidence": "derived"})

    bat_v = latest.get((args.instance, 0xED8D))
    load_i = latest.get((args.instance, 0xEDAD))
    if bat_v and load_i and latest.get((args.instance, 0xEDAA)) is None:
        volts = decode_raw(bat_v, "u16_100")
        amps = decode_raw(load_i, "u16_10")
        rows.append({"section": "Load output", "label": "Power derived", "register": "0xED8D*0xEDAD", "raw": f"{bat_v.hex()}*{load_i.hex()}", "value": f"{volts * amps:.0f}W", "confidence": "derived"})

    result = {"ok": True, "target": device.name or adv.local_name, "address": device.address, "rows": rows}
    if args.json_events:
        result["events"] = raw_events
    print(json.dumps(result, indent=2))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", default="Solar Charger")
    parser.add_argument("--instance", type=int, default=3)
    parser.add_argument("--scan-time", type=float, default=12)
    parser.add_argument("--listen-time", type=float, default=10)
    parser.add_argument("--connect-timeout", type=float, default=30)
    parser.add_argument("--repeat", action="store_true", help="repeat getValues every second while listening")
    parser.add_argument("--no-fallbacks", action="store_true")
    parser.add_argument("--no-negotiate", action="store_true", help="skip Control fa80ff/f980 negotiation writes")
    parser.add_argument("--no-subscribe", action="store_true", help="skip subscribe(instance) before getValues")
    parser.add_argument("--subscribe-wait", type=float, default=2.0, help="seconds to wait after subscribe before getValues")
    parser.add_argument("--json-events", action="store_true")
    parser.add_argument("--verbose", action="store_true")
    return asyncio.run(run(parser.parse_args()))


if __name__ == "__main__":
    raise SystemExit(main())
