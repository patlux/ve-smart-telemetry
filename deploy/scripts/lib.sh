#!/usr/bin/env bash
# lib.sh — shared helpers for the victron-collector deployment scripts.
#
# Sourced by install-release.sh, update-release.sh, rollback-release.sh,
# verify-installation.sh and pair-device.sh. Do not execute directly.
#
# All scripts are Raspberry Pi OS (Debian) targets and run as root for any
# mutation. Paths can be overridden via VICTRON_* environment variables,
# which makes dry-run testing in a staging root possible.
set -euo pipefail

# ---- paths (env-overridable) -------------------------------------------
: "${VICTRON_SERVICE:=victron-collector}"
: "${VICTRON_USER:=victron-collector}"
: "${VICTRON_GROUP:=victron-collector}"
: "${VICTRON_BINARY:=victron-collector}"
: "${VICTRON_CLI:=victron-cli}"
: "${VICTRON_BIN_DIR:=/usr/local/bin}"
: "${VICTRON_ETC_DIR:=/etc/victron-collector}"
: "${VICTRON_STATE_DIR:=/var/lib/victron-collector}"
: "${VICTRON_BACKUP_DIR:=/usr/local/lib/victron-collector/backups}"
: "${VICTRON_UNIT_DIR:=/etc/systemd/system}"
: "${VICTRON_UNIT:=victron-collector.service}"
VICTRON_CONFIG="${VICTRON_CONFIG:-$VICTRON_ETC_DIR/config.toml}"
VICTRON_DB="${VICTRON_DB:-$VICTRON_STATE_DIR/state.sqlite3}"

# ---- runtime flags ------------------------------------------------------
DRY_RUN=0

# ---- logging ------------------------------------------------------------
info() { printf '%s\n' "[info]  $*"; }
warn() { printf '%s\n' "[warn]  $*" >&2; }
error() { printf '%s\n' "[error] $*" >&2; }
die() {
  error "$*"
  exit 1
}

# run: execute a command, or print it under --dry-run.
run() {
  if ((DRY_RUN)); then
    printf '[dry-run]'
    printf ' %q' "$@"
    printf '\n'
    return 0
  fi
  "$@"
}

need_root() {
  [[ ${VICTRON_TEST_ASSUME_ROOT:-0} == 1 || $EUID -eq 0 ]] ||
    die "this script must run as root (use sudo)"
}

# need_arg: called as the first statement of an option branch that consumes
# a value. Verifies the remaining arguments contain a non-empty, non-option
# value. Usage:  --binary) need_arg "$@"; BINARY=$2; shift 2 ;;
need_arg() {
  local opt=${1:-}
  if (($# < 2)); then
    die "$opt requires an argument"
  fi
  if [[ $2 == --* ]]; then
    die "$opt requires an argument (got '$2')"
  fi
}

# ---- ARMv6 binary identity / linkage -----------------------------------
# The Pi Zero W is ARMv6 (ARM1176JZF-S, ARMv6KZ-class), hard-float. The
# ARM1176JZF-S does NOT implement Thumb-2, so an ARMv6T2 binary must be
# rejected even though it is ARMv6-class. readelf -A prints the Tag_CPU_arch
# names from the ARM ELF ABI table; the defensible tags for the
# arm-unknown-linux-gnueabihf target are:
#   v6   — what the default build emits (rustc emits Tag_CPU_arch "v6")
#   v6KZ — the exact ARM1176JZF-S architecture (emitted with
#          -C target-cpu=arm1176jzf-s)
#   v6K  — ARMv6K, a subset of ARMv6KZ; runs on the Pi Zero W
# Anything else — including "v6T2" (Thumb-2, not supported by ARM1176JZF-S)
# and "v7"+ — is rejected.
is_armv6_arch_tag() {
  case "$1" in
  v6 | v6KZ | v6K) return 0 ;;
  *) return 1 ;;
  esac
}

