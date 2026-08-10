#!/usr/bin/env bash
# exposure-check.sh — read-only, network-only reachability probe for the
# victron-collector deployment.
#
# Runs from ANY host (no root, no Pi, no installation required): it performs
# ONLY the requested URL reachability assertions and exits. It never touches
# the local installation, never starts/stops services, never writes files,
# and sends no metrics data (the probe body is an empty POST, which the
# VictoriaMetrics import API parses as zero samples). All probes use
# curl --noproxy '*' so they measure direct reachability of the endpoint,
# not of an HTTP(S)_PROXY/ALL_PROXY proxy.
#
# Typical use:
#   intended path (the Pi with the tailnet up):
#     deploy/scripts/exposure-check.sh --reachable http://100.64.0.2:8429/...
#   unintended path (an off-tailnet host that must be blocked):
#     deploy/scripts/exposure-check.sh --unreachable http://100.64.0.2:8429/...
#
# verify-installation.sh is NOT suitable for the external probe: it verifies
# the local installation (binaries, unit, service, BLE, DB) before any
# reachability assertion. Use this script for exposure-only checks.
set -euo pipefail
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
  cat <<'EOF'
Assert URL reachability from this host (read-only; empty POST, no data written).
Probes bypass HTTP(S)_PROXY/ALL_PROXY (curl --noproxy '*') to measure direct
reachability of the endpoint.

Usage:
  exposure-check.sh (--reachable URL | --unreachable URL) [...]

Options:
  --reachable URL    Assert URL responds over HTTP (any code != 000)
  --unreachable URL  Assert URL does NOT respond (code 000: timeout/refused/no route)
  -h, --help         Show this help

At least one of --reachable/--unreachable is required; both may be repeated.
Each assertion is independent; exit code 0 = all assertions passed.

Use from the intended path (tailnet up) with --reachable and from an
unintended path (off-tailnet host, e.g. a laptop with the tailnet off) with
--unreachable to prove the VictoriaMetrics endpoint is not exposed beyond
the tailnet.

Exit codes: 0 all assertions passed; 1 any assertion failed.
EOF
}

check_url() {
  local expect=$1 url=$2 code
  # Strict URL validation before curl: malformed or option-like URLs are
  # rejected without touching the network.
  parse_http_url "$url" || {
    printf '[FAIL] invalid URL (rejected before probing): %s\n' "$url" >&2
    return 1
  }
  code=$(curl -sS -o /dev/null -w '%{http_code}' --noproxy '*' --connect-timeout 5 --max-time 10 \
    -X POST --data-binary '' -- "$url" 2>/dev/null) || true
  if [[ $expect == reachable ]]; then
    if [[ $code != 000 ]]; then
      printf '[PASS] reachable: %s (HTTP %s, empty POST, no data written)\n' "$url" "$code"
      return 0
    fi
    printf '[FAIL] not reachable (expected reachable): %s\n' "$url" >&2
    return 1
  else
    if [[ $code == 000 ]]; then
      printf '[PASS] unreachable as expected: %s (no route / timeout / refused)\n' "$url"
      return 0
    fi
    printf '[FAIL] reachable, expected unreachable: %s (HTTP %s)\n' "$url" "$code" >&2
    return 1
  fi
}

REACHABLE=()
UNREACHABLE=()
while (($#)); do
  case $1 in
  --reachable)
    need_arg "$@"
    REACHABLE+=("$2")
    shift 2
    ;;
  --unreachable)
    need_arg "$@"
    UNREACHABLE+=("$2")
    shift 2
    ;;
  -h | --help)
    usage
    exit 0
    ;;
  *) die "unknown option: $1 (see --help)" ;;
  esac
done

((${#REACHABLE[@]} + ${#UNREACHABLE[@]} > 0)) ||
  die "at least one --reachable or --unreachable URL is required"

command -v curl >/dev/null 2>&1 || die "curl not found"

rc=0
for url in "${REACHABLE[@]}"; do
  check_url reachable "$url" || rc=1
done
for url in "${UNREACHABLE[@]}"; do
  check_url unreachable "$url" || rc=1
done
exit "$rc"
