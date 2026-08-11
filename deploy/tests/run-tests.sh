#!/usr/bin/env bash
# run-tests.sh — non-mutating shell-level tests for the deployment scripts.
#
# Runs on ANY host: no root, no Pi, no systemd, no network. It mocks
# curl/ss/systemctl (and file/readelf/ldd for the binary identity checks)
# through PATH and exercises pure helper functions from lib.sh. Nothing
# outside a private mktemp directory is read or written; no service is
# started or stopped, no files on the host are touched.
set -euo pipefail
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=../scripts/lib.sh
source "$SCRIPT_DIR/../scripts/lib.sh"

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1" >&2; }
check() { # name expected_rc actual_rc
  if [[ $3 -eq $2 ]]; then ok "$1"; else bad "$1 (expected rc $2, got $3)"; fi
}

MOCK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/victron-tests.XXXXXX")
trap 'rm -rf "$MOCK_DIR"' EXIT

# --- mocks (written as heredocs; shellcheck does not analyze them) --------
cat >"$MOCK_DIR/curl" <<'EOF'
#!/usr/bin/env bash
# mock curl: log args, print FAKE_HTTP_CODE (default 000 = unreachable).
printf '%s\n' "$*" >>"${FAKE_CURL_LOG:-/dev/null}"
printf '%s' "${FAKE_HTTP_CODE:-000}"
exit 0
EOF
cat >"$MOCK_DIR/systemctl" <<'EOF'
#!/usr/bin/env bash
# mock systemctl: log mutations; configurable active state.
printf '%s\n' "$*" >>"${FAKE_SYSTEMCTL_LOG:-/dev/null}"
if [[ ${1:-} == show ]]; then
  printf '%s\n' "${FAKE_MAINPID:-0}"
  exit 0
fi
if [[ ${1:-} == is-active ]]; then
  exit "${FAKE_SERVICE_ACTIVE_RC:-0}"
fi
if [[ ${1:-} == is-enabled ]]; then
  exit "${FAKE_SERVICE_ENABLED_RC:-0}"
fi
exit 0
EOF
cat >"$MOCK_DIR/ss" <<'EOF'
#!/usr/bin/env bash
# mock ss: print FAKE_SS_OUTPUT (default: empty).
printf '%s\n' "${FAKE_SS_OUTPUT:-}"
exit 0
EOF
cat >"$MOCK_DIR/file" <<'EOF'
#!/usr/bin/env bash
# mock file(1): print FAKE_FILE_OUTPUT.
printf '%s\n' "${FAKE_FILE_OUTPUT:-ELF 32-bit LSB executable, ARM, EABI5 version 1 (SYSV)}"
exit 0
EOF
cat >"$MOCK_DIR/readelf" <<'EOF'
#!/usr/bin/env bash
# mock readelf -A/-l with the release contract's merged attributes/loader.
if [[ ${1:-} == -l ]]; then
  printf '      [Requesting program interpreter: %s]\n' "${FAKE_INTERPRETER:-/lib/ld-linux-armhf.so.3}"
else
  printf '%s\n' "${FAKE_READELF_ATTRS:-Tag_CPU_arch: v6
Tag_THUMB_ISA_use: Thumb-1
Tag_FP_arch: VFPv2
Tag_ABI_VFP_args: VFP}"
fi
exit 0
EOF
cat >"$MOCK_DIR/ldd" <<'EOF'
#!/usr/bin/env bash
# mock ldd: print FAKE_LDD_OUTPUT (default: no unresolved libs).
printf '%s\n' "${FAKE_LDD_OUTPUT:-}"
exit 0
EOF
cat >"$MOCK_DIR/fake-collector" <<'EOF'
#!/usr/bin/env bash
# fake collector: logs argv; check result can vary by executable basename.
printf '%s|%s\n' "$(basename "$0")" "$*" >>"${FAKE_COLLECTOR_LOG:-/dev/null}"
case "$*" in
*--version*) exit 0 ;;
*--check-config*)
  base=$(basename "$0")
  if [[ $base == victron-collector.* ]]; then
    exit "${FAKE_BACKUP_CHECK_CONFIG_RC:-${FAKE_CHECK_CONFIG_RC:-0}}"
  fi
  var="FAKE_CHECK_CONFIG_RC_$(printf '%s' "$base" | tr '[:lower:].-' '[:upper:]__')"
  rc=${!var:-${FAKE_CHECK_CONFIG_RC:-0}}
  exit "$rc"
  ;;
*) exit 2 ;;
esac
EOF
cat >"$MOCK_DIR/id" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat >"$MOCK_DIR/getent" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat >"$MOCK_DIR/install" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${FAKE_INSTALL_LOG:-/dev/null}"
exit 0
EOF
cat >"$MOCK_DIR/cp" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${FAKE_CP_LOG:-/dev/null}"
command /bin/cp "$@"
EOF
cat >"$MOCK_DIR/sleep" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$MOCK_DIR"/*
export PATH="$MOCK_DIR:$PATH"

FAKE_BIN="$MOCK_DIR/fakebin"
: >"$FAKE_BIN"
export FAKE_CURL_LOG="$MOCK_DIR/curl.log"
export FAKE_COLLECTOR_LOG="$MOCK_DIR/collector.log"
export FAKE_SYSTEMCTL_LOG="$MOCK_DIR/systemctl.log"
export FAKE_INSTALL_LOG="$MOCK_DIR/install.log"
export FAKE_CP_LOG="$MOCK_DIR/cp.log"

# --- config fixtures -------------------------------------------------------
# valid-config.toml mirrors the current collector schema
# (service/apps/victron-collector/src/config.rs): all required poll keys
# present, storage.path inside the default state dir.
cat >"$MOCK_DIR/valid-config.toml" <<'EOF'
[device]
name = "solar-charger"
bluez_alias = "Solar Charger"
instance = 3
adapter = "hci0"

