#!/usr/bin/env bash
# Collect one private, bounded raw history capture while preserving the collector.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=config-lib.sh
source "$SCRIPT_DIR/config-lib.sh"

CONFIG=${CONFIG:-/etc/victron-collector/config.toml}
COLLECTOR_SERVICE=${COLLECTOR_SERVICE:-victron-collector.service}
CLI=${CLI:-/usr/local/bin/victron-cli}
EVIDENCE_DIR=${EVIDENCE_DIR:-/var/lib/victron-history-evidence}
LOCK_FILE=${LOCK_FILE:-/run/lock/victron-history-evidence.lock}
TIMEOUT_SECONDS=${TIMEOUT_SECONDS:-180}
READ_ATTEMPTS=${READ_ATTEMPTS:-2}
RETRY_DELAY_SECONDS=${RETRY_DELAY_SECONDS:-10}

log() { printf 'victron-history-capture: %s\n' "$*" >&2; }
fail() { log "$*"; exit 1; }

[ "$(id -u)" -eq 0 ] || fail "must run as root"
[ -x "$CLI" ] || fail "diagnostic CLI is unavailable"
[ -r "$CONFIG" ] || fail "collector configuration is unavailable"
case "$READ_ATTEMPTS" in
  1|2) ;;
  *) fail "read attempts must be 1 or 2" ;;
esac
case "$EVIDENCE_DIR" in
  /var/lib/victron-history-evidence|/var/lib/victron-history-evidence/*) ;;
  *) fail "evidence directory must remain under /var/lib/victron-history-evidence" ;;
esac

exec 9>"$LOCK_FILE"
flock -n 9 || { log "another capture is active"; exit 0; }

# config-lib uses die() for fail-closed parser failures.
die() { fail "$*"; }
alias=$(config_bluez_alias "$CONFIG")
adapter=$(config_ble_adapter "$CONFIG")
instance=$(config_instance "$CONFIG")
[ "$instance" -gt 0 ] || fail "instance must be positive"

install -d -m 0700 -o root -g root "$EVIDENCE_DIR"
capture_id=$(date -u +%Y%m%dT%H%M%SZ)
tmp_dir=$(mktemp -d "$EVIDENCE_DIR/.capture.XXXXXX")
out="$tmp_dir/history.json"
metadata="$tmp_dir/metadata.json"
log_file="$tmp_dir/diagnostic.log"
was_active=false

cleanup() {
  status=$?
  if [ "$was_active" = true ] && ! systemctl is-active --quiet "$COLLECTOR_SERVICE"; then
    systemctl start "$COLLECTOR_SERVICE" || true
  fi
  if [ "$status" -ne 0 ]; then
    rm -rf "$tmp_dir"
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM HUP

if systemctl is-active --quiet "$COLLECTOR_SERVICE"; then
  was_active=true
  systemctl stop "$COLLECTOR_SERVICE"
  for _ in $(seq 1 30); do
    systemctl is-active --quiet "$COLLECTOR_SERVICE" || break
    sleep 1
  done
  systemctl is-active --quiet "$COLLECTOR_SERVICE" && fail "collector did not stop"
fi

cli_status=1
for attempt in $(seq 1 "$READ_ATTEMPTS"); do
  rm -f "$out" "$log_file"
  set +e
  timeout --signal=TERM --kill-after=15 "$TIMEOUT_SECONDS" \
    env RUST_LOG='warn,victron_bluez=debug,victron_client=debug,victron_cli=debug' \
    "$CLI" read-history \
      --device "$alias" \
      --adapter "$adapter" \
      --instance "$instance" \
      --days 30 \
      --connect-timeout-seconds 120 \
      --response-timeout-seconds 15 \
      --batch-size 8 \
      --raw \
      --out "$out" \
      >/dev/null 2>"$log_file"
  cli_status=$?
  set -e
  [ "$cli_status" -eq 0 ] && break
  log "bounded history read attempt $attempt failed"
  if [ "$attempt" -lt "$READ_ATTEMPTS" ]; then
    sleep "$RETRY_DELAY_SECONDS"
  fi
done

recovery_epoch=$(date +%s)
if [ "$was_active" = true ]; then
  systemctl start "$COLLECTOR_SERVICE"
fi
[ "$cli_status" -eq 0 ] || fail "bounded history read failed"
[ -s "$out" ] || fail "history read produced no capture"

# Raw diagnostic traces are transient; only structured, root-only evidence persists.
rm -f "$log_file"
cat >"$metadata" <<EOF
{
  "captureId": "$capture_id",
  "timezone": "Europe/Berlin",
  "capturedLocalDate": "$(TZ=Europe/Berlin date +%F)",
  "referenceStatus": "device-raw-only",
  "victronConnectReferenceAvailable": false
}
EOF
(
  cd "$tmp_dir"
  sha256sum history.json metadata.json > SHA256SUMS
)
chmod 0600 "$tmp_dir/history.json" "$tmp_dir/metadata.json" "$tmp_dir/SHA256SUMS"

if [ "$was_active" = true ]; then
  for _ in $(seq 1 30); do
    systemctl is-active --quiet "$COLLECTOR_SERVICE" && break
    sleep 1
  done
  systemctl is-active --quiet "$COLLECTOR_SERVICE" || fail "collector did not recover"
  success=false
  for _ in $(seq 1 20); do
    if journalctl -u "$COLLECTOR_SERVICE" --since "@${recovery_epoch}" --no-pager -o cat \
      | grep -q 'acquisition cycle succeeded.*delivery=Delivered'; then
      success=true
      break
    fi
    sleep 15
  done
  [ "$success" = true ] || fail "collector did not deliver a recovery cycle"
  main_pid=$(systemctl show -p MainPID --value "$COLLECTOR_SERVICE")
  case "$main_pid" in
    ''|0|*[!0-9]*) fail "collector has no main PID after recovery" ;;
  esac
  if ss -lntup 2>/dev/null | grep -Eq "pid=${main_pid}([,)]|$)"; then
    fail "collector opened a listener after recovery"
  fi
fi

final_dir="$EVIDENCE_DIR/$capture_id"
[ ! -e "$final_dir" ] || fail "capture identifier already exists"
mv "$tmp_dir" "$final_dir"
mapfile -t captures < <(find "$EVIDENCE_DIR" -mindepth 2 -maxdepth 2 -type f -name history.json -print | sort)
if ((${#captures[@]} >= 2)); then
  comparison_tmp="$EVIDENCE_DIR/.comparison.json.tmp"
  "$CLI" analyze-history "${captures[@]}" --out "$comparison_tmp" >/dev/null
  chmod 0600 "$comparison_tmp"
  mv "$comparison_tmp" "$EVIDENCE_DIR/comparison.json"
fi
log "capture complete: $capture_id"
