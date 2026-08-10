#!/usr/bin/env bash
# pair-device.sh — one-time local pairing of the Victron device via
# bluetoothctl. The PIN is entered INSIDE bluetoothctl's own interactive
# prompt, never into the shell, so it never appears in shell history,
# process listings, or this script. No PIN literal exists anywhere here.
#
# This script performs no pairing by itself: it preflights the Bluetooth
# stack and then hands over to an interactive bluetoothctl session.
set -euo pipefail
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
  cat <<'EOF'
Pair the Victron VE.Smart device once, locally, via bluetoothctl.

Usage:
  pair-device.sh [options]

Options:
  --dry-run    Print the preflight and the interactive instructions, do not launch bluetoothctl
  -h, --help   Show this help

The PIN is typed into bluetoothctl's own prompt. bluetoothctl is launched
interactively, so the PIN never enters the shell and is never stored.
EOF
}

DRY_RUN=0
while (($#)); do
  case $1 in
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

# --- preflight (read-only) -------------------------------------------------
info "=== preflight ==="
command -v bluetoothctl >/dev/null 2>&1 || die "bluetoothctl not found"
if command -v systemctl >/dev/null 2>&1; then
  if systemctl is-active --quiet bluetooth; then
    info "bluetooth.service active"
  else
    warn "bluetooth.service not active — start it: sudo systemctl start bluetooth"
    if ((DRY_RUN)); then
      info "(dry-run) stopping before launching bluetoothctl"
      exit 1
    fi
    exit 1
  fi
fi
out=$(bluetoothctl --timeout 10 list 2>/dev/null) || true
if grep -q '^Controller' <<<"$out"; then
  info "adapter present:"
  sed -n 's/^Controller /  Controller /p' <<<"$out"
else
  die "no Bluetooth controller visible — check the adapter (rfkill list bluetooth)"
fi
if command -v rfkill >/dev/null 2>&1; then
  if rfkill list bluetooth 2>/dev/null | grep -q 'soft blocked'; then
    die "Bluetooth is soft-blocked — unblock with: sudo rfkill unblock bluetooth"
  fi
fi

cat <<'EOF'

=== pairing instructions (one-time, local) ===
Run bluetoothctl (interactively; may need sudo depending on policy):

    sudo bluetoothctl

Inside bluetoothctl type:

    power on
    agent on
    default-agent
    scan on

Wait until the Victron device appears, e.g. "Solar Charger", and note its
MAC. Then:

    pair AA:BB:CC:DD:EE:FF
    trust AA:BB:CC:DD:EE:FF
    connect AA:BB:CC:DD:EE:FF
    scan off
    exit

When bluetoothctl asks for the PIN (its own prompt: "Enter PIN code:"),
type it there. The PIN is captured by bluetoothctl itself — it never goes
through the shell, is never written to history, and is never stored by any
deployment script or config. If the device uses passkey confirmation
instead, confirm the six-digit code shown by bluetoothctl with "yes".

The bond material stays under /var/lib/bluetooth/ (root-only). To re-pair
later, remove the device first inside bluetoothctl:

    remove AA:BB:CC:DD:EE:FF
EOF

if ((DRY_RUN)); then
  info "(dry-run) not launching bluetoothctl"
  exit 0
fi

info "launching interactive bluetoothctl — press Ctrl-D to exit it"
exec bluetoothctl
