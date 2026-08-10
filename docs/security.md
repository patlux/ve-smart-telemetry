# Security & exposure — victron-collector

## Exposure statement

The victron-collector service exposes **no inbound port** on the Raspberry
Pi. It ships no listener (`/metrics` server, web UI, or socket) and the
systemd unit contains no `Port=`/`Socket=`/`ListenStream=`. All its
communication is **outbound HTTP** to exactly one destination: the
configured `victoria_metrics.url`.

"No listener" is an **application contract verified at runtime**, not a
property the hardening directives enforce: `RestrictAddressFamilies=`,
`CapabilityBoundingSet=` empty, and the rest of the baseline unit still
allow `bind()`/`listen()` on an ordinary IP socket (they only remove
raw/packet socket families and privileged binds). The verification is
`ss -lntup` (listening TCP and UDP sockets) attributed to the collector
process, run as root.

| Property | Value |
|---|---|
| Inbound | none — runtime-verified: as root, `ss -lntup` shows no listening socket owned by the collector (see Verification) |
| Outbound | `POST http://100.64.0.2:8429/api/v1/import/prometheus` (TCP 8429, HTTP/1.1, no TLS) |
| Who can reach the Pi | nobody through this service — no listener, no forwarded port, no tunnel |
| Who can reach the endpoint | hosts on the tailnet only; see reachability below |
| Authentication | none at the VictoriaMetrics endpoint today (plain HTTP, internal network) — assumption, see below |
| Source restriction | tailnet CGNAT route; optionally enforced in-kernel via a deny-by-default egress allowlist in the unit (see Route restriction) |
| Public Internet | not reachable by design; must be verified, see Verification |

The `100.64.0.2` address lies inside Tailscale's CGNAT range
`100.64.0.0/10`. The intended path is: Pi → tailnet → VictoriaMetrics on
`metrics-node`. No DNS name, no public IP, no port-forward is involved.

## Assumptions

1. **No TLS** at the VictoriaMetrics endpoint — the collector is built
   without TLS features (`reqwest` default features disabled). The plan
   targets an internal HTTP endpoint only. If the endpoint ever moves to
   HTTPS or becomes reachable from non-tailnet networks, the collector's
   request layer must be extended and this document updated.
2. **No authentication** at the endpoint — no API key, no basic auth.
   Reachability control relies on the tailnet route. This is acceptable
   only while the port is not publicly reachable; verify as below.
3. **Bond material** stays in BlueZ storage (`/var/lib/bluetooth/`,
   root-only) and in config-free form: no PIN, PUK, bond key, or raw
   capture exists in this repository or in any deployed file.
4. The Raspberry Pi's own network is trusted (home/tailnet); the VM
   endpoint must never be exposed to the public Internet.

## Route restriction

The strictest egress option is the commented unit block
`IPAddressAllow=100.64.0.0/10` + `IPAddressDeny=any`. Two facts matter:

- **`IPAddressAllow=` alone does not create a deny-by-default policy.**
  systemd allows everything that is not explicitly denied, so an allowlist
  only exists when the allow entries are paired with `IPAddressDeny=any`.
- **Semantics** (systemd.resource-control(5)): the rules are applied in
  turn — access is granted when the address matches an `IPAddressAllow=`
  entry, otherwise denied when it matches an `IPAddressDeny=` entry,
  otherwise granted. The allow list is evaluated before the deny list, so
  **Allow has precedence over Deny**; the textual order of the lines in the
  unit file is irrelevant (all `IPAddressAllow=` lines combine into one
  list, all `IPAddressDeny=` lines into another). With
  `IPAddressAllow=100.64.0.0/10` + `IPAddressDeny=any` the tailnet CGNAT
  range is allowed and everything else is denied.
- **`any` is aggressive**: it also blocks loopback (`127.0.0.0/8`, `::1`)
  and DNS lookups, so if the config URL ever becomes a DNS name you must
  also allow the resolver's address (e.g. `IPAddressAllow=192.168.0.1/32`),
  and any other service the collector needs locally must be allowed
  explicitly. D-Bus is AF_UNIX and is unaffected by IP filtering.

This block is **not active by default**. Enable it incrementally, one line
at a time, and re-verify after each change (`systemctl daemon-reload`,
restart, `verify-installation.sh --strict`, one live read).

The baseline unit instead limits address families
(`RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`) and capabilities
(`CapabilityBoundingSet=` empty, `NoNewPrivileges=yes`). Those reduce the
attack surface — they exclude raw/packet socket families and privileged
binds — but they do **not** by themselves prevent `bind()`/`listen()` on an
allowed IP family. That remains an application contract, verified at
runtime by the `ss` checks.

