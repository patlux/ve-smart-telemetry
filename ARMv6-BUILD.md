# ARMv6 cross-build (Raspberry Pi Zero W)

Target hardware: Raspberry Pi Zero W, ARM1176JZF-S CPU — **ARMv6**, 32-bit,
hard-float Linux, Thumb-1/VFPv2, no NEON. Primary Rust target:
`arm-unknown-linux-gnueabihf`. Do not assume generic Debian armhf or ARMv7
Raspberry Pi binaries run on the Pi Zero W.

**There are two ARMv6 build paths in this workspace and they must never be
confused:**

| Path | Toolchain | Artifact |
|---|---|---|
| `make check-armv6` | Nix dev shell (`flake.nix`) | **compile/codegen validation only** — links nixpkgs glibc **2.42** with a `/nix/store` loader, **NOT deployable** to stock Pi OS |
| `make release-armv6` | `release/Dockerfile` plus pinned Raspberry Pi OS Bullseye ARMv6 sysroot | **Pi-Zero-compatible release artifact** — merged ARMv6/Thumb-1/VFPv2 attributes, standard loader `/lib/ld-linux-armhf.so.3`, glibc floor ≤ 2.31 |

`make build-armv6` intentionally refuses to run (`exit 2`) so nobody
silently produces an undeployable binary.

## Why the split (measured evidence)

- Nix cross build (nixpkgs `pkgsCross.raspberryPi`, glibc 2.42): the dynamic
  loader is a nix-store path
  (`/nix/store/.../glibc-armv6l-unknown-linux-gnueabihf-2.42-.../lib/ld-linux-armhf.so.3`)
  and glibc is newer than stock Pi OS. Such a binary cannot run on stock
  Raspberry Pi OS without invasive patching → **validation only**.
- Generic Debian Bullseye armhf supplies ARMv7/Thumb-2 startup and GCC CRT
  objects. A release built that way carried merged `Tag_CPU_arch: v7`,
  `Tag_THUMB_ISA_use: Thumb-2`, and `Tag_FP_arch: VFPv3-D16`. On the original
  Pi Zero W it failed before `main` with `Segmentation fault` or
  `Illegal instruction`. An instruction scan that ignored startup objects
  was therefore insufficient.
- The authoritative release container now downloads exact, SHA-256-pinned
  Raspberry Pi OS Bullseye armhf packages into a dedicated sysroot. It puts
  the Raspbian ARMv6 `Scrt1.o`, `crtbeginS.o`, libc, libgcc, D-Bus and related
  libraries ahead of the Debian cross compiler's ARMv7 objects. Absolute
  sysroot symlinks are rewritten to remain within the sysroot.

Verified on the real original Pi Zero W:

```text
victron-collector 0.1.0
victron-cli 0.1.0
Tag_CPU_arch: v6
Tag_THUMB_ISA_use: Thumb-1
Tag_FP_arch: VFPv2
Tag_ABI_VFP_args: VFP registers
interpreter: /lib/ld-linux-armhf.so.3
maximum required GLIBC version: 2.31 or older
```

These merged attributes are a fail-closed release contract. ARMv7,
Thumb-2, VFPv3, absent required attributes, a non-standard loader, or a
newer GLIBC requirement must fail `release/verify-armv6.sh`; they must not
be treated as harmless metadata.

## Release container (`make release-armv6`)

Base image pinned by digest (verified 2026-08-09):
`debian:bullseye-slim@sha256:f313b4bd62667092a59b3a664d7d3ab8b5e65f41675f48e81455a15dc5abe792`.
The base provides host-side build and cross tools only. Target CRT, libc,
libgcc, D-Bus and related libraries come from exact Raspberry Pi OS
Bullseye package paths and SHA-256 values in
`release/install-raspbian-sysroot.sh`.

What it provides:

- `crossbuild-essential-armhf` + `build-essential`: the host-side
  `arm-linux-gnueabihf-gcc` driver and native compiler for build scripts.
- Pinned Raspberry Pi OS Bullseye ARMv6 hard-float sysroot, including
  glibc 2.31, GCC 10 CRT objects, D-Bus development files and runtime
  dependencies.
- `release/build-armv6.sh`: explicit `--sysroot` and `-B` ordering, target
  pkg-config wiring, `target-cpu=arm1176jzf-s`, and `--locked` Cargo build.
- Rust **1.97.1** with `arm-unknown-linux-gnueabihf` target std — matches
  `rust-toolchain.toml`.
- SQLite bundled via `rusqlite`; no system SQLite runtime dependency.

Usage:

```console
$ make release-armv6            # docker build + cargo build --release --target
$ make verify-armv6 BIN=target/arm-unknown-linux-gnueabihf/release/<binary>
$ make strip-armv6  BIN=target/arm-unknown-linux-gnueabihf/release/<binary>   # optional, cross strip
$ make release-shell            # interactive container shell
```

`verify-armv6` is hard-failing (no error swallowing): missing/wrong EABI5,
standard loader, merged ARMv6/Thumb-1/VFPv2/hard-float attributes, unsafe
object attributes, ARMv7/Thumb-2/NEON instructions, or an excessive GLIBC
floor abort with exit 1.

