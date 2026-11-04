# Compliance — ache 1.0.0

The compliance record for ache, written current against the
landed 10-tool surface at 1.0.0 (checked 2026-10-28, the
1.0.0 tree). Every statement below is traceable to the tree
(source, `README.md`, `CONFORMANCE.md`, `EFLO.md`) or to the
gate record it landed with.

## Identity and protocol posture

- Crate `ache`, version 1.0.0, licence Apache-2.0. Studio
  attribution: Nice Thoughts Ranch (NTRanch).
- Built on rmcp `=3.8.0` (the fleet pin, D-113), modern MCP
  era only (2026-07-28). Sessions open with
  `server/discover`; every request carries its protocol
  version in `_meta`.
- Legacy contact is refused in code, not by client hints:
  the stdio era guard (`src/guard.rs`) answers any
  `initialize` with the typed `-32022` unsupported-era
  error, and the process stays up. The guard also owns the
  framing contract: malformed JSON → `-32700`, non-object
  request → `-32600`, unknown method → `-32601`, unknown
  tool → `-32602`, 4 MiB line bound, notifications dropped
  and never executed.
- The handler pins `supported_protocol_versions` to
  2026-07-28, so `server/discover` advertises exactly one
  version.

## Tool surface (10 tools)

- Read: `lovense_status`, `lovense_toys`, `ache_devices`,
  `ache_device`.
- Session: `lovense_arm`, `lovense_disarm`, `lovense_stop`.
- Output (all through the one `decide()` gate):
  `lovense_function`, `lovense_pattern`, `lovense_preset`.

Every tool shipped with a complete schema and behaviour
annotations from birth (readOnly / destructive /
idempotent hints). Declared access matches detected access
on all 10 tools (EFLO record). `tools/list` measures 1,760
tokens (cl100k_base) against the fleet 2,500 cap.

## Safety posture — decide() in server code

- **Master off by default.** Output is possible only in the
  armed state; arming is always the explicit `lovense_arm`
  call — never a side effect of connecting, discovering, or
  a prior session.
- **One pure gate.** Every output on either provider passes
  through `decide()` / `decide_plan()` exactly once; the
  functions are byte-identical to the 0.3.0 baseline (the
  rmcp migration's diff over `src/decide.rs` is empty).
  Providers execute only gate-approved plans and never see
  raw intents. A deny is final; no tool schema carries an
  override parameter.
- **Ceiling clamped in code** (default 20, config
  `intensity_ceiling` 0..=20), on a function strength and on
  each value inside a pattern; a target's own narrower
  range clamps further (e.g. a Buttplug device's reported
  range). Results report the effective values sent.
- **Synchronous stop.** `lovense_stop` zeroes every device
  on both providers before it answers and latches the
  master off. It is callable in any state and is never
  gated (U5).
- **No resume.** A dropped session, a watchdog trip (5 s
  default heartbeat window), and the shutdown path all zero
  output and latch off; recovery is a fresh explicit arm.
- **Bounded durations** 1..=3600 s; the Lovense API's
  indefinite `timeSec: 0` can never be emitted on either
  provider.
- **Cross-provider parity is tested**: an actuation the
  contract refuses on the Lovense path is refused with the
  same code on the Buttplug path; device/provider absence
  surfaces as typed unavailable, never a fake success
  (roadmap R-304; observed live at the wire in
  CONFORMANCE.md session B).
- **No credentials.** The local Lovense route needs no
  token and the server holds none; the config loader
  refuses credential-shaped keys. The embedded Buttplug
  stack opens no listening socket (the websocket-server
  manager is deliberately excluded).

## Verification state at 1.0.0

- Test suite 130/130 (121 unit + 9 stdio-smoke) on primo and
  on the sandbox copy; `cargo fmt --check` clean; clippy
  `-D warnings` clean (clippy 0.1.100, f7575a9da8
  2026-09-24, nightly-2026-09-25 pin).
- Conformance pass + two-client matrix recorded in
  `CONFORMANCE.md`; EFLO record in `EFLO.md`; release
  artefacts checksummed in `ARTEFACTS.md`.
- Hardware verification remains the separately gated live
  rung: 1.0.0 is verified against the mock/fixture providers
  only, and no live device was touched for this release.
