# Conformance — ache 1.0.0 (R-401 / R-402 evidence)

Recorded 2026-10-28 on primo against the built 1.0.0 binary
(`mcp/target/debug/ache`), mock transport + fixture Buttplug
provider. No live hardware was touched: the live rung stays
Owner-gated (U5 — this is conformance evidence, not device
work). Modern era only (2026-07-28), rmcp 3.8.0.

## Conformance pass (MCP Inspector-style, modern-era strict)

Driven the way the MCP Inspector drives a server: open,
discover the surface, call representative tools, probe the
edges. The Inspector CLI itself opens with a legacy
`initialize`; against a modern-only server that is the
designed `-32022` refusal (fleet-known CLI limitation,
recorded at torori T0 and here again, verbatim below), and
the same session continues serving modern frames after it.

Verdicts: discover advertises exactly `2026-07-28` and
serverInfo `ache 1.0.0`; tools/list returns the 10 tools;
`lovense_status` reports state `mock`, disarmed; the unified
`ache_devices` enumerates 4 devices across both providers
(2 Lovense fixtures + 2 Buttplug fixtures); `ache_device`
returns one device by unified id; an absent id is the typed
`device_not_found`; an unknown tool is protocol error
`-32602`; a malformed line is answered `-32700` and the
session survives. The shipped `scripts/stdio-smoke.sh` full
session also passes at 1.0.0 (discover; disarmed denied;
arm; 25 clamped to 20; pattern; preset; stop; final state
safe), and the in-tree gated smoke (`cargo test --test
stdio_smoke`, 9 tests) is part of the 130/130 suite.

### server/discover (modern session open)
Request: `{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"inspector-style","version":"1.0.0"},"io.modelcontextprotocol/clientCapabilities":{}}}}`
Response: `{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"instructions":"ache \u2014 unified haptics: Lovense local Game Mode and an embedded Buttplug server behind one surface. Tools: lovense_status, lovense_toys, lovense_arm, lovense_disarm, lovense_function, lovense_pattern, lovense_preset, lovense_stop, ache_devices, ache_device. Output is off by default: arm explicitly, every output on either provider passes the one safety gate that clamps intensity to the configured ceiling, and stop always works.","ttlMs":0,"cacheScope":"private","_meta":{"io.modelcontextp`

### legacy initialize probe (Inspector CLI shape) — designed refusal
Request: `{"jsonrpc":"2.0","id":90,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"mcp-inspector-cli","version":"0"}}}`
Response: `{"error":{"code":-32022,"message":"legacy protocol era not supported by this server; use protocol 2026-07-28 (server/discover)"},"id":90,"jsonrpc":"2.0"}`

### tools/list
Response: 10 tools — lovense_status (readOnly=True, idempotent=True), lovense_toys (readOnly=True, idempotent=True), lovense_arm (readOnly=False, idempotent=True), lovense_disarm (readOnly=False, idempotent=True), lovense_function (readOnly=False, idempotent=False), lovense_pattern (readOnly=False, idempotent=False), lovense_preset (readOnly=False, idempotent=False), lovense_stop (readOnly=False, idempotent=True), ache_devices (readOnly=True, idempotent=True), ache_device (readOnly=True, idempotent=True). (Full schemas on the wire;
payload measured 1,760 cl100k_base tokens — see EFLO.md.)

### tools/call lovense_status
Response payload: `{
  "armed": false,
  "intensity_ceiling": 20,
  "last_event": null,
  "note": "mock transport: fixture toys only; no device or app is contacted",
  "output_active": false,
  "output_path": "gated: every output on every provider passes the one decide() gate; stop, disarm, the watchdog, a session drop, and shutdown each zero output on both providers and latch the master off",
  "providers": {
    "buttplug": {
      "available": true,
      "note": "fixture provider: fixture devices only; no device or app is contacted",
      "provider": "buttplug",
      "state": "mock"
    },
    "lovense": {
      "available": true,
      "note": "mock transport: fixture toys only; no device or app is cont ...[trimmed]`

