#!/usr/bin/env bash
# Config field extraction and strict URL parsing for deployment scripts.
# Sourced by lib.sh; not an executable entry point.

# Extract one single-line TOML string scoped to SECTION. Returns 0 found,
# 1 missing, 2 malformed/duplicate/escaped/multiline.
toml_string_value() {
  local cfg=$1 wanted_section=$2 wanted_key=$3 line trimmed section='' rhs value rest found=0
  while IFS= read -r line || [[ -n $line ]]; do
    line=${line%$'\r'}
    trimmed=${line#"${line%%[![:space:]]*}"}
    [[ -z $trimmed || $trimmed == \#* ]] && continue
    if [[ $trimmed =~ ^\[[[:space:]]*([A-Za-z0-9_-]+)[[:space:]]*\][[:space:]]*(#.*)?$ ]]; then
      section=${BASH_REMATCH[1]}
      continue
    fi
    [[ $section == "$wanted_section" ]] || continue
    if [[ $trimmed =~ ^${wanted_key}[[:space:]]*=[[:space:]]*(.*)$ ]]; then
      ((found == 0)) || return 2
      rhs=${BASH_REMATCH[1]}
      case $rhs in
      \"*)
        rhs=${rhs:1}
        [[ $rhs == *\"* ]] || return 2
        value=${rhs%%\"*}
        rest=${rhs#*\"}
        [[ $value != *\\* ]] || return 2
        ;;
      \'*)
        rhs=${rhs:1}
        [[ $rhs == *\'* ]] || return 2
        value=${rhs%%\'*}
        rest=${rhs#*\'}
        ;;
      *) return 2 ;;
      esac
      [[ $rest =~ ^[[:space:]]*(#.*)?$ ]] || return 2
      found=1
    fi
  done <"$cfg"
  ((found == 1)) || return 1
  printf '%s\n' "$value"
}

required_toml_string() {
  local cfg=$1 section=$2 key=$3 value rc
  if value=$(toml_string_value "$cfg" "$section" "$key"); then
    printf '%s\n' "$value"
    return 0
  else
    rc=$?
  fi
  ((rc == 1)) && die "config missing [$section] $key string: $cfg"
  die "config has malformed, duplicate, escaped, or multiline [$section] $key string: $cfg"
}

config_storage_path() { required_toml_string "$1" storage path; }
config_vm_url() { required_toml_string "$1" victoria_metrics url; }
config_ble_adapter() { required_toml_string "$1" device adapter; }

check_config() {
  local cfg=$1 bin=${2:-} storage
  [[ -f $cfg ]] || die "config not found: $cfg"
  grep -qiE '^\s*(pin|puk|passcode|password|secret|bond[_-]?key)\s*=' "$cfg" &&
    die "config contains a credential key (pin/puk/passcode/...) — forbidden: $cfg"

  if [[ -n $bin ]]; then
    [[ -x $bin ]] || die "collector binary not executable: $bin"
    "$bin" --config "$cfg" --check-config >/dev/null 2>&1 ||
      die "config rejected by $bin --config $cfg --check-config: $cfg"
    info "config parses with $bin --config $cfg --check-config: $cfg"
  elif [[ -x $VICTRON_BIN_DIR/$VICTRON_BINARY ]]; then
    "$VICTRON_BIN_DIR/$VICTRON_BINARY" --config "$cfg" --check-config >/dev/null 2>&1 ||
      die "config rejected by installed $VICTRON_BINARY --config $cfg --check-config: $cfg"
    info "config parses with $VICTRON_BINARY --config $cfg --check-config: $cfg"
  else
    grep -q '^\s*\[device\]' "$cfg" || die "config missing [device] section: $cfg"
    grep -q '^\s*\[victoria_metrics\]' "$cfg" || die "config missing [victoria_metrics] section: $cfg"
    info "config textual sanity OK (collector binary unavailable): $cfg"
  fi

  storage=$(config_storage_path "$cfg")
  if [[ $storage != "$VICTRON_STATE_DIR"/* || $storage == *'/../'* || $storage == */.. || $storage == *'//'* ]]; then
    die "storage.path '$storage' is outside or escapes $VICTRON_STATE_DIR (unit ReadWritePaths) — fix config or unit"
  fi
  config_vm_url "$cfg" >/dev/null
}

# Mirrors the Rust collector/client URL contract. Sets URL_SCHEME, URL_HOST,
# URL_PORT and URL_PATH on success.
parse_http_url() {
  local url=$1 rest authority path host port port_number normalized i ch ord
  URL_SCHEME='' URL_HOST='' URL_PORT='' URL_PATH=''
  [[ $url == http://* ]] || return 1
  rest=${url#http://}
  [[ $rest == *'?'* || $rest == *'#'* ]] && return 1
  if [[ $rest == */* ]]; then
    authority=${rest%%/*}; path=/${rest#*/}
  else
    authority=$rest; path=/api/v1/import/prometheus
  fi
  [[ -n $authority && $authority != *@* ]] || return 1
  if [[ $authority == *:* ]]; then
    host=${authority%:*}; port=${authority##*:}
    [[ -n $host && $host != *:* && $host != \[* && $port =~ ^[0-9]+$ ]] || return 1
    normalized=$port
    while [[ ${#normalized} -gt 1 && $normalized == 0* ]]; do normalized=${normalized:1}; done
    [[ ${#normalized} -le 5 ]] || return 1
    port_number=$((10#$normalized))
    ((port_number >= 1 && port_number <= 65535)) || return 1
  else
    host=$authority; port_number=80
  fi
  [[ -n $host && ${#host} -le 253 && $host =~ ^[A-Za-z0-9._-]+$ ]] || return 1
  LC_ALL=C
  for ((i = 0; i < ${#path}; i++)); do
    ch=${path:i:1}; printf -v ord '%d' "'$ch"
    ((ord >= 0x21 && ord <= 0x7e)) || return 1
    case $ch in
    ' ' | '"' | '<' | '>' | '\' | '^' | '`' | '{' | '|' | '}' | '#' | '?') return 1 ;;
    esac
  done
  URL_SCHEME=http URL_HOST=$host URL_PORT=$port_number URL_PATH=$path
}
