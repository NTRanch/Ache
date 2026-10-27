# ache

The unified haptics MCP stdio server, built by Nice Thoughts Ranch.
One server, two device providers behind a seam:

- **Lovense** — toys over the Lovense Standard API **local Game Mode**
  route (the original server, carried over unchanged in behavior).
- **Buttplug** — devices over an embedded, in-process Buttplug server
  (the Rust `buttplug` crates; no Intiface instance needed). BLE and
  USB-serial hardware managers are wired; the websocket-server
  manager is deliberately not (it would open a listening socket), and
  neither are the Lovense dongle/connect managers — the Lovense
  provider already covers Lovense hardware.

The governing brief is [`../MCP-BRIEF.md`](../MCP-BRIEF.md) (see its
2026-10-27 amendment for the unification); build tracking is Phase 6
of [`../CHECKLIST.md`](../CHECKLIST.md).

## The safety contract

The server's reason to exist is its safety contract; everything else
is plumbing. The contract, as implemented — and since the unification
it binds **both** providers, enforced by the same code:

- **Master off by default.** Output is possible only in the armed
  state, and arming is always the explicit `lovense_arm` tool call —
  never a side effect of connecting, discovering, or a prior session.
- **One pure `decide()` gate.** Every output on either provider passes
  through it exactly once. It validates, clamps, and plans; Lovense
  wire bodies are constructed inside it, and providers execute only
  gate-approved plans — a provider never sees a raw intent. A deny is
  final, and no tool schema carries an override parameter.
