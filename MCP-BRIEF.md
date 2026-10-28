# Lovense MCP Server — Brief

Commissioned by the Owner, 2026-10-21. Home: `NTRanch/Lovense`
(this brief's canonical copy ships in that repo as `MCP-BRIEF.md`;
build tracking lives in that repo's `CHECKLIST.md`, Phase 6).
Register entry: IDEA-0080 (SFW index in the SFW-lane register;
detailed tracking in the NTRanch lane per the Owner's tracking
lane rule, 2026-10-21).

## Why build — the landscape, verified 2026-10-21

Lovense **does** publish an official MCP server — a first-party
Python script (`RemoteMCP.py`, shipped as `RemoteMCP.zip`),
documented at developer.lovense.com/docs/ai/remote-mcp.html
(page last updated 2025-07-03). The first version of this brief
said otherwise; that survey checked registries and directories
and missed Lovense's own developer docs. Corrected 2026-10-21.

The official server was studied firsthand the same day (zip
SHA-256 `c1cb2a6f…`, four files, ~4.7 KB of Python). It is a
demo-grade thin bridge over this brief's own route — stdio,
FastMCP, the Lovense Remote app's Game Mode over the LAN, no
credentials — with exactly **two tools**:

- `SendFunctions(python_expression)` — the parameter is accepted
  and **ignored**; every call sends a hardcoded `Vibrate:10`
  for 2 seconds to all toys.
- `SendStopFunction()` — sends Stop.

Its docs page advertises "commands, patterns, and presets"; the
code implements neither patterns nor presets, has no discovery
tool, no per-toy targeting (the `toy` field is always empty), no
intensity control at all, no request timeout, and no safety
contract of any kind. Its wire shape does confirm ours:
`POST /command`, `Function` at apiVer 1, Stop as
`Function`/`Stop`/timeSec 0. One wire fact matters for S2/S4:
over HTTPS the app is addressed as
`https://{dashed-ip}.lovense.club:{ssl-port}`, not by raw IP —
the config in S2 and the S4 runbook must use that form.

Community work also exists:

- A PulseMCP-listed "Lovense" server (clotho2) driving the Game
  Mode API — community classification, no published safety
  contract.
- "Lovense Cloud MCP" (Glama listing) — a Cloudflare Workers
  bridge over the Lovense cloud route with a developer token.
  That route was already evaluated and rejected for STROBE on
  latency and token-custody grounds.
- Buttplug-ecosystem MCP servers (chiefautism's buttplug-mcp;
  the mcpstore buttplug-mcp, whose own listing calls the current
  experience "unstable and frustrating"). Different ecosystem:
  our bridge speaks the Lovense Standard API directly, by design.

None of them — official included — enforces our safety contract,
separates lanes, or carries a mock-transport test ladder. That
contract is this server's reason to exist: not first existence,
but the only decide()-gated, ceiling-clamped, watchdog-guarded
surface on this route. Under the estate's graduation rule this is
a server, not a skill: it holds live connection state and
enforces a decide()-class safety gate.

## What it is

A small Rust MCP server (stdio) that gives MCP clients — agents,
phlow, the editor stack — a controlled way to drive Lovense toys
through the **Standard API local Game Mode route**, for linking
into the studio's games and apps. It is the agent-side control
surface. It does not replace the in-game bridge: games keep
embedding the bridge, and both surfaces enforce the **same**
safety state machine, so behavior is identical whichever surface
drives.

Wire contract (already proven by the bridge and the probe CLI):
`POST {scheme}://{host}:{port}/command`, JSON body, header
`X-platform` required. Function apiVer 1; Pattern apiVer 2 (rule
`V:1;F:<features>;S:<ms>#`, at most 50 strengths, step interval
strictly above 100 ms); Preset apiVer 1. Default ports 20010
(HTTP) / 30010 (HTTPS) — **ports move; host and port are always
configuration, never constants treated as truth.**

## The safety contract (non-negotiable, mirrors the bridge)

- Master toggle defaults **off**. Output is possible only in the
  armed state, and arming is always an explicit tool call, never
  a side effect of connecting or discovering.
- Intensity ceiling defaults to 20 and is clamped **in code** on
  every outbound value (the probe already proved this shape:
  an armed request for 25 hit the wire as 20).
- Stop is synchronous, zeroes every feature, is callable in any
  state, and is never itself gated by the arm state.
- Reconnect never resumes output. A dropped session zeroes
  output, and the cleanup runs on an explicit shutdown path
  (the atelier S1 cancellation lesson: no skipped cleanup logs).
- Heartbeat watchdog (default 5 s): while output is active,
  silence past the window zeroes output.
- Every output call passes a pure `decide(state, request)` gate
  in the flowguard shape: allow (possibly clamped) or deny with
  a reason. A deny is final; there is no override parameter.

## Tools (v1: 8, well under the 25-tool budget)

- `lovense_status` — state machine state, transport, target,
  ceiling, watchdog, session health. Read-only.
- `lovense_toys` — discover connected toys (unarmed-safe).
- `lovense_arm` — explicit master-on. Refuses without a target.
- `lovense_disarm` — master-off; zeroes output synchronously.
- `lovense_function` — one function call (e.g. Vibrate) with
  strength and duration, clamped by decide().
- `lovense_pattern` — a Pattern rule, validated (strength count,
  interval floor, feature letters) before it can reach the wire.
- `lovense_preset` — a named preset from the Remote app.
- `lovense_stop` — panic zero. Idempotent, always available.

Every tool carries full schema `properties` and annotations per
the Tiger Style MCP rules; every result reports the effective
(clamped) values actually sent — no hidden behavior.

## Configuration

TOML, fail-closed (the atelier pattern): `transport` = `mock`
(default) or `real`; `host` default empty (no target, no output
possible); `port`; `use_https`; `intensity_ceiling`;
`watchdog_secs`. The local Game Mode route needs **no developer
token**; the server holds no credentials of any kind. If a future
phase ever touches a credentialed surface, credentials are
referenced by password-store path only.

## Testing

Half validation, half adversarial, mock transport first (the
repo's TESTING.md ladder). The mock records every outbound
command for assertion. Adversarial battery, minimum: strength
above ceiling; pattern with 51 strengths; step interval at or
below 100 ms; output attempted while disarmed; output attempted
with no target configured; malformed pattern rules; stop during
an active pattern; watchdog expiry mid-pattern; session drop
mid-pattern (output zeroed, cleanup logged); arm/disarm races.
Gates: cargo fmt clean, clippy `-D warnings` clean, cargo test
all passing, nix build, stdio smoke (initialize, tools/list,
one full mock session) before any stage is called done.

## Stages

- **S0** — this brief + Phase 6 checklist in the home repo.
- **S1** — read core: config, status, toy discovery over mock
  and real-unarmed transports. No output path exists yet.
- **S2** — output core: arm/disarm, function, pattern, preset,
  stop, the decide() gate, full adversarial battery green.
- **S3** — hardening: watchdog, session-drop path, shutdown
  hook, conformance pass (Inspector + rose + phlow clients).
- **S4** — live rung (separately gated): real hardware, lowest
  level, stop latency measured. Only on the Owner's explicit
  go, with the Owner present at the machine. This folds in the
  previously tabled live rung as this server's acceptance.

## Amendment — 2026-10-27, Owner-commissioned: the unification

The Owner commissioned merging this server and Buttplug.io support
into ONE unified haptics MCP server, renamed **ache**. This amendment
supersedes two things in this brief:

- **"Buttplug transport" (out of scope for v1) is superseded.** The
  unified server carries two providers behind one seam — Lovense
  (this brief's server, behavior unchanged) and Buttplug (embedded
  in-process; no Intiface instance) — behind one tool surface, with
  this brief's safety contract enforced unchanged across both.
- **Distribution naming.** The server is `ache` from v0.3.0; the
  GitHub repository rename (`NTRanch/Lovense` → `NTRanch/ache`) is
  staged as a landing decision, not yet executed.

Everything else in this brief stands — in particular the S4 live rung
remains separately gated on the Owner's go + presence, and now
covers both providers' hardware. The merge's build report lives with
the commissioning work (`lovense-merge-20261027/REPORT.md` in the
estate workspace).

## Out of scope for v1

- The cloud/server Standard API route (rejected posture stands).
- The App Gallery platform API (Phase 4 of the home checklist;
  a separate product decision).
- Buttplug transport (revisit only if a target toy lacks
  Standard API support). **[Superseded 2026-10-27 — see the
  amendment above.]**
- Any presence in any SFW-lane registry or on any SFW-lane site:
  this is an NTRanch-lane artifact, distributed from the
  NTRanch/Lovense GitHub repo.

## Standing rules carried in

Studio attribution only on every public surface; no personal
names in this repo or its artifacts. Credentials by
password-store path only — and v1 needs none. Never route this
build to Azoth (adult-content production is outside its scope
by its own ruling).
