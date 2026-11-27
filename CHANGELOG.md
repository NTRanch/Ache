# Changelog

All notable changes to this repository's MCP server. Versions are the
crate version in `mcp/Cargo.toml`.

## 1.4.0 — 2026-11-27 — the KoiSync and XToys landings

Built on the 1.2.0 lineage (base c1e5a1f) and landed by merge with
the 1.3.0 GHR programme lineage: the 1.4.0 on main contains both
the landings below and everything in the 1.3.0 section that follows.

- KoiSync landing: a TCode multi-axis provider (SR6 / OSR2 / SSR1 over
  USB serial or UDP; opt-in, `[tcode] enabled = false` by default) with
  six first-class axes, per-axis travel windows and a per-frame slew
  bound in the gate (`decide_axes`; defaults [10, 90] and 4 per 20 ms,
  from KoiSync's global limits), 50 Hz emission, `DSTOP` on every zero
  path. One funscript adapter for single-axis files and the six-axis
  pack (`ache.pack/v1`); the depth solver as a pure `ache.depth/v1`
  transform with per-character calibration. Spec, every claim cited:
  `docs/SPEC-KOISYNC.md`.
- XToys landing: `ache.pattern/v1` (Square/Linear/Sine blocks; a
  bounded, total, seedless expression language for scripted patterns),
  `ache.automation/v1` (closed trigger/action sets, jobs with required
  `max_runs` + `max_span_ms`, frozen queues, typed clamped
  session-scoped variables, a mandatory stop button; seeded `hash01`
  only), `ache.session/v1` (one source per device function, packs
  all-or-none, load-time duration bounds). No JavaScript, no webhooks,
  no runtime-mutable programs.
- Four new tools (16 total): `ache_program_load`, `ache_program_run`,
  `ache_program_stop`, `ache_program_event`. The original twelve keep
  their behaviour. Every program command passes the decide() gate at
  emission time; stop inputs and the stop button are the synchronous
  stop; the heartbeat watchdog governs programs as it governs scores.
- Seed library (`mcp/library/seeds.json`): estate-authored specs for
  the XToys study's top-10 ADAPT shapes; sensor inputs stand in as
  pre-registered named events.
- Spec extensions, recorded: S-1 Linear `falling` (descending ramps);
  S-2 `set_function` takes a bounded expression, not only a literal.
- Gates: 272 tests (261 unit + 2 manifest + 9 stdio smoke), fmt and
  clippy `-D warnings` clean, three consecutive green runs, replay
  hashes byte-identical across runs for 14 fixtures. Evidence in
  `evidence/landings-1.4.0/`. No live-device action was performed.

## 1.3.0 — 2026-11-26 — GHR programme: devices, rumble, Fredorch

- ache.rumble/v1 event-source input: rumble events as a gated input source (GHR Phase 3).
- url bumped to 2.5.9 (RUSTSEC-2026-0592).
- Gates at landing: 250 tests passing (see the release record).

- Four per-model bindings, each documented in the pinned Buttplug
  config: JoyHub RoseBUD and RoseLin Suction → constrict, and the
  Eropair Thrusting Vibrating Dildo Thrusting → oscillate.
- Fredorch Device, a position-only thrusting machine, now takes
  Thrusting as a bounded stroke program. Speed 1–20 maps one-to-one
  to Fredorch's own speed table. Raw Position stays refused by name,
  and so does every other position device, strokers included.
- Temperature, Led and Spray remain reported-but-refused.
  README "Toy coverage" now lists the BLE rows and the refusal for
  each class.
- Gates at that step: 221 tests passing (210 unit + 2 manifest + 9 stdio smoke).

## 1.1.0 — 2026-11-14 — score layer: multi-toy symphony

- Step 3, the score layer: multi-toy symphony is built into the
  server itself, not demo scripting. One new tool,
  `ache_score_start`, starts a score; each per-device part passes
  through decide() on its own, parts dispatch concurrently, and
  one ensemble stop latches every device.
- Step 2, the full catalog: the vocabulary now covers all
  compatible Lovense toys for women with all features, including
  Bluetooth-only toys, behind the same decide() gate.
- Safety posture unchanged: the master switch stays off by
  default, decide() behaviour is unchanged, and stop stays
  available in every state.
- Gates: 175 tests passing (166 unit + 9 stdio smoke). Evidence
  in `evidence/` and `step3-evidence/`; step reports at the
  repository root.

## 0.4.0 — 2026-10-27 — rmcp migration, modern era only

The protocol layer moves from the hand-rolled stdio loop to the
official Rust SDK (rmcp 3.8.0), serving the modern era
(2026-07-28) only, per the fleet standardization (D-113).

- `server/discover` replaces the `initialize` handshake; a legacy
  `initialize` is refused with the typed era error -32022 by the
  byte-level era guard in front of the SDK, which also restores
  the baseline framing contract (malformed lines answered, 4 MiB
  line bound) and serializes dispatch in wire order.
- Tool names, schemas, result payloads, refusal codes, and refusal
  messages are unchanged (canonicalized-JSON parity against the
  0.3.0 baseline; see the WS2 gate report).
- `decide()` / `decide_plan()` are byte-identical to 0.3.0 — the
  migration's diff over `src/decide.rs` is empty.
- U5 is unchanged: stop is available in every state and never
  gated.

## 0.3.0 — 2026-10-27 — ache: the unified haptics server

The Lovense MCP server and Buttplug.io support are one server now,
renamed **ache** (crate, binary, and server name; Owner-ruled name).

- Two providers behind one seam: Lovense (Standard API local Game
  Mode, carried over) and Buttplug (embedded in-process server via
  the Rust `buttplug` crates; BLE + serial managers; no Intiface
  instance needed, no listening socket opened).
- One tool surface: the original eight `lovense_*` tools keep their
  names and shapes; two unified tools are added — `ache_devices`
  (discovery across both providers, capabilities as data) and
  `ache_device` (one device's capabilities). The output tools accept
  a unified `device` id and route to either provider.
- The safety contract is unchanged and now binds both providers:
  the same pure `decide()` gate plans every output (master-off
  default, ceiling clamp, bounded durations, synchronous stop,
  watchdog, no resume), with cross-provider refusal-parity tests.
- New typed errors: `provider_unavailable`, `capability_unsupported`,
  `device_not_found`.
- Breaking: the config environment variable is now `ACHE_MCP_CONFIG`
  (the old name is no longer read); the Lovense `X-platform` header
  value is now `ache`; the server reports name `ache`, version
  `0.3.0`. A `[buttplug]` config section (`enabled`, `scan_ms`) is
  added; existing config keys are unchanged.
- Hardware verification remains the separately gated live rung: this
  release is verified against the mock/fixture providers only.

## 0.2.0 — stage S2

Read core plus the `decide()`-gated output core (function, pattern,
preset, stop) over the Lovense Standard API local Game Mode route,
as `lovense-mcp`.

## 0.1.0 — stage S1

Read core: configuration, status, and toy discovery, as `lovense-mcp`.
