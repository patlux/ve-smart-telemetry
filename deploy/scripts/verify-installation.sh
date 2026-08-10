#!/usr/bin/env bash
# verify-installation.sh — read-only verification of a victron-collector
# installation on the Pi. Performs NO mutations: no files are written, no
# service is started or stopped, no metrics are written to VictoriaMetrics
# (the reachability probe is an empty POST, see lib.sh).
set -euo pipefail
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
  cat <<'EOF'
Verify a victron-collector installation on Raspberry Pi OS.

Usage:
  verify-installation.sh [options]

Options:
  --strict                 Treat warnings as failures (exit 1)
  --config PATH            Config to inspect (default: /etc/victron-collector/config.toml)
  --no-net                 Skip the VictoriaMetrics reachability probe
  -h, --help               Show this help

Checks: host arch, ARMv6 binary identity/linkage (CPU arch tag + hard-float
ABI), CLI, accounts, directories, unit state, config (incl. forbidden
credential keys), database, BLE adapter, VictoriaMetrics reachability, and
absence of listening TCP/UDP sockets owned by the collector (PID-based).

This script verifies the local installation and is NOT an external probe.
For the unintended-path assertion (endpoint unreachable from an off-tailnet
host), use the read-only, network-only script:

  deploy/scripts/exposure-check.sh --unreachable URL

Exit codes: 0 all checks passed; 1 any failure; 2 warnings only (no failures).
With --strict, warnings become failures (exit 1).
EOF
}

STRICT=0 NO_NET=0
while (($#)); do
  case $1 in
  --strict)
    STRICT=1
    shift
    ;;
  --config)
    need_arg "$@"
    VICTRON_CONFIG=$2
    shift 2
    ;;
  --no-net)
    NO_NET=1
    shift
    ;;
  -h | --help)
    usage
    exit 0
    ;;
  *) die "unknown option: $1 (see --help)" ;;
  esac
done

# --- 1. host architecture -------------------------------------------------
arch=$(uname -m 2>/dev/null || echo unknown)
case "$arch" in
armv6l) result "host architecture" pass "armv6l — native Pi Zero W" ;;
armv7l | armhf | aarch64)
  result "host architecture" warn "$arch — ARMv6 binaries run only with 32-bit compat"
  ;;
*) result "host architecture" warn "$arch — unexpected for a Raspberry Pi" ;;
esac

# --- 2. binaries ----------------------------------------------------------
BIN="$VICTRON_BIN_DIR/$VICTRON_BINARY"
if [[ ! -x $BIN ]]; then
  result "collector binary" fail "not found or not executable: $BIN (run install-release.sh)"
else
  if ! command -v file >/dev/null 2>&1; then
    result "collector binary" warn "file(1) not found (install file/binutils) — identity check skipped"
  elif file -b "$BIN" 2>/dev/null | grep -qE 'ELF 32-bit.*ARM'; then
    result "collector binary" pass "$BIN is a 32-bit ARM ELF"
  else
    result "collector binary" fail "$BIN is not a 32-bit ARM ELF"
  fi
  if ! command -v readelf >/dev/null 2>&1; then
    result "CPU arch tag" warn "readelf not found (install binutils) — arch check skipped"
    result "hard-float ABI" warn "readelf not found (install binutils) — VFP check skipped"
  else
    arch_tag=$(readelf -A "$BIN" 2>/dev/null | awk '/Tag_CPU_arch:/{print $2; exit}')
    if is_armv6_arch_tag "$arch_tag"; then
      result "CPU arch tag" pass "$arch_tag — ARMv6 (Pi Zero W: ARMv6KZ-class, no Thumb-2)"
    else
      result "CPU arch tag" fail "${arch_tag:-missing} — must be ARMv6 for Pi Zero W (v6T2/Thumb-2 and v7+ rejected)"
    fi
    vfp_tag=$(readelf -A "$BIN" 2>/dev/null | awk '/Tag_ABI_VFP_args:/{print $2; exit}')
    if is_hard_float_vfp_tag "$vfp_tag"; then
      result "hard-float ABI" pass "Tag_ABI_VFP_args=VFP"
    else
      result "hard-float ABI" fail "${vfp_tag:-missing} — expected VFP (hard-float arm-unknown-linux-gnueabihf build)"
    fi
  fi
  if ! command -v ldd >/dev/null 2>&1; then
    result "linkage" warn "ldd not found (install libc-bin) — linkage check skipped"
  else
    missing=$(ldd "$BIN" 2>/dev/null | awk '/not found/{print $1; exit}') || true
    if [[ -n $missing ]]; then
      result "linkage" fail "unresolved library: $missing"
    else
      result "linkage" pass "all shared libraries resolved"
    fi
  fi
  if "$BIN" --version >/dev/null 2>&1; then
    result "binary --version" pass ""
  else
    result "binary --version" fail "exit code nonzero or unsupported flag"
  fi
