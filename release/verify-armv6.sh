#!/usr/bin/env bash
#
# verify-armv6.sh — hard verification for ARMv6 (Raspberry Pi Zero W)
# release/check artifacts. Exits non-zero if any required attribute is
# missing or wrong. No error swallowing: every check prints its result and
# failures accumulate into the exit code.
#
# Usage:
#   verify-armv6.sh BIN [--mode release|check] [--glibc-floor 2.31]
#
#   BIN   path to the ARM ELF artifact to verify.
#   --mode release (default): full checks — EABI5, hard-float ABI, standard
#         loader /lib/ld-linux-armhf.so.3, ARMv6-family objects, no
#         ARMv7-only instructions, minimum referenced GLIBC symbol version.
#   --mode check: for Nix `cargo check` artifacts (never deployable) —
#         object/instruction checks only; loader and glibc-floor are skipped
#         because the Nix toolchain links a /nix/store glibc by design.
#
# Release artifacts must carry merged ARMv6 + Thumb-1 + VFPv2 attributes.
# A prior verifier tolerated generic Debian ARMv7 startup objects and missed
# Thumb-2 `add.w` emitted by crtbeginS; that binary SIGILLed before main on a
# Pi Zero W. The release toolchain now links a pinned Raspbian ARMv6 sysroot,
# so any merged ARMv7/Thumb-2 attribute is an unconditional failure.

set -u

BIN="${1:-}"
MODE="release"
GLIBC_FLOOR="2.31"

while [ $# -gt 0 ]; do
  case "$1" in
    --mode) MODE="${2:-release}"; shift 2 ;;
    --glibc-floor) GLIBC_FLOOR="${2:-2.31}"; shift 2 ;;
    *) BIN="$1"; shift ;;
  esac
done

if [ -z "$BIN" ] || [ ! -f "$BIN" ]; then
  echo "verify-armv6: usage: $0 BIN [--mode release|check] [--glibc-floor N]" >&2
  echo "verify-armv6: missing or unreadable artifact: '${BIN:-}'" >&2
  exit 2
fi

# Resolve to an absolute path so relative rlib paths keep working after
# subshells cd into temp extraction directories.
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

# ---- tool discovery (no swallowing: missing tools are an error) ----------
find_tool() {
  local name="$1"; shift
  for c in "$@"; do
    if command -v "$c" >/dev/null 2>&1; then echo "$c"; return 0; fi
  done
  echo "verify-armv6: required tool '$name' not found (tried: $*)" >&2
  return 1
}

READELF="$(find_tool readelf arm-linux-gnueabihf-readelf armv6l-unknown-linux-gnueabihf-readelf llvm-readelf)" || exit 2
OBJDUMP="$(find_tool objdump arm-linux-gnueabihf-objdump armv6l-unknown-linux-gnueabihf-objdump llvm-objdump)" || exit 2
FILE_BIN="$(find_tool file file)" || exit 2

FAIL=0
ok()   { printf '  PASS  %s\n' "$1"; }
bad()  { printf '  FAIL  %s\n' "$1"; FAIL=1; }

echo "verify-armv6: mode=${MODE} artifact=${BIN}"
echo "  tools: readelf=${READELF} objdump=${OBJDUMP}"

# ---- 1. container / file-level -------------------------------------------
echo "[1] ELF identification"
FILE_OUT="$("$FILE_BIN" "$BIN")"
echo "      file: ${FILE_OUT}"
case "$FILE_OUT" in
  *"ELF 32-bit"*"ARM"*) ok "ELF 32-bit ARM" ;;
  *) bad "not an ELF 32-bit ARM binary" ;;
esac
case "$FILE_OUT" in
  *"EABI5"*) ok "EABI5" ;;
  *) bad "EABI5 missing (wrong ABI convention)" ;;
esac

# ---- 2. program interpreter (release only) --------------------------------
echo "[2] program interpreter"
INTERP="$("$READELF" -l "$BIN" 2>/dev/null | grep -oE '/[a-zA-Z0-9./_-]+' | head -1)"
echo "      interp: ${INTERP:-<none>}"
if [ "$MODE" = "release" ]; then
  if [ "$INTERP" = "/lib/ld-linux-armhf.so.3" ]; then
    ok "standard loader /lib/ld-linux-armhf.so.3"
  else
    bad "release artifact must use /lib/ld-linux-armhf.so.3 (got: '${INTERP}')"
  fi
else
  echo "      (skipped loader check: --mode check, Nix artifacts link a /nix/store glibc)"
fi