### tools/call ache_devices (unified, both providers)
Response payload (trimmed): `{
  "count": 4,
  "devices": [
    {
      "battery": 87,
      "capabilities": [
        {
          "feature": "Vibrate",
          "max": 20,
          "native": "lovense:Vibrate",
          "steps": null
        }
      ],
      "capability_source": "reported",
      "id": "lovense:mock-toy-1",
      "kind": "lush",
      "name": "Mock Lush",
      "provider": "lovense"
    },
    {
      "battery": 42,
      "capabilities": [
        {
          "feature": "Vibrate",
          "max": 20,
          "native": "lovense:Vibrate",
          "steps": null
        }
      ],
      "capability_source": "reported",
      "id": "lovense:mock-toy-2",
      "kind": "edge",
      "name": "Mock Edge",
      "provider": "lovense"
    },
    {
      "battery": null,
      "capabilities": [
        {
          "feature": "Vibrate",
          "max": 20,
          "native": "Vibrate",
          "steps ...[trimmed]`

### tools/call ache_device device=buttplug:1
Response payload: `{
  "battery": 63,
  "capabilities": [
    {
      "feature": "Vibrate",
      "max": 20,
      "native": "Vibrate",
      "steps": 20
    },
    {
      "feature": "Rotate",
      "max": 20,
      "native": "Rotate",
      "steps": 20
    }
  ],
  "capability_source": "fixture",
  "id": "buttplug:1",
  "kind": "fixture",
  "name": "Fixture Duo",
  "provider": "buttplug"
}`

### tools/call ache_device device=buttplug:99 — typed absence
Response payload: `{
  "error": {
    "class": "target",
    "code": "device_not_found",
    "message": "no device \"buttplug:99\" is present"
  }
}`

### tools/call unknown tool — protocol refusal
Response: `{"error":{"code":-32602,"message":"unknown tool `ache_nope`"},"id":7,"jsonrpc":"2.0"}`

### malformed line — parse refusal, session survives
Request: `this is not json`
Response: `{"error":{"code":-32700,"message":"expected ident at line 1 column 2"},"id":null,"jsonrpc":"2.0"}`

## Client matrix

Two real clients, both scripted stdio JSON-RPC clients run
against the same built 1.0.0 binary in the same sitting.
Every request in both sessions carries its protocol version
in `_meta`; both open with `server/discover`.

| # | Client | Shape | Verdict |
|---|---|---|---|
| A | inspector-style (above) | discover, legacy probe, list, status, unified discovery + lookup, unknown tool, malformed line | PASS — all verdicts above |
| B | rose/phlow-shaped production client | discover, list, status, unified discovery, gated output flow against a Buttplug fixture device | PASS — detail below |

### Session B detail (rose/phlow-shaped production client)

The flow a consuming client (the rose.nvim / phlow client
shape: discover once, list once, then `tools/call` with
`_meta` on every frame) drives at production time, aimed at
a Buttplug fixture device through the unified surface:

- `lovense_function` on `buttplug:1` while disarmed → typed
  refusal `not_armed` (the gate binds before any provider
  routing).
- `lovense_arm` → armed.
- `lovense_function` Vibrate strength 25 on `buttplug:1` →
  ran with effective strength **20** (ceiling clamp observed
  at the wire, cross-provider).
- `lovense_preset` on `buttplug:1` → typed refusal
  `capability_unsupported` (presets are Lovense-only).
- `lovense_function` Rotate on `buttplug:0` → typed refusal
  `capability_unsupported` (the fixture offers Vibrate only).
- `lovense_stop` → synchronous zero; closing
  `lovense_status` reports `armed: false`,
  `output_active: false`.

## Failures and gaps

None in either session: every request was answered, no
request timed out, no refusal was unexpected. The one known
non-session item stands: the Inspector CLI's legacy-only
open (designed refusal above).