## Credential handling

- The Victron **PIN exists only transiently inside bluetoothctl's own
  prompt** during the one-time pairing (`deploy/scripts/pair-device.sh`).
  It never enters the shell, shell history, process arguments, config, or
  logs. No PIN literal is present anywhere in this repository
  (`grep -ri pin deploy docs` returns nothing relevant).
- If hidden input is ever needed interactively, use zsh-safe syntax
  (`IFS= read -r -s 'VAR?Prompt: ' && printf '\n'`), never `read -p`.
- `verify-installation.sh` fails any config containing
  `pin`/`puk`/`passcode`/`password`/`secret`/bond-key keys.

## Verification

Intended path (the Pi, tailnet up) — expect an HTTP response (non-000);
the empty POST writes no metrics:

```bash
curl -sS -o /dev/null -w '%{http_code}\n' --noproxy '*' --max-time 8 \
  -X POST --data-binary '' http://100.64.0.2:8429/api/v1/import/prometheus
```

`--noproxy '*'` forces a direct connection so the probe measures
reachability of the endpoint itself, not of an HTTP(S)_PROXY/ALL_PROXY
proxy.

Unintended path — from a host **not** on the tailnet (e.g. a laptop with
the tailnet off), the endpoint must be unreachable (code `000` / timeout).
Any HTTP response means the endpoint is exposed beyond the tailnet and
must be fixed. Use the dedicated read-only, network-only probe — it
performs only the reachability assertion and can run on any host:

```bash
# from the external (off-tailnet) host:
deploy/scripts/exposure-check.sh \
  --unreachable http://100.64.0.2:8429/api/v1/import/prometheus
# expected: [PASS] unreachable as expected ... — exit 0
```

`verify-installation.sh` is **not** suitable as the external probe: it
verifies the local installation (binaries, unit, service, BLE, database)
before any reachability check, which requires root on the Pi itself.

On the Pi, as root, `ss -lntup` must show no listening TCP or UDP socket
owned by the collector. Attribution is **by PID, not by process name**:
Linux task comm is truncated to 15 characters, so `victron-collector`
(17 characters) appears as `victron-collecto` in `ss -p` output and a name
grep can miss it. `verify-installation.sh` reads the service MainPID from
`systemctl show` and greps for `pid=<PID>` in the root `ss -lntup` output:

```bash
sudo systemctl show -p MainPID --value victron-collector   # e.g. 1234
sudo ss -lntup | grep 'pid=1234'                           # expect NO output
```

If the service is inactive (MainPID 0) the check is skipped with a warning
rather than reported as clean. Without root, `ss -p` cannot attribute
sockets to processes, so the listener check is skipped with a warning
rather than reported as clean.

## Hardening summary

Baseline (active): `NoNewPrivileges`, `ProtectSystem=strict` (state dir
writable via `ReadWritePaths`), `ProtectHome`, `PrivateTmp`, restricted
namespaces/syscalls (`SystemCallFilter=@system-service`), restricted
address families (excludes raw/packet socket families — does not by itself
prevent listen), empty capability bounding set (removes privileged binds —
does not by itself prevent unprivileged binds above port 1024), `UMask=0077`,
bounded tasks/memory, restart policy with crash-loop bound. D-Bus access is
preserved: AF_UNIX to `/run/dbus/system_bus_socket` is allowed and
`PrivateUsers=yes` is deliberately **not** set (it would break the system
bus). The absence of a listener is an application contract verified at
runtime by `verify-installation.sh`.

Extended (enable incrementally, one line at a time, verify after each):
`IPAddressAllow=100.64.0.0/10` + `IPAddressDeny=any` (deny-by-default
egress allowlist; see Route restriction), `PrivateDevices=yes`,
`ProtectProc=invisible`, `ProcSubset=pid`,
`SystemCallArchitectures=native`.

## Risk table

| Risk | Mitigation |
|---|---|
| VM endpoint reachable from public Internet | verify from an off-tailnet host with `exposure-check.sh --unreachable`; never publish port 8429; optional deny-by-default egress allowlist |
| Pi used as pivot | no listener (application contract, runtime-verified via `ss -lntup`); raw/packet sockets excluded by `RestrictAddressFamilies` + empty capability set; outbound restricted to tailnet only when the deny-by-default allowlist is enabled |
| credentials in repo/config | enforced absent; pairing PIN lives only in bluetoothctl's prompt |
| D-Bus hardening breaks BLE | baseline is D-Bus-safe; extended lines are incremental and verified one by one |