fi

CLI="$VICTRON_BIN_DIR/$VICTRON_CLI"
if [[ -x $CLI ]]; then
  result "diagnostic CLI" pass "$CLI present"
else
  result "diagnostic CLI" warn "not installed ($CLI) — optional"
fi

# --- 3. accounts and directories ------------------------------------------
if id "$VICTRON_USER" >/dev/null 2>&1; then
  result "service user" pass "$VICTRON_USER exists"
else
  result "service user" fail "missing (run install-release.sh)"
fi
if getent group "$VICTRON_GROUP" >/dev/null 2>&1; then
  result "service group" pass "$VICTRON_GROUP exists"
else
  result "service group" fail "missing"
fi

if [[ -d $VICTRON_STATE_DIR ]]; then
  mode=$(stat -c '%U:%G %a' "$VICTRON_STATE_DIR" 2>/dev/null || echo unknown)
  result "state directory" pass "$VICTRON_STATE_DIR ($mode)"
  if [[ $EUID -eq 0 ]]; then
    if sudo -u "$VICTRON_USER" test -w "$VICTRON_STATE_DIR"; then
      result "state directory writable" pass "by $VICTRON_USER"
    else
      result "state directory writable" fail "$VICTRON_USER cannot write $VICTRON_STATE_DIR"
    fi
  else
    result "state directory writable" warn "skipped (run as root for permission checks)"
  fi
else
  result "state directory" fail "missing: $VICTRON_STATE_DIR"
fi
if [[ -d $VICTRON_ETC_DIR ]]; then
  result "config directory" pass "$VICTRON_ETC_DIR exists"
else
  result "config directory" fail "missing: $VICTRON_ETC_DIR"
fi

# --- 4. systemd unit -------------------------------------------------------
UNIT="$VICTRON_UNIT_DIR/$VICTRON_UNIT"
if [[ -f $UNIT ]]; then
  if grep -q "ExecStart=.*$VICTRON_BIN_DIR/$VICTRON_BINARY" "$UNIT"; then
    result "unit ExecStart" pass "points at $BIN"
  else
    result "unit ExecStart" fail "does not reference $BIN"
  fi
  if systemd-analyze verify "$UNIT" >/dev/null 2>&1; then
    result "unit syntax" pass "systemd-analyze verify clean"
  else
    result "unit syntax" warn "systemd-analyze verify reported issues (see full output below)"
    systemd-analyze verify "$UNIT" 2>&1 | sed 's/^/  /' || true
  fi
  if grep -q '^Type=notify$' "$UNIT" && grep -q '^NotifyAccess=main$' "$UNIT" &&
    grep -qE '^WatchdogSec=[1-9][0-9]*$' "$UNIT" && grep -q '^WatchdogSignal=SIGKILL$' "$UNIT"; then
    result "unit watchdog contract" pass "Type=notify, NotifyAccess=main, finite WatchdogSec, immediate stuck-process kill"
  else
    result "unit watchdog contract" fail "expected Type=notify, NotifyAccess=main, positive WatchdogSec and WatchdogSignal=SIGKILL"
  fi
else
  result "systemd unit" fail "missing: $UNIT"
fi
if svc_is_enabled; then
  result "service enabled" pass ""
else
  result "service enabled" fail "not enabled (systemctl enable $VICTRON_SERVICE)"
fi
if svc_is_active; then
  result "service active" pass ""
else
  result "service active" fail "not running (systemctl status $VICTRON_SERVICE)"
fi