**Residual supply-chain limitation (honest):** the base-image digest and all
Raspbian target-package paths/hashes are pinned. Host-side apt tools and the
Rust toolchain are still fetched at image-build time and are not fully
byte-reproducible across dates. Full byte reproducibility would additionally
mirror and hash the host apt pool and Rust distribution artifacts.

## Nix dev shell (`make check-armv6` / `nix develop`)

Purpose: `cargo check --target arm-unknown-linux-gnueabihf` — type/codegen
validation with a pinned toolchain. Provides:

- Rust **1.97.1** + ARMv6 target std (rust-overlay), matching
  `rust-toolchain.toml` and the release container.
- ARMv6 cross GCC (nixpkgs `pkgsCross.raspberryPi`) under the conventional
  name `arm-linux-gnueabihf-gcc`, injected as the cargo linker.
- ARMv6 D-Bus pkg-config wiring for bluer: `dbusArmv6Pc` (a runCommand that
  gathers `dbus-1.pc` from the nixpkgs cross `dbus` outputs) plus
  `PKG_CONFIG_ALLOW_CROSS=1`, `PKG_CONFIG_SYSROOT_DIR=/`,
  `PKG_CONFIG_LIBDIR=<dbusArmv6Pc>/lib/pkgconfig`, so `libdbus-sys`'s
  pkg-config probe can resolve against the ARMv6 target.
- `pkg-config`, `file`, `llvm` (llvm-readelf), cross binutils.

**Verified limitation (2026-08-09, macOS/Darwin host):** the armv6l cross
closure of nixpkgs `dbus` includes `tcl`, and nixpkgs' `tcl` cross build
fails on Darwin hosts — the macOS SDK header `mach/mach_time.h` leaks into
the armv6l-linux compile (`fatal error: mach/mach_time.h: No such file or
directory` in `tclUnixTime.c`), failing the whole dbus → systemd-minimal
closure. This was verified with an actual build attempt; it is a nixpkgs
cross bug, not a workspace issue. Consequences:

- `dbusArmv6Pc` is **not** in the default shell's packages on Darwin, so
  `nix develop` works. The default shell sets only the cross pkg-config
  preconditions, not `PKG_CONFIG_LIBDIR`.
- Checking the bluez crate on **any host** can go through the release
  container (`make check-linux` / `release-shell`), which is authoritative
  for anything needing libdbus.
- On Linux, build `.#dbus-armv6-pc` and export its `lib/pkgconfig` path as
  `PKG_CONFIG_LIBDIR` for an optional Nix-side target check. This path has
  not been verified on this macOS machine.

## Host checks and bluer

`bluer` is **Linux-only** (BlueZ D-Bus). The assembled workspace therefore
uses the Linux container for full workspace checks and tests; do not treat a
macOS host-only workspace run as authoritative:

```console
$ make check-linux    # cargo check --workspace inside the release container
$ make test-linux     # cargo test  --workspace inside the release container
```

## Artifact verification

```console
$ make verify-armv6 BIN=target/arm-unknown-linux-gnueabihf/release/<binary>   # release (full checks)
$ make verify-armv6-check BIN=target/arm-unknown-linux-gnueabihf/debug/...     # nix check artifact
```

Checks (all hard-failing):

1. ELF 32-bit ARM, EABI5, dynamically linked (`file`)
2. interpreter `/lib/ld-linux-armhf.so.3` (release mode)
3. hard-float ABI: `Tag_ABI_VFP_args` = VFP
4. merged release attributes: ARMv6-family + Thumb-1 + VFPv2 + VFP registers
5. object/instruction level: reject unsafe ARMv7/Thumb-2/NEON code
6. minimum referenced `GLIBC_*` symbol version ≤ floor (release mode)

## Release profile

```toml
[profile.release]
opt-level = "s"
lto = "thin"
codegen-units = 1
panic = "abort"
strip = "symbols"
```

`panic = "abort"` means `cargo test --release` is unsupported; run tests with
the dev profile. Measure startup time, RSS, CPU and binary size on hardware
before further tuning.

## Residual Phase-0 hardware requirements

Release CPU/runtime compatibility has been verified on a physical original
Pi Zero W running Raspberry Pi OS armhf. Remaining end-to-end hardware work
requires:

1. successful BlueZ bond with the current Victron Bluetooth PIN,
2. repeated direct BLE negotiation/subscription/read cycles,
3. reachable VictoriaMetrics ingestion endpoint,
4. systemd, SQLite recovery/spool replay, and listener-freedom checks.

The ELF attributes and both binaries' `--version` output have already been
verified on-device. Remaining hardware verification is BLE read/reconnect,
SQLite recovery/spool replay, VictoriaMetrics delivery, systemd operation,
and listener-freedom.

## Cargo.lock / members

The assembled workspace has real members and a root `Cargo.lock`. Release
builds use `--locked`. Crate-local lockfiles remain forbidden.
