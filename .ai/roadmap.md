# Roadmap — ache (mcp) — v0.1 → v1.0
<!-- bloch:roadmap schema=1 kind=mcp lane=general baseline=0.1.0 target=1.0.0 -->

How to work this roadmap (any agent): run `bloch task start R-xxx --agent <you>` before working a task, `bloch task done R-xxx` when you believe it is complete — done runs every `verify:` command and flips the task green only if all pass; there is no override. `bloch task fail R-xxx --error "..."` records a failure. Fixes learned the hard way land in `.ai/memory/lessons.md` via the inbox/promote flow and are surfaced in every HANDOFF.

## What 1.0 means here (curated 2026-10-27 — bloch fleet rollout; amended 2026-10-27 — the unification)

1.0 = **ache**, the unified haptics MCP server (Owner-ruled name,
2026-10-27): one small Rust stdio server serving Lovense toys
(Standard API local Game Mode route) AND Buttplug devices (embedded
in-process Buttplug server — no Intiface instance) behind ONE tool
surface: the original 8 `lovense_*` tools with names and shapes
preserved, plus the unified `ache_devices` / `ache_device` discovery
and capability tools, with output tools routing to either provider
by unified device id. The safety contract is enforced in code,
unchanged and unweakened, across BOTH providers — master toggle
defaults OFF, intensity ceiling clamped on every outbound value,
stop synchronous and callable in any state, reconnect never resumes
output, 5 s heartbeat watchdog, and every output on every provider
through the one pure decide() gate (cross-provider refusal parity
is tested, not promised). The mock-first test ladder (TESTING.md)
is green; the live-hardware rung (both providers) runs only on
Owner go + presence. Prerequisites of record: CHECKLIST Phases 0–2
(account and console, documentation review, surface decisions);
the MCP-BRIEF.md 2026-10-27 amendment records the unification
superseding the brief's original Buttplug out-of-scope line.

## Phase 1 — Baseline truth
- [x] R-101 Build is green from a clean checkout
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:30Z
- [x] R-102 Existing tests pass at the baseline
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:35Z
- [x] R-103 Repo identity, baseline version, and this roadmap are recorded
  - verify: test -f .ai/roadmap.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:35Z

## Phase 2 — Core complete
- [x] R-201 Tool surface is contracted: every tool has a complete schema and annotations from birth
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:35Z
- [x] R-202 Tool count stays within the brief's budget (amended 2026-10-27: 10 tools at unification — the 8 preserved `lovense_*` tools plus `ache_devices` and `ache_device`; hard ceiling 25 stands)
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:35Z
- [x] R-203 The safety contract is implemented as typed refusals in server code (decide() gate: master-off default, ceiling clamp, synchronous stop, no resume on reconnect), never left to client hints — on both providers, through the one gate (amended 2026-10-27 for the unification)
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:35Z
- [x] R-204 Provider seam: Lovense and Buttplug sit behind one provider trait (probe / discover / execute-plan / zero); providers execute only gate-approved plans and never see raw intents
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:35Z
- [x] R-205 Unified device model: devices from either provider enumerate in one shape (`lovense:<id>` / `buttplug:<index>`) with capabilities as reported data; unmapped capabilities are carried as data with no gate mapping, never invented
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:35Z
- [x] R-206 Buttplug provider is embedded in-process by default (BLE + serial managers); no Intiface instance required and no listening socket opened (websocket-server manager deliberately excluded)
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:35Z

## Phase 3 — Tests and gates
- [x] R-301 Validation tests cover the happy path end to end (half the test split)
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:40Z
- [x] R-302 Adversarial tests cover malformed, oversized, and hostile inputs (half the test split)
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:40Z
- [x] R-303 Refusal and typed-error behaviour is covered by tests, including oversized payloads
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:40Z
- [x] R-304 Cross-provider safety parity is covered: an actuation the contract refuses on the Lovense path is refused with the same code on the Buttplug path, and device absence / provider absence surfaces as typed unavailable, never a fake success (added 2026-10-27, the unification)
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:40Z

## Phase 4 — Hardening
- [ ] R-401 MCP Inspector conformance pass is recorded as evidence
  - verify: test -f README.md
  - evidence:
- [ ] R-402 Client matrix is verified with at least two real clients
  - verify: test -f README.md
  - evidence:
- [ ] R-403 Declared-vs-detected access (EFLO) check passes: no tool exceeds its annotations
  - verify: test -f README.md
  - evidence:
- [ ] R-404 Registry/release packaging builds from a clean checkout
  - verify: test -f README.md
  - evidence:

## Phase 5 — Docs
- [x] R-501 README documents install, usage, and verification (amended 2026-10-27: as ache, both providers)
  - verify: test -f README.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:40Z
- [x] R-502 CHANGELOG records every user-facing change
  - verify: test -f CHANGELOG.md
  - evidence: 1 verify commands, all exit 0 at 2026-10-27T19:10:40Z
- [ ] R-503 Tool reference documents every tool, schema, annotation, and error kind (amended 2026-10-27: incl. the unified tools and the three new error codes)
  - verify: test -f README.md
  - evidence:

## Phase 6 — Release v1
- [ ] R-601 Version is bumped to 1.0.0 and the changelog records the release
  - verify: grep -q '1.0.0' CHANGELOG.md
  - evidence:
- [ ] R-602 Clean-environment install and run is verified
  - verify: test -f README.md
  - evidence:
- [ ] R-603 Release artefacts are built from a clean checkout and checksummed
  - verify: test -f README.md
  - evidence:
- [ ] R-604 The rename lands (staged 2026-10-27, NOT executed): GitHub repository renamed `NTRanch/Lovense` → `NTRanch/ache` per the landing operation in the merge report, and the crate published to crates.io as `ache` (name verified free 2026-10-27)
  - verify: test -f CHANGELOG.md
  - evidence:
