#!/usr/bin/env bash
# install-release.sh — first install or reinstall of the victron-collector
# release assets on Raspberry Pi OS (Debian). Idempotent: safe to re-run.
#
# Performs: ARMv6 binary verification, dedicated user/group, binary + CLI
# install, config install (existing config is preserved), systemd unit
# install, daemon-reload, optional enable/start. No deployment of any kind
# happens on a non-root host, and --dry-run prints every mutation.
set -euo pipefail
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
  cat <<'EOF'
Install the victron-collector release assets on Raspberry Pi OS.

Usage:
  install-release.sh --binary PATH [options]

Options:
  --binary PATH     Compiled ARMv6 hard-float victron-collector binary (required)
  --cli PATH        Optional victron-cli binary (ARMv6, verified when given)
  --config PATH     Config to install as /etc/victron-collector/config.toml
                    (default: ../systemd/victron-collector.example.toml)
  --force-config    Overwrite an existing config (default: keep existing config)
  --no-enable       Install the unit but do not enable it
  --no-start        Install the unit but do not enable or start the service
                    (implies --no-enable)
  --dry-run         Print every mutation instead of performing it (no root needed)
  -h, --help        Show this help

First install vs reinstall:
  A FIRST install (no unit file present yet) never enables or starts the
  service automatically: the collector needs a bonded Victron device before
  it can run, so requiring an active service pre-pairing would make install
  depend on pairing. After installing, pair once, then enable and start:

      deploy/scripts/pair-device.sh
      sudo systemctl enable --now victron-collector
      sudo deploy/scripts/verify-installation.sh --strict

  A REINSTALL (unit already present) keeps the old default: enable and start
  the service and require it to stay active, unless --no-enable/--no-start.

Exit codes: 0 success; 1 failure (preflight, verification, or start).
EOF
}

BINARY="" CLI="" CONFIG=""
FORCE_CONFIG=0 NO_ENABLE=0 NO_START=0

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
  --config)
    need_arg "$@"
    CONFIG=$2
    shift 2
    ;;
  --force-config)
    FORCE_CONFIG=1
    shift
    ;;
  --no-enable)
    NO_ENABLE=1
    shift
    ;;
  --no-start)
    NO_START=1
    NO_ENABLE=1
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
CONFIG=${CONFIG:-$SCRIPT_DIR/../systemd/victron-collector.example.toml}
UNIT_SRC=$SCRIPT_DIR/../systemd/$VICTRON_UNIT
[[ -f $UNIT_SRC ]] || die "unit template not found: $UNIT_SRC"
[[ -f $CONFIG ]] || die "config template not found: $CONFIG"

# A dry run performs no mutations, so it does not need root; this also makes
# the install flow testable in a mocked, non-root harness.
((DRY_RUN)) || need_root

info "=== preflight: ARMv6 binary identity ==="
verify_armv6_binary "$BINARY"
smoke_run "$BINARY" --version
if [[ -n $CLI ]]; then
  verify_armv6_binary "$CLI"
  smoke_run "$CLI" --version
fi

info "=== preflight: effective configuration ==="
# Resolve and validate the configuration BEFORE the first mutation. This
# ordering is intentional: a rejected candidate/existing config must not
# create accounts, directories, binaries, unit files, or touch systemd.
EFFECTIVE_CONFIG=$CONFIG
if [[ -f $VICTRON_CONFIG && $FORCE_CONFIG -eq 0 ]]; then
  EFFECTIVE_CONFIG=$VICTRON_CONFIG
  if cmp -s "$CONFIG" "$VICTRON_CONFIG"; then
    info "config already in place: $VICTRON_CONFIG"
  else
    warn "existing config kept: $VICTRON_CONFIG (use --force-config to overwrite)"
    diff -u "$VICTRON_CONFIG" "$CONFIG" | head -40 || true
  fi
fi
check_config "$EFFECTIVE_CONFIG" "$BINARY"

info "=== accounts ==="
ensure_accounts

info "=== directories ==="
run mkdir -p "$VICTRON_BIN_DIR" "$VICTRON_ETC_DIR" "$VICTRON_STATE_DIR" "$VICTRON_BACKUP_DIR"
run chown "$VICTRON_USER:$VICTRON_GROUP" "$VICTRON_STATE_DIR"
run chmod 0750 "$VICTRON_STATE_DIR"

info "=== binaries ==="
run install -o root -g root -m 0755 "$BINARY" "$VICTRON_BIN_DIR/$VICTRON_BINARY"
if [[ -n $CLI ]]; then
  run install -o root -g root -m 0755 "$CLI" "$VICTRON_BIN_DIR/$VICTRON_CLI"
fi

info "=== configuration ==="
if [[ ! -f $VICTRON_CONFIG || $FORCE_CONFIG -eq 1 ]]; then
  run install -o root -g root -m 0644 "$CONFIG" "$VICTRON_CONFIG"
  info "installed config: $VICTRON_CONFIG"
fi

info "=== systemd unit ==="
# Detect a first install BEFORE installing the unit file: whether the unit
# existed before this run decides if we enable/start (reinstall) or leave
# the service disabled/stopped for pairing (first install).
FIRST_INSTALL=0
[[ -f $VICTRON_UNIT_DIR/$VICTRON_UNIT ]] || FIRST_INSTALL=1
run install -o root -g root -m 0644 "$UNIT_SRC" "$VICTRON_UNIT_DIR/$VICTRON_UNIT"
run systemctl daemon-reload

if ((FIRST_INSTALL)); then
  # A first install must not require a healthy (paired) collector: the
  # service cannot do useful work before pairing, so we never enable/start
  # it here and never require it to stay active. Documented flow:
  # install --no-start -> pair-device.sh -> systemctl enable --now -> verify.
  enable_start_service 1
else
  # Reinstall: enable and start unless --no-enable/--no-start. --no-start
  # implies no enable ("do not enable or start").
  enable_start_service 0 || exit 1
fi

info "=== install complete ==="
if ((FIRST_INSTALL)); then
  info "Next: pair the Victron device once (deploy/scripts/pair-device.sh), then"
  info "enable/start with: sudo systemctl enable --now $VICTRON_SERVICE"
  info "and confirm with: sudo deploy/scripts/verify-installation.sh --strict"
else
  info "Next: run deploy/scripts/verify-installation.sh to confirm everything."
fi