[poll]
active_interval_seconds = 15
idle_interval_seconds = 60
response_timeout_seconds = 8
phase_timeout_seconds = 12
maximum_energy_gap_seconds = 300
spool_claim_ttl_seconds = 120
spool_max_attempts = 5
backoff_base_seconds = 5
backoff_factor = 2
backoff_cap_seconds = 300

[victoria_metrics]
url = "http://100.64.0.2:8429/api/v1/import/prometheus"
request_timeout_seconds = 10

[storage]
path = "/var/lib/victron-collector/state.sqlite3"
maximum_spool_batches = 10000
maximum_spool_age_days = 7
EOF
# invalid-config.toml: missing the required poll keys (phase timeout, spool
# claim/retry/backoff) — the schema drift the example used to have.
cat >"$MOCK_DIR/invalid-config.toml" <<'EOF'
[device]
name = "solar-charger"
bluez_alias = "Solar Charger"
instance = 3
adapter = "hci0"

[poll]
active_interval_seconds = 15
idle_interval_seconds = 60
response_timeout_seconds = 8
maximum_energy_gap_seconds = 300

[victoria_metrics]
url = "http://100.64.0.2:8429/api/v1/import/prometheus"
request_timeout_seconds = 10

[storage]
path = "/var/lib/victron-collector/state.sqlite3"
maximum_spool_batches = 10000
maximum_spool_age_days = 7
EOF
cat >"$MOCK_DIR/cred-config.toml" <<'EOF'
[device]
name = "solar-charger"
bluez_alias = "Solar Charger"
instance = 3
adapter = "hci0"
pin = "1234"

[poll]
active_interval_seconds = 15
idle_interval_seconds = 60
response_timeout_seconds = 8
phase_timeout_seconds = 12
maximum_energy_gap_seconds = 300
spool_claim_ttl_seconds = 120
spool_max_attempts = 5
backoff_base_seconds = 5
backoff_factor = 2
backoff_cap_seconds = 300

[victoria_metrics]
url = "http://100.64.0.2:8429/api/v1/import/prometheus"
request_timeout_seconds = 10

[storage]
path = "/var/lib/victron-collector/state.sqlite3"
maximum_spool_batches = 10000
maximum_spool_age_days = 7
EOF
cat >"$MOCK_DIR/outside-config.toml" <<'EOF'
[device]
name = "solar-charger"
bluez_alias = "Solar Charger"
instance = 3
adapter = "hci0"

[poll]
active_interval_seconds = 15
idle_interval_seconds = 60
response_timeout_seconds = 8
phase_timeout_seconds = 12
maximum_energy_gap_seconds = 300
spool_claim_ttl_seconds = 120
spool_max_attempts = 5
backoff_base_seconds = 5
backoff_factor = 2
backoff_cap_seconds = 300

[victoria_metrics]
url = "http://100.64.0.2:8429/api/v1/import/prometheus"
request_timeout_seconds = 10

[storage]
path = "/tmp/evil.sqlite3"
maximum_spool_batches = 10000
maximum_spool_age_days = 7
EOF
VALID_CONFIG="$MOCK_DIR/valid-config.toml"
INVALID_CONFIG="$MOCK_DIR/invalid-config.toml"
CRED_CONFIG="$MOCK_DIR/cred-config.toml"
OUTSIDE_CONFIG="$MOCK_DIR/outside-config.toml"

# --- 1. pure helpers: ARMv6 arch tag --------------------------------------
if is_armv6_arch_tag v6; then ok "arch tag v6 accepted"; else bad "arch tag v6 rejected"; fi
if is_armv6_arch_tag v6KZ; then ok "arch tag v6KZ accepted"; else bad "arch tag v6KZ rejected"; fi
if is_armv6_arch_tag v6K; then ok "arch tag v6K accepted"; else bad "arch tag v6K rejected"; fi
if is_armv6_arch_tag v6T2; then bad "arch tag v6T2 must be rejected (Thumb-2)"; else ok "arch tag v6T2 rejected"; fi
if is_armv6_arch_tag v7; then bad "arch tag v7 must be rejected"; else ok "arch tag v7 rejected"; fi
if is_armv6_arch_tag v8; then bad "arch tag v8 must be rejected"; else ok "arch tag v8 rejected"; fi
if is_armv6_arch_tag v6Z; then bad "arch tag v6Z must be rejected (not a readelf tag)"; else ok "arch tag v6Z rejected"; fi
if is_armv6_arch_tag ""; then bad "empty arch tag must be rejected"; else ok "empty arch tag rejected"; fi

# --- 2. pure helpers: hard-float ABI --------------------------------------
if is_hard_float_vfp_tag VFP; then ok "VFP tag accepted"; else bad "VFP tag rejected"; fi
if is_hard_float_vfp_tag soft; then bad "soft-float tag must be rejected"; else ok "soft-float tag rejected"; fi
if is_hard_float_vfp_tag ""; then bad "empty VFP tag must be rejected"; else ok "empty VFP tag rejected"; fi

# --- 3. verify_armv6_binary (mocked file/readelf/ldd) ---------------------
rc=0
( FAKE_READELF_ATTRS=$'Tag_CPU_arch: v6\nTag_THUMB_ISA_use: Thumb-1\nTag_FP_arch: VFPv2\nTag_ABI_VFP_args: VFP' verify_armv6_binary "$FAKE_BIN" ) || rc=$?
check "verify_armv6_binary accepts v6 + VFP" 0 "$rc"

