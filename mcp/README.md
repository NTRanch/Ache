# lovense-mcp

An MCP stdio server over the Lovense Standard API **local Game Mode**
route, built by Nice Thoughts Ranch. The governing brief is
[`../MCP-BRIEF.md`](../MCP-BRIEF.md); build tracking is Phase 6 of
[`../CHECKLIST.md`](../CHECKLIST.md).

## Stage S2: read core + gated output core

The server's reason to exist is its safety contract; everything else
is plumbing. The contract, as implemented:

- **Master off by default.** Output is possible only in the armed
  state, and arming is always the explicit `lovense_arm` tool call —
  never a side effect of connecting, discovering, or a prior session.
- **A pure `decide()` gate.** Every output passes through it exactly
  once. It validates, clamps, and constructs the exact wire body;
  callers never assemble one. A deny is final, and no tool schema
  carries an override parameter.
- **Ceiling clamped in code.** The configured intensity ceiling
  (default 20) clamps every outbound strength — a function strength
  and each value inside a pattern strength list — and a feature's own
  API range clamps further where narrower (Pump, Depth: 3). Results
  report the effective values actually sent.
- **Synchronous stop.** `lovense_stop` puts the zero on the wire
  before it answers, zeroes every feature on every toy, and latches
  the master off. It is callable in any state and is never gated.
- **No resume on reconnect.** A dropped session zeroes output and
  latches off; so do a watchdog trip and the shutdown path. Recovery
  is always a fresh explicit arm.
- **Heartbeat watchdog (5 s default).** While output is active, every
  tool call is a heartbeat; silence past the window zeroes output —
  the serve loop fires it even when no further request arrives.
- **Bounded durations.** `timeSec: 0` means indefinite output on this
  API; no output tool can emit it. Durations run 1..=3600 s.
- **No credentials.** The local route needs no token and the server
  holds none; the config loader refuses credential-shaped keys.

Tools (protocol `2024-11-05`, newline-delimited JSON-RPC on stdio):

| Tool | What it does |
| --- | --- |
| `lovense_status` | State (`unconfigured`, `mock`, `ready`), transport, target, armed, output active, session health, ceiling, watchdog, version. Makes no network call. |
| `lovense_toys` | Mock: the two fixture toys. Real with a host: one `GetToys` request, result reported honestly. Real without a host: `target_unconfigured` error and no network call. Safe while disarmed; arms nothing. |
| `lovense_arm` | Arm the master toggle. Refuses without a target. Starts no output by itself. |
| `lovense_disarm` | Master off; active output is zeroed on the wire synchronously first. |
| `lovense_function` | One function, one strength, bounded duration. Gate-clamped; result reports effective values. |
| `lovense_pattern` | A pattern built from validated parts: feature letters `v r p t f s d o`, interval strictly above 100 ms, at most 50 strengths (each clamped). A raw rule string is never accepted. |
| `lovense_preset` | A built-in Remote app preset (`pulse`, `wave`, `fireworks`, `earthquake`); other names are refused. |
| `lovense_stop` | Panic zero: synchronous, idempotent, latching, always available. |

## Configuration

Set `LOVENSE_MCP_CONFIG` to a TOML file. Unset, the server runs with
the defaults below (mock transport, no target). Set, the file must exist
and validate; otherwise the server exits with status 2 and a JSON error
on stderr. Unknown keys are refused.

```toml
transport = "mock"        # "mock" | "real"
host = ""                 # bare hostname or IPv4; empty = no target
port = 20010              # 1..=65535 (the app's HTTPS port is usually 30010)
use_https = false
intensity_ceiling = 20    # 0..=20; clamps every outbound strength in code
watchdog_secs = 5         # 1..=60; silence window while output is active
```

Host and port are always configuration, never constants: the app's
ports move. Over HTTPS the app is addressed as
`https://{dashed-ip}.lovense.club:{ssl-port}` — that dashed hostname is
the `host` value; the code never rewrites it. The mock transport is
the default and, in this build, the only transport exercised; the real
transport exists for discovery and carries gated sends for the later
live rung (S4, separately gated).

## Wire contract

`POST {scheme}://{host}:{port}/command`, header `X-platform`. Two-second
timeout, no redirects, response capped at 256 KiB; every reply must
carry `"code": 200`, and a refusal code is surfaced, never swallowed.
Command bodies (constructed only by the gate):

- Discovery: `{"command":"GetToys"}`; `data.toys` may be a JSON object
  or a JSON string holding one (the documented shape).
- Function (apiVer 1): `{"command":"Function","action":"Vibrate:12","timeSec":10,"toy":"","apiVer":1}`;
  stop is the same shape with `"action":"Stop","timeSec":0`.
- Pattern (apiVer 2): `{"command":"Pattern","rule":"V:1;F:v;S:1000#","strength":"20;20;5","timeSec":9,"toy":"","apiVer":2}`.
- Preset (apiVer 1): `{"command":"Preset","name":"pulse","timeSec":9,"toy":"","apiVer":1}`.

## Build and test

```sh
nix develop          # cargo, clippy, rustfmt, python3
cargo test           # the transport tests start a loopback python3 stub
nix build
scripts/stdio-smoke.sh "$(command -v lovense-mcp)"   # scripted full-session smoke
```

Licensed Apache-2.0 (see the repository `LICENSE`).