# is_hard_float_vfp_tag: Tag_ABI_VFP_args must be "VFP" (hard-float ABI).
is_hard_float_vfp_tag() {
  [[ $1 == VFP ]]
}

is_thumb1_tag() {
  [[ $1 == Thumb-1 ]]
}

is_vfpv2_tag() {
  [[ $1 == VFPv2 ]]
}

verify_armv6_binary() {
  local bin=$1 what arch thumb fp vfp interpreter missing
  [[ -f $bin ]] || die "binary not found: $bin"
  [[ -r $bin ]] || die "binary not readable: $bin"

  command -v file >/dev/null 2>&1 || die "file(1) not found (install binutils/file)"
  command -v readelf >/dev/null 2>&1 || die "readelf not found (install binutils)"
  command -v ldd >/dev/null 2>&1 || die "ldd not found (install libc-bin)"

  what=$(file -b "$bin" 2>/dev/null || true)
  case "$what" in
  *ELF*32-bit*ARM*) ;;
  *) die "not a 32-bit ARM ELF binary: $bin -> $what" ;;
  esac

  arch=$(readelf -A "$bin" 2>/dev/null | awk '/Tag_CPU_arch:/{print $2; exit}')
  if ! is_armv6_arch_tag "$arch"; then
    die "unsupported CPU arch tag '${arch:-missing}' (need ARMv6 for Pi Zero W; v6T2/Thumb-2 and v7+ are rejected)"
  fi

  thumb=$(readelf -A "$bin" 2>/dev/null | awk '/Tag_THUMB_ISA_use:/{print $2; exit}')
  is_thumb1_tag "$thumb" ||
    die "unsupported Thumb ISA '${thumb:-missing}' (Pi Zero W requires merged Thumb-1; Thumb-2 is rejected)"

  fp=$(readelf -A "$bin" 2>/dev/null | awk '/Tag_FP_arch:/{print $2; exit}')
  is_vfpv2_tag "$fp" ||
    die "unsupported FP architecture '${fp:-missing}' (Pi Zero W release requires merged VFPv2)"

  vfp=$(readelf -A "$bin" 2>/dev/null | awk '/Tag_ABI_VFP_args:/{print $2; exit}')
  if ! is_hard_float_vfp_tag "$vfp"; then
    die "not hard-float (Tag_ABI_VFP_args='${vfp:-missing}', expected 'VFP')"
  fi

  interpreter=$(readelf -l "$bin" 2>/dev/null | sed -nE 's/.*interpreter:[[:space:]]*([^]]+)\].*/\1/p' | head -1)
  [[ $interpreter == /lib/ld-linux-armhf.so.3 ]] ||
    die "unsupported ELF interpreter '${interpreter:-missing}' (expected /lib/ld-linux-armhf.so.3)"

  missing=$(ldd "$bin" 2>/dev/null | awk '/not found/{print $1; exit}') || true
  if [[ -n $missing ]]; then
    die "unresolved shared library: $missing"
  fi
  info "ARMv6 hard-float identity + linkage OK: $bin"
}

# Run a binary briefly and require a clean exit (e.g. --version smoke test).
smoke_run() {
  local bin=$1 rc=0 out
  shift
  if command -v timeout >/dev/null 2>&1; then
    out=$(timeout 15 "$bin" "$@" 2>&1) || rc=$?
  else
    out=$("$bin" "$@" 2>&1) || rc=$?
  fi
  if ((rc != 0)); then
    die "'$bin $*' exited $rc: ${out:-no output}"
  fi
  info "'$bin $*' -> ${out:-no output}"
}

