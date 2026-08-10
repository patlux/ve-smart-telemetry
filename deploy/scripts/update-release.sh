#!/usr/bin/env bash
# update-release.sh — swap in a new victron-collector release with automatic
# rollback if the service fails to come up. Idempotent: a no-op only when
# every provided binary (collector and/or CLI) is already installed and
# identical to the new release.
set -euo pipefail
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
  cat <<'EOF'
Update the installed victron-collector binary to a new release.

Usage:
  update-release.sh --binary PATH [options]

Options:
  --binary PATH  New ARMv6 hard-float victron-collector binary (required)
  --cli PATH     Optional new victron-cli binary (verified when given)
  --no-verify    Skip post-start reachability verification (default: verify)
  --dry-run      Print every mutation instead of performing it
  -h, --help     Show this help

Behaviour:
  - verifies ARMv6 identity/linkage and --version of the new binary
  - snapshots the current collector AND the current victron-cli (if
    installed) to /usr/local/lib/victron-collector/backups under one
    timestamp; keeps the newest 3 of each
  - installs the new binaries, restarts, and verifies: service active,
    BLE adapter visible, database present, VictoriaMetrics reachable
  - on verification failure: restores the newest snapshot and restarts

A CLI-only change (collector identical) is still applied: the "nothing to
do" early exit only triggers when BOTH provided binaries are already in
place and identical.

Exit codes: 0 success; 1 failure (preflight, verification, or rollback).
EOF
}

BINARY="" CLI="" NO_VERIFY=0
while (($#)); do
  case $1 in
  --binary)
    need_arg "$@"
    BINARY=$2
    shift 2
    ;;
  --cli)
    need_arg "$@"
    CLI=$2
    shift 2
    ;;
  --no-verify)
    NO_VERIFY=1
    shift
    ;;
  --dry-run)
    DRY_RUN=1
    shift
    ;;
  -h | --help)
    usage
    exit 0
    ;;
  *) die "unknown option: $1 (see --help)" ;;
  esac
done

[[ -n $BINARY ]] || die "--binary PATH is required"
[[ -f $BINARY ]] || die "binary not found: $BINARY"
# A dry run performs no mutations, so it does not need root.
((DRY_RUN)) || need_root

info "=== preflight: new binary identity ==="
verify_armv6_binary "$BINARY"
smoke_run "$BINARY" --version
if [[ -n $CLI ]]; then
  verify_armv6_binary "$CLI"
  smoke_run "$CLI" --version
fi

CURRENT="$VICTRON_BIN_DIR/$VICTRON_BINARY"
CURRENT_CLI="$VICTRON_BIN_DIR/$VICTRON_CLI"

# Nothing-to-do check must consider BOTH binaries: a CLI-only update is a
# real change even when the collector is already at the target release.
same_collector=0
same_cli=0
if [[ -f $CURRENT ]] && cmp -s "$CURRENT" "$BINARY"; then same_collector=1; fi
if [[ -n $CLI && -f $CURRENT_CLI ]] && cmp -s "$CURRENT_CLI" "$CLI"; then same_cli=1; fi
if ((same_collector)) && { [[ -z $CLI ]] || ((same_cli)); }; then
  info "installed binaries already match the new release — nothing to do"
  exit 0
fi

info "=== validate existing config against new binary ==="
# The new binary must accept the existing config BEFORE it is installed or
# the service is restarted. If it rejects the config, fail now while the
# old binary is still in place and the service is untouched — no rollback
# is needed because nothing was changed yet.
check_config "$VICTRON_CONFIG" "$BINARY"

info "=== snapshot current binaries ==="
run mkdir -p "$VICTRON_BACKUP_DIR"
TS=$(date +%Y%m%d%H%M%S)
# Always snapshot BOTH installed binaries under one timestamp so that the Nth
# newest collector backup is paired with the Nth newest CLI backup and
# rollback-release.sh can restore a consistent snapshot.
if [[ -f $CURRENT ]]; then
  run cp -p "$CURRENT" "$VICTRON_BACKUP_DIR/$VICTRON_BINARY.$TS"
  info "snapshot: $VICTRON_BACKUP_DIR/$VICTRON_BINARY.$TS"
fi
if [[ -f $CURRENT_CLI ]]; then
  run cp -p "$CURRENT_CLI" "$VICTRON_BACKUP_DIR/$VICTRON_CLI.$TS"
  info "snapshot: $VICTRON_BACKUP_DIR/$VICTRON_CLI.$TS"
fi
prune_backups 3

info "=== install new binaries ==="
if ((!same_collector)); then
  run install -o root -g root -m 0755 "$BINARY" "$CURRENT"
fi
if [[ -n $CLI ]] && ((!same_cli)); then
  run install -o root -g root -m 0755 "$CLI" "$CURRENT_CLI"
fi

restart_and_verify() {
  run systemctl restart "$VICTRON_SERVICE"
  ((DRY_RUN)) || sleep 5
  ((DRY_RUN)) && return 0
  svc_is_active || {
    error "service not active"
    return 1
  }
  if ((NO_VERIFY)); then
    info "service active (--no-verify skips deeper checks)"
    return 0
  fi
  # Subshells: a die() inside a check must fail only the check, so the
  # rollback path below can still run.
  (check_ble_adapter) || return 1
  (check_db "$VICTRON_DB") || return 1
  local url
  url=$(config_vm_url "$VICTRON_CONFIG") || return 1
  (check_vm_reachable "$url") || return 1
}

if restart_and_verify; then
  info "=== update complete ==="
  info "run deploy/scripts/verify-installation.sh --strict for the full report"
else
  error "=== update failed — rolling back ==="
  rollback_target=$(newest_backup)
  if [[ -n $rollback_target ]]; then
    # A backup that cannot parse the effective config must never replace the
    # installed candidate. Validate it in-place before the first restore.
    if ! (check_config "$VICTRON_CONFIG" "$rollback_target"); then
      error "rollback backup rejects the effective config — new binary left in place"
      exit 1
    fi
    restore_backup "$rollback_target"
    # Restore the paired CLI snapshot too, mirroring rollback-release.sh.
    TS=$(basename "$rollback_target" | sed -nE "s/^${VICTRON_BINARY}\.//p")
    CLI_BAK="$VICTRON_BACKUP_DIR/$VICTRON_CLI.$TS"
    if [[ -f $CLI_BAK ]]; then
      restore_backup "$CLI_BAK"
    fi
    run systemctl restart "$VICTRON_SERVICE"
    ((DRY_RUN)) || sleep 3
    if ((!DRY_RUN)) && svc_is_active; then
      info "rollback to $rollback_target complete; service active"
    else
      error "rollback started but service not active — manual intervention required"
    fi
  else
    error "no backup available — new binary left in place"
  fi
  exit 1
fi