rc=0
( FAKE_READELF_ATTRS=$'Tag_CPU_arch: v6KZ\nTag_THUMB_ISA_use: Thumb-1\nTag_FP_arch: VFPv2\nTag_ABI_VFP_args: VFP' verify_armv6_binary "$FAKE_BIN" ) || rc=$?
check "verify_armv6_binary accepts v6KZ + VFP" 0 "$rc"

rc=0
( FAKE_READELF_ATTRS=$'Tag_CPU_arch: v6T2\nTag_THUMB_ISA_use: Thumb-2\nTag_FP_arch: VFPv2\nTag_ABI_VFP_args: VFP' verify_armv6_binary "$FAKE_BIN" ) || rc=$?
check "verify_armv6_binary rejects v6T2 (Thumb-2)" 1 "$rc"

rc=0
( FAKE_READELF_ATTRS=$'Tag_CPU_arch: v7\nTag_THUMB_ISA_use: Thumb-2\nTag_FP_arch: VFPv3-D16\nTag_ABI_VFP_args: VFP' verify_armv6_binary "$FAKE_BIN" ) || rc=$?
check "verify_armv6_binary rejects v7" 1 "$rc"

rc=0
( FAKE_READELF_ATTRS=$'Tag_CPU_arch: v6\nTag_THUMB_ISA_use: Thumb-1\nTag_FP_arch: VFPv2\nTag_ABI_VFP_args: soft' verify_armv6_binary "$FAKE_BIN" ) || rc=$?
check "verify_armv6_binary rejects soft-float" 1 "$rc"

rc=0
( FAKE_READELF_ATTRS=$'Tag_CPU_arch: v6\nTag_THUMB_ISA_use: Thumb-2\nTag_FP_arch: VFPv2\nTag_ABI_VFP_args: VFP' verify_armv6_binary "$FAKE_BIN" ) || rc=$?
check "verify_armv6_binary rejects merged Thumb-2" 1 "$rc"

rc=0
( FAKE_READELF_ATTRS=$'Tag_CPU_arch: v6\nTag_THUMB_ISA_use: Thumb-1\nTag_FP_arch: VFPv3-D16\nTag_ABI_VFP_args: VFP' verify_armv6_binary "$FAKE_BIN" ) || rc=$?
check "verify_armv6_binary rejects merged VFPv3" 1 "$rc"

rc=0
( FAKE_INTERPRETER=/nix/store/fake/lib/ld-linux-armhf.so.3 verify_armv6_binary "$FAKE_BIN" ) || rc=$?
check "verify_armv6_binary rejects nonstandard interpreter" 1 "$rc"

# --- 4. check_vm_reachable (mocked curl) -----------------------------------
: >"$FAKE_CURL_LOG"
rc=0
( FAKE_HTTP_CODE=200 check_vm_reachable "http://x.test:8429/api/v1/import/prometheus" ) || rc=$?
check "vm reachable (HTTP 200)" 0 "$rc"

rc=0
( FAKE_HTTP_CODE=000 check_vm_reachable "http://x.test:8429/api/v1/import/prometheus" ) || rc=$?
check "vm reachable fails on code 000" 1 "$rc"

rc=0
( FAKE_HTTP_CODE=000 check_vm_reachable "http://x.test:8429/api/v1/import/prometheus" unreachable ) || rc=$?
check "vm unreachable (code 000) as expected" 0 "$rc"

rc=0
( FAKE_HTTP_CODE=200 check_vm_reachable "http://x.test:8429/api/v1/import/prometheus" unreachable ) || rc=$?
check "vm unreachable fails on HTTP 200" 1 "$rc"

if grep -q -- '--noproxy' "$FAKE_CURL_LOG"; then ok "curl probe uses --noproxy '*'"; else bad "curl probe missing --noproxy"; fi
if grep -q -- '--data-binary' "$FAKE_CURL_LOG"; then ok "curl probe keeps empty POST body"; else bad "curl probe missing --data-binary"; fi
if grep -q -- '-X POST' "$FAKE_CURL_LOG"; then ok "curl probe keeps POST method"; else bad "curl probe missing -X POST"; fi
if grep -q -- ' -- ' "$FAKE_CURL_LOG"; then ok "curl probe uses -- before URL"; else bad "curl probe missing -- before URL"; fi

# --- 5. ss_has_pid (pure helper: pid matching in ss -lntup output) ----------
# comm is truncated to 15 chars: "victron-collecto" — a name grep for
# "victron-collector" would miss this; pid= matching must catch it.
if ss_has_pid 'users:(("victron-collecto",pid=1234,fd=5))' 1234; then
  ok "ss_has_pid matches pid=1234 (truncated comm)"
else
  bad "ss_has_pid missed pid=1234 (truncated comm)"
fi
if ss_has_pid 'users:(("victron-collecto",pid=12345,fd=5))' 1234; then
  bad "ss_has_pid pid boundary: pid=12345 must not match pid=1234"
else
  ok "ss_has_pid pid boundary: pid=12345 does not match pid=1234"
fi
if ss_has_pid '' 1234; then
  bad "ss_has_pid must not match empty output"
else
  ok "ss_has_pid empty output: no match"
fi

# --- 6. check_no_inbound (mocked systemctl + ss) ----------------------------
rc=0
( FAKE_MAINPID=0 FAKE_SS_OUTPUT="" check_no_inbound ) || rc=$?
check "inactive service (MainPID 0) skipped" 2 "$rc"

if [[ $EUID -ne 0 ]]; then
  rc=0
  ( check_no_inbound ) || rc=$?
  check "non-root listener check skipped" 2 "$rc"