# ---- accounts -----------------------------------------------------------
ensure_accounts() {
  if ! id "$VICTRON_USER" >/dev/null 2>&1; then
    run useradd --system --no-create-home --home-dir /nonexistent \
      --shell /usr/sbin/nologin "$VICTRON_USER"
    info "created system user $VICTRON_USER"
  else
    info "user $VICTRON_USER exists"
  fi
  if ! getent group "$VICTRON_GROUP" >/dev/null 2>&1; then
    run groupadd --system "$VICTRON_GROUP"
    run usermod -aG "$VICTRON_GROUP" "$VICTRON_USER"
    info "created system group $VICTRON_GROUP"
  fi
}

# ---- config / URL helpers -----------------------------------------------
# shellcheck source=config-lib.sh
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/config-lib.sh"

# ipv4_to_int: convert a strict dotted-quad IPv4 to a 32-bit integer.
# Rejects leading zeros (octal ambiguity), octets > 255, and any
# non-numeric component. Prints the integer on success, returns 1 on
# rejection.
ipv4_to_int() {
  local ip=$1 a b c d
  IFS=. read -r a b c d <<<"$ip"
  [[ $a =~ ^(0|[1-9][0-9]*)$ && $b =~ ^(0|[1-9][0-9]*)$ && $c =~ ^(0|[1-9][0-9]*)$ && $d =~ ^(0|[1-9][0-9]*)$ ]] || return 1
  ((a <= 255 && b <= 255 && c <= 255 && d <= 255)) || return 1
  printf '%d' $((a * 16777216 + b * 65536 + c * 256 + d))
}

# ipv4_in_cgnat: is the IPv4 inside Tailscale CGNAT 100.64.0.0/10?
# Numeric inclusion: (ip & 0xFFC00000) == 0x64400000. Returns 1 for any
# non-IPv4 host (DNS names cannot be classified numerically).
ipv4_in_cgnat() {
  local ip=$1 n
  n=$(ipv4_to_int "$ip") || return 1
  (( (n & 0xFFC00000) == 0x64400000 ))
}

# ---- VictoriaMetrics reachability ---------------------------------------
# Empty POST body: the import API parses zero samples, so this performs no
# data write. Any HTTP response (even 4xx) proves the route; code 000 means
# unreachable (timeout / refused / no route). --noproxy '*' forces a direct
# connection so the probe measures reachability of the endpoint itself, not
# of an HTTP(S)_PROXY/ALL_PROXY proxy. The URL is strictly validated before
# curl so malformed or option-like URLs are rejected; '--' before the URL is
# defense in depth against option injection.
check_vm_reachable() {
  local url=$1 expect=${2:-reachable} code=000
  parse_http_url "$url" ||
    die "invalid VictoriaMetrics URL: $url (must be plain http://host:port/absolute/path, no userinfo/query/fragment, valid port)"
  code=$(curl -sS -o /dev/null -w '%{http_code}' --noproxy '*' --connect-timeout 5 --max-time 10 \
    -X POST --data-binary '' -- "$url" 2>/dev/null) || true
  if [[ $expect == reachable ]]; then
    [[ $code != 000 ]] ||
      die "VictoriaMetrics unreachable from this host: $url (empty POST, no data written)"
    info "VictoriaMetrics reachable (HTTP $code, empty POST — no data written)"
  else
    [[ $code == 000 ]] ||
      die "expected unreachable, but got HTTP $code: $url"
    info "VictoriaMetrics not reachable as expected (route blocked): $url"
  fi
}

# ---- BLE adapter --------------------------------------------------------
check_ble_adapter() {
  local adapter=${1:-hci0} out
  command -v bluetoothctl >/dev/null 2>&1 || die "bluetoothctl not found"
  out=$(bluetoothctl --timeout 10 list 2>/dev/null) || true
  grep -q '^Controller' <<<"$out" ||
    die "no Bluetooth controller visible (bluetoothctl list)"
  if [[ $adapter != hci0 ]]; then
    grep -q "Controller.* $adapter" <<<"$out" ||
      warn "adapter '$adapter' not listed by bluetoothctl"
  fi
  if command -v rfkill >/dev/null 2>&1; then
    rfkill list bluetooth 2>/dev/null | grep -q 'soft blocked' &&
      warn "Bluetooth is soft-blocked (rfkill) — unblock with: rfkill unblock bluetooth"
  fi
  info "BLE adapter '$adapter' visible"
}