# ---- 3. merged attribute section -----------------------------------------
echo "[3] merged ELF attributes"
ATTRS="$("$READELF" -A "$BIN" 2>/dev/null)"
CPU_ARCH="$(printf '%s\n' "$ATTRS" | grep -iE 'CPU_arch:' | head -1 | sed -E 's/.*arch:[[:space:]]*//I; s/ARM[[:space:]]*//I; s/[^a-zA-Z0-9]//g')"
THUMB_ARCH="$(printf '%s\n' "$ATTRS" | grep -iE 'THUMB_ISA_use:' | head -1 | sed -E 's/.*use:[[:space:]]*//I; s/[^a-zA-Z0-9-]//g')"
FP_ARCH="$(printf '%s\n' "$ATTRS" | grep -iE 'FP_arch:' | head -1 | sed -E 's/.*arch:[[:space:]]*//I; s/[^a-zA-Z0-9-]//g')"
VFP_ARGS="$(printf '%s\n' "$ATTRS" | grep -iE 'VFP_args' | head -1 | sed -E 's/.*VFP_args:[[:space:]]*//I; s/[^a-zA-Z0-9-]//g')"
echo "      Tag_CPU_arch=${CPU_ARCH:-<none>} Tag_THUMB_ISA_use=${THUMB_ARCH:-<none>} Tag_FP_arch=${FP_ARCH:-<none>} Tag_ABI_VFP_args=${VFP_ARGS:-<none>}"
case "$VFP_ARGS" in
  *VFP*) ok "hard-float ABI (VFP args)" ;;
  *) bad "hard-float ABI required (Tag_ABI_VFP_args shows '${VFP_ARGS}')" ;;
esac
if [ "$MODE" = "release" ]; then
  case "$CPU_ARCH" in
    v6|v6K|v6KZ) ok "merged Tag_CPU_arch is ARMv6-family" ;;
    *) bad "release artifact must be ARMv6-family (got '${CPU_ARCH}')" ;;
  esac
  case "$THUMB_ARCH" in
    Thumb-1|Thumb1) ok "merged Thumb ISA is Thumb-1" ;;
    *) bad "release artifact must not contain Thumb-2 (got '${THUMB_ARCH}')" ;;
  esac
  case "$FP_ARCH" in
    VFPv2) ok "merged FP architecture is VFPv2" ;;
    *) bad "release artifact must use VFPv2 (got '${FP_ARCH}')" ;;
  esac
fi

# ---- 4. object-level: reject explicit incompatible attributes -------------
# Thin-LTO Rust codegen objects frequently omit .ARM.attributes entirely;
# absence at this intermediate level is not evidence of a wrong ISA. The
# final merged ELF attributes above remain mandatory and fail-closed. Here we
# reject every object that *does* declare an incompatible CPU/Thumb/FP/ABI.
echo "[4] object-level attributes (explicit declarations inside every .rlib)"
DEPS_DIR="$(dirname "$BIN")/deps"
if [ -d "$DEPS_DIR" ]; then
  N_RLIB=0; N_BAD=0; N_TAGGED=0; N_UNTAGGED=0
  for rlib in "$DEPS_DIR"/*.rlib; do
    [ -e "$rlib" ] || continue
    N_RLIB=$((N_RLIB + 1))
    work="$(mktemp -d)" || { bad "mktemp failed"; break; }
    if ! (cd "$work" && ar x "$rlib" >/dev/null 2>&1); then
      bad "cannot extract ${rlib}"
      N_BAD=$((N_BAD+1))
      rm -rf "$work"; continue
    fi
    for obj in "$work"/*.o; do
      [ -e "$obj" ] || continue
      oa="$("$READELF" -A "$obj" 2>/dev/null)"
      o_cpu="$(printf '%s\n' "$oa" | grep -iE 'CPU_arch:' | head -1 | sed -E 's/.*arch:[[:space:]]*//I; s/ARM[[:space:]]*//I; s/[^a-zA-Z0-9]//g')"
      o_thumb="$(printf '%s\n' "$oa" | grep -iE 'THUMB_ISA_use:' | head -1 | sed -E 's/.*use:[[:space:]]*//I; s/[^a-zA-Z0-9-]//g')"
      o_fp="$(printf '%s\n' "$oa" | grep -iE 'FP_arch:' | head -1 | sed -E 's/.*arch:[[:space:]]*//I; s/[^a-zA-Z0-9-]//g')"
      o_vfp="$(printf '%s\n' "$oa" | grep -iE 'VFP_args' | head -1 | sed -E 's/.*VFP_args:[[:space:]]*//I; s/[^a-zA-Z0-9-]//g')"
      if [ -z "$o_cpu$o_thumb$o_fp$o_vfp" ]; then
        N_UNTAGGED=$((N_UNTAGGED+1))
        continue
      fi
      N_TAGGED=$((N_TAGGED+1))
      if [ -n "$o_cpu" ] && ! printf '%s' "$o_cpu" | grep -qiE '^(v6|v6K|v6KZ)$'; then
        bad "object ${obj##*/} in ${rlib##*/}: explicit CPU_arch '${o_cpu}' is incompatible"; N_BAD=$((N_BAD+1))
      fi
      if [ -n "$o_thumb" ] && ! printf '%s' "$o_thumb" | grep -qiE '^(Thumb-1|Thumb1)$'; then
        bad "object ${obj##*/} in ${rlib##*/}: explicit Thumb ISA '${o_thumb}' is incompatible"; N_BAD=$((N_BAD+1))
      fi
      if [ -n "$o_fp" ] && ! printf '%s' "$o_fp" | grep -qiE '^VFPv2$'; then
        bad "object ${obj##*/} in ${rlib##*/}: explicit FP_arch '${o_fp}' is incompatible"; N_BAD=$((N_BAD+1))
      fi
      if [ -n "$o_vfp" ] && ! printf '%s' "$o_vfp" | grep -qiE 'VFP'; then
        bad "object ${obj##*/} in ${rlib##*/}: explicit ABI_VFP_args '${o_vfp}' is incompatible"; N_BAD=$((N_BAD+1))
      fi
    done
    rm -rf "$work"
  done
  if [ "$N_RLIB" -gt 0 ] && [ "$N_BAD" -eq 0 ]; then
    ok "${N_RLIB} rlibs scanned: ${N_TAGGED} tagged objects compatible; ${N_UNTAGGED} untagged Thin-LTO objects defer to mandatory final ELF checks"
  elif [ "$N_RLIB" -eq 0 ]; then
    echo "      (no rlibs found in ${DEPS_DIR} — nothing to scan)"
  fi