else
  # As root the full path is exercised: active service, clean and with a
  # listening socket attributed to the MainPID.
  rc=0
  ( FAKE_MAINPID=1234 FAKE_SS_OUTPUT="" check_no_inbound ) || rc=$?
  check "no inbound listener (active, clean)" 0 "$rc"
  rc=0
  ( FAKE_MAINPID=1234 FAKE_SS_OUTPUT='users:(("victron-collecto",pid=1234,fd=5))' check_no_inbound ) || rc=$?
  check "listener found via pid= (truncated comm)" 1 "$rc"
fi

# --- 7. exposure-check.sh end-to-end (mocked curl) -------------------------
: >"$FAKE_CURL_LOG"
rc=0
( FAKE_HTTP_CODE=200 "$SCRIPT_DIR/../scripts/exposure-check.sh" --reachable "http://x.test:8429/api/v1/import/prometheus" ) || rc=$?
check "exposure-check --reachable (HTTP 200)" 0 "$rc"

rc=0
( FAKE_HTTP_CODE=000 "$SCRIPT_DIR/../scripts/exposure-check.sh" --unreachable "http://x.test:8429/api/v1/import/prometheus" ) || rc=$?
check "exposure-check --unreachable (code 000)" 0 "$rc"

rc=0
( FAKE_HTTP_CODE=200 "$SCRIPT_DIR/../scripts/exposure-check.sh" --unreachable "http://x.test:8429/api/v1/import/prometheus" ) || rc=$?
check "exposure-check --unreachable fails on HTTP 200" 1 "$rc"

if grep -q -- '--noproxy' "$FAKE_CURL_LOG"; then ok "exposure-check curl uses --noproxy '*'"; else bad "exposure-check curl missing --noproxy"; fi
if grep -q -- ' -- ' "$FAKE_CURL_LOG"; then ok "exposure-check curl uses -- before URL"; else bad "exposure-check curl missing -- before URL"; fi

# --- 8. check_config (mocked collector) -------------------------------------
# Installed-binary mode: VICTRON_BIN_DIR contains the fake collector named
# victron-collector; empty-bin has none (textual fallback path).
mkdir -p "$MOCK_DIR/bin" "$MOCK_DIR/empty-bin"
cp "$MOCK_DIR/fake-collector" "$MOCK_DIR/bin/victron-collector"

# argv regression: the real CLI is '--config PATH --check-config' (the path
# belongs to --config; --check-config is a boolean flag). The old wrong form
# '--check-config PATH' would make clap reject the positional argument.
: >"$FAKE_COLLECTOR_LOG"
rc=0
( VICTRON_BIN_DIR="$MOCK_DIR/bin" check_config "$VALID_CONFIG" ) || rc=$?
check "check_config with installed binary accepts valid config" 0 "$rc"
if grep -q -- '--config .* --check-config' "$FAKE_COLLECTOR_LOG"; then
  ok "check_config argv: --config PATH --check-config"
else
  bad "check_config argv: missing --config PATH --check-config (log: $(cat "$FAKE_COLLECTOR_LOG"))"
fi
if grep -qE -- '--check-config[[:space:]]+/' "$FAKE_COLLECTOR_LOG"; then
  bad "check_config argv: old wrong form '--check-config PATH' used"
else
  ok "check_config argv: no '--check-config PATH' form"
fi

# binary available but rejects config -> hard failure, never textual fallback
rc=0
out=$(VICTRON_BIN_DIR="$MOCK_DIR/bin" FAKE_CHECK_CONFIG_RC=2 check_config "$VALID_CONFIG" 2>&1) || rc=$?
check "check_config fails when installed binary rejects config" 1 "$rc"
if grep -q 'rejected by' <<<"$out"; then
  ok "check_config reports rejection (no silent textual fallback)"
else
  bad "check_config silently fell back to textual sanity"
fi

# binary unavailable -> textual fallback allowed
rc=0
( VICTRON_BIN_DIR="$MOCK_DIR/empty-bin" check_config "$VALID_CONFIG" ) || rc=$?
check "check_config textual fallback when binary unavailable" 0 "$rc"

# explicit binary path mode (used by install/update/rollback)
rc=0
( check_config "$VALID_CONFIG" "$MOCK_DIR/fake-collector" ) || rc=$?
check "check_config explicit binary accepts valid config" 0 "$rc"
rc=0
( FAKE_CHECK_CONFIG_RC=2 check_config "$VALID_CONFIG" "$MOCK_DIR/fake-collector" ) || rc=$?
check "check_config explicit binary rejects invalid config" 1 "$rc"

# credential key rejected
rc=0
( VICTRON_BIN_DIR="$MOCK_DIR/empty-bin" check_config "$CRED_CONFIG" ) || rc=$?
check "check_config rejects credential key" 1 "$rc"

# storage.path outside the unit ReadWritePaths rejected
rc=0
( VICTRON_BIN_DIR="$MOCK_DIR/empty-bin" check_config "$OUTSIDE_CONFIG" ) || rc=$?
check "check_config rejects storage.path outside state dir" 1 "$rc"

# Scoped TOML extraction: single quotes work, same-named keys in other
# sections do not shadow [storage]/[victoria_metrics], and traversal fails.
cat >"$MOCK_DIR/single-quoted.toml" <<'EOF'
[device]
name = 'solar-charger'
bluez_alias = 'Solar Charger'
instance = 3
adapter = 'hci0'
path = '/tmp/not-storage.sqlite3'
url = 'http://wrong.example:1/wrong'

[poll]
active_interval_seconds = 15
idle_interval_seconds = 60
response_timeout_seconds = 8
phase_timeout_seconds = 12
maximum_energy_gap_seconds = 300
spool_claim_ttl_seconds = 120
spool_max_attempts = 5
backoff_base_seconds = 5
backoff_factor = 2
backoff_cap_seconds = 300

[victoria_metrics]
url = 'http://100.64.0.2:8429/api/v1/import/prometheus'
request_timeout_seconds = 10