# ---- service / database / exposure -------------------------------------
svc_is_active() { systemctl is-active --quiet "$VICTRON_SERVICE" 2>/dev/null; }
svc_is_enabled() { systemctl is-enabled --quiet "$VICTRON_SERVICE" 2>/dev/null; }

# enable_start_service: enable and/or start the unit after a reinstall.
# --no-start means "do not enable or start": it implies no enable, so the
# unit is never enabled when --no-start is set. A first install never
# enables or starts (the collector needs a bonded Victron device before it
# can run; see install-release.sh --help). Pure-ish helper: with DRY_RUN=1
# it only prints the systemctl commands, which makes it testable without
# root or systemd.
enable_start_service() {
  local first_install=$1
  if ((first_install)); then
    info "first install detected — unit installed but NOT enabled/started"
    info "pair the Victron device once, then enable and start:"
    info "  deploy/scripts/pair-device.sh"
    info "  sudo systemctl enable --now $VICTRON_SERVICE"
    info "  sudo deploy/scripts/verify-installation.sh --strict"
    return 0
  fi
  if ((NO_ENABLE == 0 && NO_START == 0)); then
    run systemctl enable "$VICTRON_SERVICE"
  fi
  if ((NO_START == 0)); then
    run systemctl restart "$VICTRON_SERVICE"
    ((DRY_RUN)) || sleep 3
    if ((!DRY_RUN)) && svc_is_active; then
      info "service active"
    elif ((DRY_RUN)); then
      info "(dry-run) service would be restarted"
    else
      error "service not active after start — recent journal:"
      journalctl -u "$VICTRON_SERVICE" -n 20 --no-pager 2>/dev/null | sed 's/^/  /' || true
      return 1
    fi
  fi
  return 0
}

check_db() {
  local db=$1 owner
  if [[ ! -f $db ]]; then
    warn "database not created yet: $db (appears after the first successful run)"
    return 0
  fi
  owner=$(stat -c '%U:%G' "$db" 2>/dev/null || echo unknown)
  [[ $owner == "$VICTRON_USER:$VICTRON_GROUP" ]] ||
    warn "database owner is $owner, expected $VICTRON_USER:$VICTRON_GROUP"
  if command -v sqlite3 >/dev/null 2>&1; then
    sqlite3 "$db" 'PRAGMA integrity_check;' 2>/dev/null | grep -q '^ok$' ||
      warn "sqlite3 integrity_check failed for $db"
  fi
  info "database present: $db ($owner)"
}

# ss_has_pid: does ss -lntup output attribute a listening socket to the given
# pid? Matches the `users:(("comm",pid=<PID>,fd=N))` format; the pid boundary
# is enforced so pid=1234 does not match pid=12345. Pure helper (testable
# without root).
ss_has_pid() {
  local out=$1 pid=$2
  grep -qE "pid=${pid}([,)]|$)" <<<"$out"
}