- **Ceiling clamped in code.** The configured intensity ceiling
  (default 20) clamps every outbound strength — a function strength
  and each value inside a pattern strength list — and a target's own
  range clamps further where narrower (Lovense Pump, Depth: 3; a
  Buttplug device's reported range). Results report the effective
  values actually sent.
- **Capabilities are data, and they bind.** A unified device reports
  what it can do; asking it for a feature it does not offer — or a
  Lovense preset aimed at a Buttplug device — is a typed
  `capability_unsupported` refusal, not a silent no-op.
- **Synchronous stop.** `lovense_stop` zeroes every device on both
  providers before it answers, and latches the master off. It is
  callable in any state and is never gated.
- **No resume on reconnect.** A dropped session zeroes output and
  latches off; so do a watchdog trip and the shutdown path. Recovery
  is always a fresh explicit arm.
- **Heartbeat watchdog (5 s default).** While output is active, every
  tool call is a heartbeat; silence past the window zeroes output on
  both providers — the serve loop fires it even when no further
  request arrives.
- **Bounded durations.** `timeSec: 0` means indefinite output on the
  Lovense API; no output tool can emit it, on either provider (the
  Buttplug provider schedules the zero itself, since its commands
  carry no duration). Durations run 1..=3600 s.
- **No credentials.** The local Lovense route needs no token and the
  server holds none; the config loader refuses credential-shaped keys.

Tools (protocol `2024-11-05`, newline-delimited JSON-RPC on stdio):

| Tool | What it does |
| --- | --- |
| `lovense_status` | State (`unconfigured`, `mock`, `ready`), transport, target, armed, output active, session health, ceiling, watchdog, version, plus a per-provider account under `providers`. Makes no network call. |
| `lovense_toys` | The Lovense provider's toys, original shape. Mock: the two fixture toys. Real with a host: one `GetToys` request, result reported honestly. Real without a host: `target_unconfigured` error and no network call. Safe while disarmed; arms nothing. |
| `ache_devices` | Every device across both providers in the unified model: `lovense:<toy id>` / `buttplug:<index>` ids, provider, name, battery, capabilities as data (each with its max on the unified 0..=20 scale, or `null` when reported but unmapped). A provider that cannot enumerate reports its typed error in its entry; the call fails only when no provider could enumerate. |
| `ache_device` | One device by unified id, with its full capability list. Absence is `device_not_found`; a provider that cannot look says so instead. |
| `lovense_arm` | Arm the master toggle (both providers). Refuses without any target. Starts no output by itself. |
| `lovense_disarm` | Master off; active output is zeroed on both providers synchronously first. |
| `lovense_function` | One function, one strength, bounded duration. Gate-clamped; result reports effective values. With a `device` argument (unified id) it routes to that device on either provider and plans against its reported capabilities; without one it is the original Lovense call. |
| `lovense_pattern` | A pattern built from validated parts: feature letters `v r p t f s d o`, interval strictly above 100 ms, at most 50 strengths (each clamped). A raw rule string is never accepted. On a Buttplug device the provider steps the sequence itself and zeroes what it drove at the end. |
| `lovense_preset` | A built-in Remote app preset (`pulse`, `wave`, `fireworks`, `earthquake`); other names are refused. Presets exist only on the Lovense provider: a `device` naming a Buttplug device is refused `capability_unsupported`. |
| `lovense_stop` | Panic zero across both providers: synchronous, idempotent, latching, always available. |

Errors are typed (`{"error": {"code", "class", "message"}}`). The
unification added three codes: `provider_unavailable` (a provider
cannot serve at all), `capability_unsupported` (the device exists but
does not offer that), and `device_not_found` (no provider can see
that id).

## Configuration

Set `ACHE_MCP_CONFIG` to a TOML file (renamed from
`LOVENSE_MCP_CONFIG`; the old name is no longer read). Unset, the
server runs with the defaults below (mock transport, fixture Buttplug
provider, no real target). Set, the file must exist and validate;
otherwise the server exits with status 2 and a JSON error on stderr.
Unknown keys are refused, at every level.

```toml
transport = "mock"        # "mock" | "real"
host = ""                 # bare hostname or IPv4; empty = no target
port = 20010              # 1..=65535 (the app's HTTPS port is usually 30010)
use_https = false
intensity_ceiling = 20    # 0..=20; clamps every outbound strength in code
watchdog_secs = 5         # 1..=60; silence window while output is active

[buttplug]
enabled = true            # the embedded Buttplug provider
scan_ms = 1500            # 100..=10000; one discovery scan's window
```

Host and port are always configuration, never constants: the app's
ports move. Over HTTPS the app is addressed as
`https://{dashed-ip}.lovense.club:{ssl-port}` — that dashed hostname
is the `host` value; the code never rewrites it. In mock mode both
providers serve fixtures, labelled as fixtures everywhere their data
appears. The real Lovense transport and the embedded Buttplug stack
are exercised against hardware only on the separately gated live rung
(S4; see `../TESTING.md`).

## Wire contracts

Lovense: `POST {scheme}://{host}:{port}/command`, header
`X-platform: ache`. Two-second timeout, no redirects, response capped
at 256 KiB; every reply must carry `"code": 200`, and a refusal code
is surfaced, never swallowed. Command bodies (constructed only by the
gate):

- Discovery: `{"command":"GetToys"}`; `data.toys` may be a JSON object
  or a JSON string holding one (the documented shape). A toy's
  reported `fullFunctionNames` become its unified capabilities when
  present.
- Function (apiVer 1): `{"command":"Function","action":"Vibrate:12","timeSec":10,"toy":"","apiVer":1}`;
  stop is the same shape with `"action":"Stop","timeSec":0`.
- Pattern (apiVer 2): `{"command":"Pattern","rule":"V:1;F:v;S:1000#","strength":"20;20;5","timeSec":9,"toy":"","apiVer":2}`.
- Preset (apiVer 1): `{"command":"Preset","name":"pulse","timeSec":9,"toy":"","apiVer":1}`.

Buttplug: the protocol runs in-process between the embedded server
and its client; strengths on the unified scale convert to the
device's percent range (`strength / 20`). Unified mappings: Vibrate,
Rotate, Oscillate directly; Pump drives Buttplug's constriction
output (the air-pump action); position outputs are reported as
capabilities but not mapped (position is not a strength).

## Build and test

```sh
nix develop          # cargo, clippy, rustfmt, python3, pkg-config, dbus
cargo test           # mock + fixture providers; no hardware involved
nix build
scripts/stdio-smoke.sh "$(command -v ache)"   # scripted full-session smoke
```

Building outside Nix needs the D-Bus development files
(`pkg-config dbus-1`) for the BLE stack's `libdbus-sys`.

Licensed Apache-2.0 (see the repository `LICENSE`).
