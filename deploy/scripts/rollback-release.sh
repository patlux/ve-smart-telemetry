#!/usr/bin/env bash
# rollback-release.sh — restore the previous victron-collector binary from
# the backup directory and restart the service. The newest backup is restored
# by default; --index N selects the Nth newest backup.
set -euo pipefail
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
  cat <<'EOF'
Roll back the victron-collector binary to a previous release.

Usage:
  rollback-release.sh [options]

Options:
  --index N     Restore the Nth newest snapshot (default: 1 = newest)
  --list        List available backups and exit
  --no-verify   Skip post-restart reachability verification
  --dry-run     Print every mutation instead of performing it
  -h, --help    Show this help

Backups live in /usr/local/lib/victron-collector/backups/ and are pruned to
the newest 3 by update-release.sh. Each update snapshots the collector AND
the installed victron-cli under one timestamp; this script restores the
chosen collector snapshot and, when a CLI snapshot with the same timestamp
exists, the CLI snapshot too.
EOF
}

INDEX=1 NO_VERIFY=0 LIST=0
while (($#)); do
  case $1 in
  --index)
    need_arg "$@"
    INDEX=$2
    shift 2
    ;;
  --list)
    LIST=1
    shift
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

# Positive integer only: 0 is not a valid backup index and would silently
# restore nothing.
[[ $INDEX =~ ^[1-9][0-9]*$ ]] || die "--index must be a positive integer"
# A dry run performs no mutations, so it does not need root.
((DRY_RUN)) || need_root

if ((LIST)); then
  list_backups
  exit 0
fi

rollback_target=$(list_backups | sed -n "${INDEX}p")
if [[ -z $rollback_target ]]; then
  die "no backup #$INDEX found in $VICTRON_BACKUP_DIR (see --list)"
fi

info "=== validate snapshot against effective config ==="
# Validate the selected backup at its current path BEFORE replacing any
# installed binary. A config-incompatible backup must leave the running
# release and service untouched.
check_config "$VICTRON_CONFIG" "$rollback_target"

info "=== restoring snapshot #$INDEX ==="
restore_backup "$rollback_target"
# Restore the CLI from the same snapshot timestamp when one exists. The CLI
# is a diagnostic tool; if its snapshot is absent (e.g. it was never
# installed at that point), it is left as-is rather than removed.
TS=$(basename "$rollback_target" | sed -nE "s/^${VICTRON_BINARY}\.//p")
CLI_BAK="$VICTRON_BACKUP_DIR/$VICTRON_CLI.$TS"
if [[ -f $CLI_BAK ]]; then
  restore_backup "$CLI_BAK"
else
  info "no CLI snapshot paired with snapshot #$INDEX — victron-cli left as-is"
fi

info "=== restart ==="
run systemctl restart "$VICTRON_SERVICE"
((DRY_RUN)) || sleep 5
if ((DRY_RUN)); then
  info "(dry-run) service would be restarted"
  exit 0
fi
svc_is_active || die "service not active after rollback"
info "service active"

if ((NO_VERIFY)); then
  info "(--no-verify skips deeper checks)"
else
  (check_ble_adapter) || exit 1
  (check_db "$VICTRON_DB") || exit 1
  url=$(config_vm_url "$VICTRON_CONFIG") || exit 1
  (check_vm_reachable "$url") || exit 1
fi

info "=== rollback complete ==="
info "run deploy/scripts/verify-installation.sh --strict for the full report"
