#!/usr/bin/env bash
# autoheal.sh — recover a wedged BlueZ discovery session conservatively.
#
# Recovery only runs when the recent collector journal contains repeated
# discovery contention and no successful acquisition. A persistent cooldown
# prevents restart loops. Intended to run as root from victron-autoheal.timer.
set -euo pipefail

: "${VICTRON_SERVICE:=victron-collector}"
: "${VICTRON_BLUETOOTH_SERVICE:=bluetooth.service}"
: "${VICTRON_AUTOHEAL_STATE_DIR:=/var/lib/victron-autoheal}"
: "${VICTRON_AUTOHEAL_WINDOW_MINUTES:=15}"
: "${VICTRON_AUTOHEAL_FAILURE_THRESHOLD:=3}"
: "${VICTRON_AUTOHEAL_COOLDOWN_SECONDS:=1800}"
: "${VICTRON_AUTOHEAL_VERIFY_ATTEMPTS:=12}"
: "${VICTRON_AUTOHEAL_VERIFY_INTERVAL_SECONDS:=10}"

state_file="$VICTRON_AUTOHEAL_STATE_DIR/last-recovery"

log() { printf 'victron-autoheal: %s\n' "$*"; }
die() {
  log "ERROR: $*"
  exit 1
}

[[ $EUID -eq 0 || ${VICTRON_TEST_ASSUME_ROOT:-0} == 1 ]] ||
  die "must run as root"

for setting in \
  "$VICTRON_AUTOHEAL_WINDOW_MINUTES" \
  "$VICTRON_AUTOHEAL_FAILURE_THRESHOLD" \
  "$VICTRON_AUTOHEAL_COOLDOWN_SECONDS" \
  "$VICTRON_AUTOHEAL_VERIFY_ATTEMPTS" \
  "$VICTRON_AUTOHEAL_VERIFY_INTERVAL_SECONDS"; do
  [[ $setting =~ ^[1-9][0-9]*$ ]] || die "all numeric settings must be positive integers"
done

recent_logs=$(journalctl -u "$VICTRON_SERVICE" \
  --since "-$VICTRON_AUTOHEAL_WINDOW_MINUTES minutes" --no-pager -o cat)

successes=$(grep -c 'acquisition cycle succeeded' <<<"$recent_logs" || true)
contention_failures=$(grep -cE 'acquisition cycle failed.*phase=discovering.*error_kind="contention"' <<<"$recent_logs" || true)

if ((successes > 0)); then
  log "healthy: $successes successful acquisition(s) in the last $VICTRON_AUTOHEAL_WINDOW_MINUTES minutes"
  exit 0
fi
if ((contention_failures < VICTRON_AUTOHEAL_FAILURE_THRESHOLD)); then
  log "no action: $contention_failures discovery contention failure(s), threshold $VICTRON_AUTOHEAL_FAILURE_THRESHOLD"
  exit 0
fi

now=${VICTRON_AUTOHEAL_NOW:-$(date +%s)}
if [[ -f $state_file ]]; then
  last_recovery=$(<"$state_file")
  if [[ $last_recovery =~ ^[0-9]+$ ]] && ((now - last_recovery < VICTRON_AUTOHEAL_COOLDOWN_SECONDS)); then
    remaining=$((VICTRON_AUTOHEAL_COOLDOWN_SECONDS - (now - last_recovery)))
    log "cooldown active: ${remaining}s remaining"
    exit 0
  fi
fi

mkdir -p "$VICTRON_AUTOHEAL_STATE_DIR"
chmod 0755 "$VICTRON_AUTOHEAL_STATE_DIR"
printf '%s\n' "$now" >"$state_file.tmp"
mv "$state_file.tmp" "$state_file"

log "recovery: $contention_failures discovery contention failures and no recent success; restarting BlueZ and collector"
systemctl restart "$VICTRON_BLUETOOTH_SERVICE"
systemctl restart "$VICTRON_SERVICE"

for ((attempt = 1; attempt <= VICTRON_AUTOHEAL_VERIFY_ATTEMPTS; attempt++)); do
  if systemctl is-active --quiet "$VICTRON_SERVICE"; then
    recovery_logs=$(journalctl -u "$VICTRON_SERVICE" --since "@$now" --no-pager -o cat)
    if grep -q 'acquisition cycle succeeded' <<<"$recovery_logs"; then
      log "recovery successful: acquisition resumed on verification attempt $attempt"
      exit 0
    fi
  fi
  sleep "$VICTRON_AUTOHEAL_VERIFY_INTERVAL_SECONDS"
done

die "recovery failed: no successful acquisition after $((VICTRON_AUTOHEAL_VERIFY_ATTEMPTS * VICTRON_AUTOHEAL_VERIFY_INTERVAL_SECONDS))s"