# --- 5. configuration ------------------------------------------------------
if [[ -f $VICTRON_CONFIG ]]; then
  # check_config enforces: no credential keys, binary parse validation
  # (installed collector; textual sanity only when no binary exists), and
  # storage.path confinement under the unit's ReadWritePaths. A failure
  # here is a hard config failure, not a warning.
  if (check_config "$VICTRON_CONFIG"); then
    result "config" pass "no credential keys; parses with $VICTRON_BINARY (or textual sanity); storage.path confined"
  else
    result "config" fail "see error above (credential key, parse failure, or storage.path outside $VICTRON_STATE_DIR)"
  fi
  url=''
  if ! url=$(config_vm_url "$VICTRON_CONFIG"); then
    result "config url" fail "missing or malformed [victoria_metrics] url string"
  elif parse_http_url "$url"; then
    result "config url" pass "$URL_SCHEME://$URL_HOST:$URL_PORT$URL_PATH"
    if ipv4_in_cgnat "$URL_HOST"; then
      result "url route" pass "$URL_HOST is inside tailnet CGNAT 100.64.0.0/10 (numeric check)"
    else
      result "url route" warn "$URL_HOST is outside 100.64.0.0/10 (or not an IPv4 address) — the deny-by-default egress allowlist (IPAddressAllow + IPAddressDeny=any) would block it"
    fi
  else
    result "config url" fail "invalid URL: $url (must be plain http://host:port/absolute/path; no https, userinfo, query/fragment, whitespace, or invalid port)"
  fi
else
  result "configuration" fail "missing: $VICTRON_CONFIG"
fi

# --- 6. database -----------------------------------------------------------
if [[ -f $VICTRON_DB ]]; then
  check_db "$VICTRON_DB"
else
  result "database" warn "not created yet ($VICTRON_DB appears after first successful run)"
fi

# --- 7. BLE adapter --------------------------------------------------------
adapter=hci0
if [[ -f $VICTRON_CONFIG ]]; then
  adapter=$(config_ble_adapter "$VICTRON_CONFIG") || adapter=hci0
fi
if command -v bluetoothctl >/dev/null 2>&1; then
  out=$(bluetoothctl --timeout 10 list 2>/dev/null) || true
  if grep -q '^Controller' <<<"$out"; then
    result "BLE adapter" pass "$adapter visible (bluetoothctl list)"
  else
    result "BLE adapter" fail "no controller listed — check bluetoothd (systemctl status bluetooth)"
  fi
else
  result "BLE adapter" fail "bluetoothctl not found"
fi

# --- 8. VictoriaMetrics reachability ---------------------------------------
if ((!NO_NET)); then
  # Probe only when the config URL passed the strict parser (section 5).
  if [[ -n ${url:-} ]] && parse_http_url "$url"; then
    if code=$(curl -sS -o /dev/null -w '%{http_code}' --noproxy '*' --connect-timeout 5 --max-time 10 \
      -X POST --data-binary '' -- "$url" 2>/dev/null) && [[ $code != 000 ]]; then
      result "VictoriaMetrics (intended path)" pass "HTTP $code (empty POST, no data written)"
    else
      result "VictoriaMetrics (intended path)" fail "no route/response from this host"
    fi
  fi
fi

# --- 9. no inbound exposure ------------------------------------------------
# TCP and UDP, listening sockets only; process attribution (ss -p) needs
# root, so without root we warn instead of claiming certainty.
_no_inbound_rc=0
check_no_inbound || _no_inbound_rc=$?
case $_no_inbound_rc in
0) result "no inbound listener" pass "no listening TCP/UDP sockets owned by victron-collector" ;;
1) result "no inbound listener" fail "collector owns a listening TCP/UDP socket — see output above" ;;
2) result "no inbound listener" warn "skipped: no root / no ss / no systemctl / service has no running MainPID — run with sudo and an active service" ;;
esac

# --- summary ---------------------------------------------------------------
printf '\n=== summary: %s passed, %s failed, %s warned ===\n' \
  "$CHECKS_PASS" "$CHECKS_FAIL" "$CHECKS_WARN"
if ((CHECKS_FAIL > 0)); then
  exit 1
fi
if ((STRICT && CHECKS_WARN > 0)); then
  exit 1
fi
if ((CHECKS_WARN > 0)); then
  exit 2
fi
exit 0