else
  echo "      (no deps dir ${DEPS_DIR} — nothing to scan)"
fi

# ---- 5. instruction-level: no ARMv7-only / NEON instructions --------------
echo "[5] instruction scan (objdump-selected ARM/Thumb modes)"
DIS_ARM="$("$OBJDUMP" -d "$BIN" 2>/dev/null)"
N_VIOL="$(printf '%s\n' "$DIS_ARM" | grep -cE '[[:space:]](movw|movt|sdiv|udiv|cbz|cbnz)[[:space:]]|[[:space:]]add\.w[[:space:]]|[[:space:]]sub\.w[[:space:]]' || true)"
N_NEON="$(printf '%s\n' "$DIS_ARM" | grep -cE '[[:space:]]v(ld[124]|st[124]|tbl|tbx|zip|uzp|trn|rev|ext|swp|qdmulh|qdmlal|abal|pad|pmin|pmax|rhadd)[[:space:]]|q[0-9]+,|,q[0-9]+' || true)"
echo "      v7/Thumb-2-only patterns: ${N_VIOL}; NEON patterns: ${N_NEON}"
if [ "$N_VIOL" -eq 0 ] && [ "$N_NEON" -eq 0 ]; then
  ok "no ARMv7/Thumb-2-only or NEON instructions in linked text"
else
  bad "ARMv7/Thumb-2-only (${N_VIOL}) and/or NEON (${N_NEON}) instructions present — not runnable on ARM1176JZF-S"
fi

# ---- 6. minimum referenced GLIBC symbol version (release only) ------------
echo "[6] minimum referenced GLIBC symbol version (floor: ${GLIBC_FLOOR})"
if [ "$MODE" = "release" ]; then
  # Some otherwise capable cross-objdump builds do not print the dynamic
  # symbol table for a foreign ARM executable. GNU readelf's version-needs
  # output is an equally authoritative source and keeps this check portable
  # without accepting a missing version contract.
  VERSION_TEXT="$("$OBJDUMP" -T "$BIN" 2>/dev/null)"
  if ! printf '%s\n' "$VERSION_TEXT" | grep -qE 'GLIBC_[0-9]+\.[0-9]+'; then
    VERSION_TEXT="$("$READELF" --version-info "$BIN" 2>/dev/null)"
  fi
  FLOOR="$(printf '%s\n' "$VERSION_TEXT" | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sort -V | tail -1 | sed 's/GLIBC_//')"
  if [ -z "$FLOOR" ]; then
    bad "no GLIBC_* version requirement found in dynamic symbols or version-needs metadata"
  else
    echo "      max referenced GLIBC_* symbol version: ${FLOOR}"
    if printf '%s\n%s\n' "$GLIBC_FLOOR" "$FLOOR" | sort -V | head -1 | grep -qx "$FLOOR"; then
      ok "glibc floor ${FLOOR} <= target ${GLIBC_FLOOR}"
    else
      bad "glibc floor ${FLOOR} exceeds target ${GLIBC_FLOOR} — won't run on the chosen Pi OS baseline"
    fi
  fi
else
  echo "      (skipped: --mode check; Nix glibc 2.42 is not a Pi OS baseline)"
fi

echo
if [ "$FAIL" -eq 0 ]; then
  echo "verify-armv6: ALL CHECKS PASSED (mode=${MODE})"
  exit 0
fi
echo "verify-armv6: VERIFICATION FAILED (mode=${MODE})"
exit 1