# check_no_inbound: assert that the collector owns no listening TCP or UDP
# socket (ss -lntup). Attribution is by PID, not by process name: Linux task
# comm is truncated to 15 chars (TASK_COMM_LEN), so "victron-collector"
# (17 chars) appears as "victron-collecto" in ss -p output and a name grep
# can miss it. We read the service MainPID from `systemctl show` and match
# pid=<PID> in the root ss -lntup output. Process attribution via ss -p is
# only possible as root; without root we cannot observe other users'
# processes and must not claim certainty, so we say so and skip. Exit:
# 0 clean, 1 listener found, 2 skipped (no root / no ss / no systemctl /
# service has no running MainPID).
check_no_inbound() {
  local pid out found
  if [[ $EUID -ne 0 ]]; then
    warn "listener check skipped: ss -p cannot attribute sockets to processes without root"
    return 2
  fi
  command -v ss >/dev/null 2>&1 || {
    warn "ss not found (install iproute2) — listener check skipped"
    return 2
  }
  command -v systemctl >/dev/null 2>&1 || {
    warn "systemctl not found — listener check skipped"
    return 2
  }
  pid=$(systemctl show -p MainPID --value "$VICTRON_SERVICE" 2>/dev/null || true)
  if [[ -z $pid || $pid == 0 ]]; then
    warn "service $VICTRON_SERVICE has no running MainPID (inactive or not started) — no listener expected, nothing to attribute"
    return 2
  fi
  out=$(ss -lntup 2>/dev/null || true)
  if ss_has_pid "$out" "$pid"; then
    found=$(grep -E "pid=${pid}([,)]|$)" <<<"$out" || true)
    error "victron-collector (pid $pid) owns a listening TCP or UDP socket (unexpected inbound exposure):"
    printf '%s\n' "$found" >&2
    return 1
  fi
  info "no listening TCP or UDP sockets owned by victron-collector (pid $pid, root check)"
  return 0
}

# ---- backups ------------------------------------------------------------
# update-release.sh snapshots the installed collector AND the installed CLI
# with the same timestamp, so the Nth newest collector backup is paired with
# the Nth newest CLI backup. rollback-release.sh restores both.
# Snapshot filenames end in a fixed-width YYYYmmddHHMMSS timestamp. Sort by
# that timestamp, not file mtime: `cp -p` deliberately preserves the source
# mtime, so `ls -t` can otherwise delete the snapshot that was just created.
list_named_backups() {
  local prefix=$1
  find "$VICTRON_BACKUP_DIR" -maxdepth 1 -type f -name "$prefix.*" -print 2>/dev/null \
    | LC_ALL=C sort -r
}
list_backups() { list_named_backups "$VICTRON_BINARY"; }
list_cli_backups() { list_named_backups "$VICTRON_CLI"; }
newest_backup() { list_backups | head -1; }

restore_backup() {
  local b=$1 dest
  [[ -f $b ]] || die "backup not found: $b"
  verify_armv6_binary "$b"
  case "$(basename "$b")" in
  "$VICTRON_CLI".*) dest=$VICTRON_BIN_DIR/$VICTRON_CLI ;;
  "$VICTRON_BINARY".*) dest=$VICTRON_BIN_DIR/$VICTRON_BINARY ;;
  *) die "unexpected backup name: $b" ;;
  esac
  run install -o root -g root -m 0755 "$b" "$dest"
  info "restored $(basename "$dest") from $b"
}

prune_backup_list() {
  local f=$1 keep=$2 count oldest
  count=$("$f" | wc -l | tr -d ' ')
  while ((count > keep)); do
    oldest=$("$f" | tail -1)
    run rm -f "$oldest"
    info "pruned old backup: $oldest"
    count=$((count - 1))
  done
}

prune_backups() {
  local keep=${1:-3}
  prune_backup_list list_backups "$keep"
  prune_backup_list list_cli_backups "$keep"
}

# ---- result accounting (verify script) ---------------------------------
declare -i CHECKS_PASS=0 CHECKS_FAIL=0 CHECKS_WARN=0
result() {
  local name=$1 rc=$2 detail=${3:-}
  case $rc in
  pass)
    CHECKS_PASS=$((CHECKS_PASS + 1))
    printf '[PASS] %s\n' "$name"
    ;;
  warn)
    CHECKS_WARN=$((CHECKS_WARN + 1))
    printf '[WARN] %s — %s\n' "$name" "$detail"
    ;;
  fail)
    CHECKS_FAIL=$((CHECKS_FAIL + 1))
    printf '[FAIL] %s — %s\n' "$name" "$detail"
    ;;
  esac
}