[storage]
path = '/var/lib/victron-collector/state.sqlite3'
maximum_spool_batches = 10000
maximum_spool_age_days = 7
EOF
rc=0
( VICTRON_BIN_DIR="$MOCK_DIR/empty-bin" check_config "$MOCK_DIR/single-quoted.toml" ) || rc=$?
check "check_config supports scoped single-quoted TOML strings" 0 "$rc"
if [[ $(config_vm_url "$MOCK_DIR/single-quoted.toml") == http://100.64.0.2:8429/api/v1/import/prometheus ]]; then
  ok "config_vm_url ignores device.url"
else
  bad "config_vm_url used wrong section"
fi

sed 's|/var/lib/victron-collector/state.sqlite3|/var/lib/victron-collector/../escape.sqlite3|' \
  "$MOCK_DIR/single-quoted.toml" >"$MOCK_DIR/traversal-config.toml"
rc=0
( VICTRON_BIN_DIR="$MOCK_DIR/empty-bin" check_config "$MOCK_DIR/traversal-config.toml" ) || rc=$?
check "check_config rejects storage traversal" 1 "$rc"

# --- 9. evidence config extraction stays section-scoped and fail-closed ----
value=$(config_bluez_alias "$VALID_CONFIG")
if [[ $value == "Solar Charger" ]]; then ok "history evidence reads scoped BlueZ alias"; else bad "history evidence alias mismatch"; fi
value=$(config_ble_adapter "$VALID_CONFIG")
if [[ $value == hci0 ]]; then ok "history evidence reads scoped adapter"; else bad "history evidence adapter mismatch"; fi
value=$(config_instance "$VALID_CONFIG")
if [[ $value == 3 ]]; then ok "history evidence reads scoped instance"; else bad "history evidence instance mismatch"; fi

cat >"$MOCK_DIR/duplicate-instance.toml" <<'EOF'
[device]
bluez_alias = "Solar Charger"
adapter = "hci0"
instance = 3
instance = 4
EOF
rc=0
toml_u16_value "$MOCK_DIR/duplicate-instance.toml" device instance >/dev/null || rc=$?
check "history evidence rejects duplicate instance" 2 "$rc"

cat >"$MOCK_DIR/wrong-section-instance.toml" <<'EOF'
[other]
instance = 7
[device]
bluez_alias = "Solar Charger"
adapter = "hci0"
instance = 3 # bounded inline comment
EOF
value=$(config_instance "$MOCK_DIR/wrong-section-instance.toml")
if [[ $value == 3 ]]; then ok "history evidence ignores same key in another section"; else bad "history evidence section scope failed"; fi

for invalid in '3.0' '"3"' '-1' '65536'; do
  sed "s/instance = 3/instance = $invalid/" "$MOCK_DIR/wrong-section-instance.toml" >"$MOCK_DIR/invalid-instance.toml"
  rc=0
  toml_u16_value "$MOCK_DIR/invalid-instance.toml" device instance >/dev/null || rc=$?
  check "history evidence rejects invalid instance $invalid" 2 "$rc"
done

# --- 10. install-release.sh mocked install flow (--dry-run, no root) -------
# The fake collector is the --binary; file/readelf/ldd are mocked; every
# mutation is printed by --dry-run. VICTRON_* point at mock dirs;
# VICTRON_STATE_DIR stays at its default so the valid config's storage.path
# (/var/lib/victron-collector/...) stays confined.
INSTALL_ENV=(VICTRON_BIN_DIR="$MOCK_DIR/bin" VICTRON_ETC_DIR="$MOCK_DIR/etc"
  VICTRON_BACKUP_DIR="$MOCK_DIR/backups" VICTRON_UNIT_DIR="$MOCK_DIR/units"
  VICTRON_CONFIG="$MOCK_DIR/etc/config.toml")
rm -rf "${MOCK_DIR:?}/units" "${MOCK_DIR:?}/etc" "${MOCK_DIR:?}/backups"
mkdir -p "$MOCK_DIR/units" "$MOCK_DIR/etc" "$MOCK_DIR/backups"

# A. first install (no unit file yet): never enable/start, even with --no-start
: >"$FAKE_COLLECTOR_LOG"
rc=0
out=$(env "${INSTALL_ENV[@]}" "$SCRIPT_DIR/../scripts/install-release.sh" --dry-run \
  --binary "$MOCK_DIR/fake-collector" --config "$VALID_CONFIG" --no-start 2>&1) || rc=$?
check "install --dry-run --no-start (first install) exits 0" 0 "$rc"
if grep -q '\[dry-run\] systemctl enable' <<<"$out"; then bad "first install must not enable"; else ok "first install: no systemctl enable"; fi
if grep -q '\[dry-run\] systemctl restart' <<<"$out"; then bad "first install must not restart"; else ok "first install: no systemctl restart"; fi
if grep -q 'first install detected' <<<"$out"; then ok "first install message present"; else bad "first install message missing"; fi

# B. reinstall with --no-start (unit present): no enable, no restart
cp "$SCRIPT_DIR/../systemd/victron-collector.service" "$MOCK_DIR/units/victron-collector.service"
cp "$VALID_CONFIG" "$MOCK_DIR/etc/config.toml"
rc=0
out=$(env "${INSTALL_ENV[@]}" "$SCRIPT_DIR/../scripts/install-release.sh" --dry-run \
  --binary "$MOCK_DIR/fake-collector" --config "$VALID_CONFIG" --no-start 2>&1) || rc=$?
check "install --dry-run --no-start (reinstall) exits 0" 0 "$rc"
if grep -q '\[dry-run\] systemctl enable' <<<"$out"; then bad "--no-start must imply no enable"; else ok "--no-start: no systemctl enable"; fi
if grep -q '\[dry-run\] systemctl restart' <<<"$out"; then bad "--no-start must not restart"; else ok "--no-start: no systemctl restart"; fi

# C. reinstall default (no --no-start): enable + restart printed (dry-run)
rc=0
out=$(env "${INSTALL_ENV[@]}" "$SCRIPT_DIR/../scripts/install-release.sh" --dry-run \
  --binary "$MOCK_DIR/fake-collector" --config "$VALID_CONFIG" 2>&1) || rc=$?
check "install --dry-run (reinstall) exits 0" 0 "$rc"
if grep -q '\[dry-run\] systemctl enable' <<<"$out"; then ok "reinstall: enable printed (dry-run)"; else bad "reinstall: enable missing"; fi
if grep -q '\[dry-run\] systemctl restart' <<<"$out"; then ok "reinstall: restart printed (dry-run)"; else bad "reinstall: restart missing"; fi

# D. candidate binary rejects the config -> install fails before unit/start
rc=0
out=$(env FAKE_CHECK_CONFIG_RC=2 "${INSTALL_ENV[@]}" "$SCRIPT_DIR/../scripts/install-release.sh" --dry-run \
  --binary "$MOCK_DIR/fake-collector" --config "$INVALID_CONFIG" --force-config 2>&1) || rc=$?
check "install fails when candidate binary rejects config" 1 "$rc"
if grep -q 'rejected by' <<<"$out"; then ok "install reports config rejection"; else bad "install did not report config rejection"; fi
if grep -q '\[dry-run\] systemctl enable' <<<"$out"; then bad "install must not enable after config rejection"; else ok "install: no enable after config rejection"; fi
if grep -qE '\[dry-run\] (mkdir|install|chown|chmod|systemctl|useradd|groupadd|usermod)' <<<"$out"; then
  bad "install mutated before config rejection"
else
  ok "install rejects config before every mutation"
fi

# --- 10. parse_http_url (strict plaintext HTTP) -----------------------------
if parse_http_url "http://100.64.0.2:8429/api/v1/import/prometheus"; then
  if [[ $URL_SCHEME == http && $URL_HOST == 100.64.0.2 && $URL_PORT == 8429 && $URL_PATH == /api/v1/import/prometheus ]]; then
    ok "parse_http_url: valid URL parsed into scheme/host/port/path"
  else
    bad "parse_http_url: wrong fields (scheme=$URL_SCHEME host=$URL_HOST port=$URL_PORT path=$URL_PATH)"
  fi
else
  bad "parse_http_url rejected valid URL"
fi

reject_url() {
  if parse_http_url "$1"; then bad "parse_http_url accepted: $1"; else ok "parse_http_url rejected: $1"; fi
}
reject_url "https://100.64.0.2:8429/api/v1/import/prometheus"
reject_url "ftp://100.64.0.2:8429/x"
reject_url "HTTP://100.64.0.2:8429/x"
reject_url "http://user:pass@100.64.0.2:8429/x"
reject_url "http://100.64.0.2:8429/x?y=1"
reject_url "http://100.64.0.2:8429/x#frag"
reject_url "http://100.64.0.2:8429/x y"
reject_url "http://100.64.0.2:8429/x$(printf '\t')y"
if parse_http_url "http://100.64.0.2/x" && [[ $URL_PORT == 80 && $URL_PATH == /x ]]; then
  ok "parse_http_url accepts omitted port as 80"
else
  bad "parse_http_url must accept omitted port as 80"
fi
reject_url "http://100.64.0.2:0/x"
reject_url "http://100.64.0.2:99999/x"
if parse_http_url "http://100.64.0.2:08429/x" && [[ $URL_PORT == 8429 ]]; then
  ok "parse_http_url accepts Rust-compatible leading-zero port"
else
  bad "parse_http_url must mirror Rust leading-zero port parsing"
fi
reject_url "http://100.64.0.2:port/x"
if parse_http_url "http://100.64.0.2:8429/../etc/passwd"; then
  ok "parse_http_url mirrors Rust path acceptance for '..' bytes"
else
  bad "parse_http_url diverged from Rust on '..' path bytes"
fi
reject_url "http://100.64.0.2:8429/api\\x"
reject_url "100.64.0.2:8429/x"
reject_url "http://:8429/x"

# --- 11. ipv4_to_int / ipv4_in_cgnat (numeric CGNAT classification) ---------
n=$(ipv4_to_int 100.64.0.0) || n=-1
if [[ $n == 1681915904 ]]; then ok "ipv4_to_int 100.64.0.0 = 1681915904"; else bad "ipv4_to_int wrong: $n"; fi
if ipv4_in_cgnat 100.64.0.2; then ok "100.64.0.2 inside 100.64.0.0/10"; else bad "100.64.0.2 must be inside CGNAT"; fi
if ipv4_in_cgnat 100.64.0.0; then ok "100.64.0.0 (network) inside"; else bad "100.64.0.0 must be inside"; fi
if ipv4_in_cgnat 100.127.255.255; then ok "100.127.255.255 (last) inside"; else bad "100.127.255.255 must be inside"; fi
if ipv4_in_cgnat 100.63.255.255; then bad "100.63.255.255 must be outside"; else ok "100.63.255.255 outside"; fi
if ipv4_in_cgnat 100.128.0.0; then bad "100.128.0.0 must be outside"; else ok "100.128.0.0 outside"; fi
if ipv4_in_cgnat 8.8.8.8; then bad "8.8.8.8 must be outside"; else ok "8.8.8.8 outside"; fi
if ipv4_in_cgnat 100.64.0.2.5; then bad "5-octet must be rejected"; else ok "5-octet rejected"; fi
if ipv4_in_cgnat 100.93.110; then bad "3-octet must be rejected"; else ok "3-octet rejected"; fi
if ipv4_in_cgnat 100.93.110.999; then bad "octet >255 must be rejected"; else ok "octet >255 rejected"; fi
if ipv4_in_cgnat 100.093.110.116; then bad "leading zero must be rejected"; else ok "leading zero rejected"; fi
if ipv4_in_cgnat not-an-ip; then bad "hostname must not classify as IPv4"; else ok "hostname rejected by ipv4_in_cgnat"; fi

# --- 12. check_vm_reachable rejects malformed/option-like URLs --------------
: >"$FAKE_CURL_LOG"
rc=0
( check_vm_reachable "https://100.64.0.2:8429/api/v1/import/prometheus" ) || rc=$?
check "check_vm_reachable rejects https URL" 1 "$rc"
rc=0
( check_vm_reachable "--data-binary" ) || rc=$?
check "check_vm_reachable rejects option-like URL" 1 "$rc"
rc=0
( FAKE_HTTP_CODE=200 check_vm_reachable "http://100.64.0.2:8429/api/v1/import/prometheus" ) || rc=$?
check "check_vm_reachable accepts valid URL" 0 "$rc"
if grep -q -- ' -- ' "$FAKE_CURL_LOG"; then ok "check_vm_reachable curl uses -- before URL"; else bad "check_vm_reachable curl missing -- before URL"; fi

# --- 13. exposure-check.sh rejects malformed URLs before curl ---------------
: >"$FAKE_CURL_LOG"
rc=0
( FAKE_HTTP_CODE=200 "$SCRIPT_DIR/../scripts/exposure-check.sh" --reachable "https://100.64.0.2:8429/api/v1/import/prometheus" ) || rc=$?
check "exposure-check rejects https URL" 1 "$rc"
rc=0
( FAKE_HTTP_CODE=200 "$SCRIPT_DIR/../scripts/exposure-check.sh" --reachable "http://100.64.0.2:8429/api/v1/import/prometheus" ) || rc=$?
check "exposure-check accepts valid URL" 0 "$rc"
if grep -q -- ' -- ' "$FAKE_CURL_LOG"; then ok "exposure-check curl uses -- before URL"; else bad "exposure-check curl missing -- before URL"; fi

# --- 14. enable_start_service (--no-start implies no enable) ---------------
out=$(DRY_RUN=1 NO_ENABLE=0 NO_START=1 enable_start_service 0)
if grep -q '\[dry-run\] systemctl enable' <<<"$out"; then bad "--no-start must imply no enable"; else ok "enable_start_service: --no-start implies no enable"; fi
if grep -q '\[dry-run\] systemctl restart' <<<"$out"; then bad "--no-start must not restart"; else ok "enable_start_service: --no-start does not restart"; fi

out=$(DRY_RUN=1 NO_ENABLE=1 NO_START=1 enable_start_service 0)
if grep -q '\[dry-run\] systemctl' <<<"$out"; then bad "--no-enable --no-start must not touch systemctl"; else ok "enable_start_service: no systemctl with --no-enable --no-start"; fi

out=$(DRY_RUN=1 NO_ENABLE=0 NO_START=0 enable_start_service 0)
if grep -q '\[dry-run\] systemctl enable' <<<"$out"; then ok "reinstall default: enable printed"; else bad "reinstall default: enable missing"; fi
if grep -q '\[dry-run\] systemctl restart' <<<"$out"; then ok "reinstall default: restart printed"; else bad "reinstall default: restart missing"; fi

out=$(DRY_RUN=1 NO_ENABLE=0 NO_START=0 enable_start_service 1)
if grep -q '\[dry-run\] systemctl' <<<"$out"; then bad "first install must not touch systemctl"; else ok "enable_start_service: first install never enables/starts"; fi

# --- 15. rollback prevalidation ordering (real script, mocked mutations) ----
# Run the mutation path without host root by invoking bash with EUID overridden;
# all mutating commands are PATH mocks and all paths live in MOCK_DIR.
ROLL_ROOT="$MOCK_DIR/rollback-root"
mkdir -p "$ROLL_ROOT/bin" "$ROLL_ROOT/backups" "$ROLL_ROOT/state"
cp "$MOCK_DIR/fake-collector" "$ROLL_ROOT/bin/victron-collector"
cp "$MOCK_DIR/fake-collector" "$ROLL_ROOT/bin/victron-cli"
cp "$MOCK_DIR/fake-collector" "$ROLL_ROOT/backups/victron-collector.20260809010101"
cp "$VALID_CONFIG" "$ROLL_ROOT/config.toml"
chmod +x "$ROLL_ROOT/bin/"* "$ROLL_ROOT/backups/"*
ROLL_ENV=(VICTRON_BIN_DIR="$ROLL_ROOT/bin" VICTRON_BACKUP_DIR="$ROLL_ROOT/backups"
  VICTRON_STATE_DIR=/var/lib/victron-collector VICTRON_CONFIG="$ROLL_ROOT/config.toml"
  VICTRON_DB="$ROLL_ROOT/state/state.sqlite3")

: >"$FAKE_INSTALL_LOG"; : >"$FAKE_SYSTEMCTL_LOG"; : >"$FAKE_COLLECTOR_LOG"
rc=0
out=$(env VICTRON_TEST_ASSUME_ROOT=1 FAKE_BACKUP_CHECK_CONFIG_RC=2 "${ROLL_ENV[@]}" \
  "$SCRIPT_DIR/../scripts/rollback-release.sh" --index 1 --no-verify 2>&1) || rc=$?
check "rollback rejects config-incompatible backup" 1 "$rc"
if [[ -s $FAKE_INSTALL_LOG ]]; then bad "rollback restored before config validation"; else ok "rollback: no install before rejected backup validation"; fi
if grep -q '^restart ' "$FAKE_SYSTEMCTL_LOG"; then bad "rollback restarted after rejected backup"; else ok "rollback: no restart after rejected backup validation"; fi

: >"$FAKE_INSTALL_LOG"; : >"$FAKE_SYSTEMCTL_LOG"; : >"$FAKE_COLLECTOR_LOG"
rc=0
out=$(env VICTRON_TEST_ASSUME_ROOT=1 FAKE_BACKUP_CHECK_CONFIG_RC=0 FAKE_SERVICE_ACTIVE_RC=0 \
  "${ROLL_ENV[@]}" "$SCRIPT_DIR/../scripts/rollback-release.sh" --index 1 --no-verify 2>&1) || rc=$?
check "rollback compatible backup completes" 0 "$rc"
if [[ $(wc -l <"$FAKE_INSTALL_LOG" | tr -d ' ') -ge 1 ]]; then ok "rollback installs after validation"; else bad "rollback compatible backup was not installed"; fi
if grep -q '^restart victron-collector$' "$FAKE_SYSTEMCTL_LOG"; then ok "rollback restarts after restore"; else bad "rollback restart missing"; fi
first_event=$(awk 'NR==1{print $1}' "$FAKE_COLLECTOR_LOG")
if [[ $first_event == victron-collector.20260809010101\|--config ]]; then
  ok "rollback validates backup before installed binary"
else
  bad "rollback first validation target wrong: $first_event"
fi

# --- 16. backup ordering follows snapshot timestamps, not preserved mtime ---
ORDER_ROOT="$MOCK_DIR/order-root"
mkdir -p "$ORDER_ROOT"
for name in \
  victron-collector.20260810120000 victron-collector.20260811120000 \
  victron-cli.20260810120000 victron-cli.20260811120000; do
  : >"$ORDER_ROOT/$name"
done
# Make the newest named snapshots look oldest by mtime. Timestamp sorting must
# still select them first and pruning must retain them.
touch -t 202001010000 "$ORDER_ROOT/victron-collector.20260811120000" "$ORDER_ROOT/victron-cli.20260811120000"
touch -t 203001010000 "$ORDER_ROOT/victron-collector.20260810120000" "$ORDER_ROOT/victron-cli.20260810120000"
value=$(VICTRON_BACKUP_DIR="$ORDER_ROOT" newest_backup)
if [[ $value == "$ORDER_ROOT/victron-collector.20260811120000" ]]; then ok "newest backup follows filename timestamp"; else bad "newest backup followed mtime: $value"; fi
VICTRON_BACKUP_DIR="$ORDER_ROOT" prune_backups 1
if [[ -f $ORDER_ROOT/victron-collector.20260811120000 && -f $ORDER_ROOT/victron-cli.20260811120000 ]]; then
  ok "backup pruning retains newest named pair"
else
  bad "backup pruning removed newest named pair"
fi
if [[ ! -e $ORDER_ROOT/victron-collector.20260810120000 && ! -e $ORDER_ROOT/victron-cli.20260810120000 ]]; then
  ok "backup pruning removes oldest named pair"
else
  bad "backup pruning retained oldest named pair"
fi

# --- 17. update automatic rollback prevalidation ordering ------------------
UP_ROOT="$MOCK_DIR/update-root"
mkdir -p "$UP_ROOT/bin" "$UP_ROOT/backups" "$UP_ROOT/state"
printf 'old collector\n' >"$UP_ROOT/bin/victron-collector"
printf 'old cli\n' >"$UP_ROOT/bin/victron-cli"
cp "$MOCK_DIR/fake-collector" "$UP_ROOT/new-collector"
cp "$VALID_CONFIG" "$UP_ROOT/config.toml"
chmod +x "$UP_ROOT/bin/"* "$UP_ROOT/new-collector"
UP_ENV=(VICTRON_BIN_DIR="$UP_ROOT/bin" VICTRON_BACKUP_DIR="$UP_ROOT/backups"
  VICTRON_STATE_DIR=/var/lib/victron-collector VICTRON_CONFIG="$UP_ROOT/config.toml"
  VICTRON_DB="$UP_ROOT/state/state.sqlite3")

: >"$FAKE_INSTALL_LOG"; : >"$FAKE_SYSTEMCTL_LOG"; : >"$FAKE_COLLECTOR_LOG"; : >"$FAKE_CP_LOG"
rc=0
out=$(env VICTRON_TEST_ASSUME_ROOT=1 FAKE_SERVICE_ACTIVE_RC=1 FAKE_BACKUP_CHECK_CONFIG_RC=2 \
  "${UP_ENV[@]}" "$SCRIPT_DIR/../scripts/update-release.sh" --binary "$UP_ROOT/new-collector" --no-verify 2>&1) || rc=$?
check "update failure reports rollback failure" 1 "$rc"
# One install is the candidate; a second would mean the incompatible backup
# replaced it. Prevalidation must stop before that second install.
install_count=$(wc -l <"$FAKE_INSTALL_LOG" | tr -d ' ')
if [[ $install_count == 1 ]]; then ok "update leaves candidate when backup rejects config"; else bad "update restored incompatible backup (install count $install_count)"; fi
restart_count=$(grep -c '^restart victron-collector$' "$FAKE_SYSTEMCTL_LOG" || true)
if [[ $restart_count == 1 ]]; then ok "update does not restart incompatible rollback backup"; else bad "unexpected update restart count: $restart_count"; fi
if grep -q 'rollback backup rejects the effective config' <<<"$out"; then ok "update reports backup config incompatibility"; else bad "update missing rollback incompatibility report"; fi

# --- summary ---------------------------------------------------------------
printf '\n=== tests: %s passed, %s failed ===\n' "$PASS" "$FAIL"
((FAIL == 0))
