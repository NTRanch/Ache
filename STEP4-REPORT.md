---
origin_lane: ntranch
content_label: nsfw
evidence: D-1451 (Owner ruling); DOCS-NOTES.md; COVERAGE-MATRIX.md; STEP3-REPORT.md; cargo/clippy/fmt/theodolite gate runs on primo, 2026-11-14
---

# Step 4 report — ready for Pax's gate

## Base and discipline

Worktree: `/home/phaedrus/.local/share/pax/ache-step4-wt-20261114`.
Branch: `step4-server-api`, created from the 1.1.0 release tip
`24f2269` on `step3-score`. The release worktree
(`ache-step2-wt-20261113`) was never written. No pushes, no
publishes, no version bump: `Cargo.toml` still reports 1.1.0. Local
commits on this branch only.

All Cargo jobs used `flock -w 7200
/home/phaedrus/.local/share/pax/night-locks/build.lock`, sourced
`$HOME/.cargo/env`, set `CARGO_HOME=/home/phaedrus/.cargo` and empty
`RUSTC_WRAPPER`, and invoked Cargo with `nice -n 10` from `mcp/`.

One correction to the step-4 brief, flagged per its own terms: the
brief assumed the codebase already had a secret-handling pattern to
reuse. It did not — no code read `pass` or any token, and the config
tests actively refused credential-shaped keys. A new minimal pattern
was designed instead (below); nothing about the gate contract
changed to accommodate it.

## Contract as built (D-1451)

A third provider, `ProviderId::ServerApi` (`serverapi:<toy id>`),
behind the same `decide()` gate. The provider changes the
transport, never the authority:

- **Opt-in, off by default.** `[server_api] enabled = false` is the
  default; disabled wires an inert provider and a refusing pairing
  surface, and the local routes remain the whole server.
- **Config surface.** Flat fields mirroring the Buttplug knobs:
  `server_api_enabled` (default `false`),
  `server_api_token_pass_entry` (default `lovense/ntr/token`),
  `server_api_uid_pass_entry` (default `lovense/ntr/uid`). The
  `[server_api]` section denies unknown fields, so a literal
  `token = "..."` key refuses exactly like every other
  credential-shaped config key. Entry paths are validated (relative,
  ≤128 bytes, `[A-Za-z0-9/_.-]`, no empty or `..` components).
- **Custody.** Config names password-store paths only — never a
  value. `PassSecretSource` loads an entry by running `pass show`
  with stdin and stderr nulled, a 5 s poll deadline, and a kill on
  timeout: no retry, nothing persisted, nothing logged. A failed
  load is the new typed error `secret_unavailable` (class
  `configuration`); its message names the entry path (configuration,
  not a secret), never a value. Loaded credentials are cached in
  memory only; failed loads are not cached; the `Credentials` type
  has no `Debug`. `zero()` never fails on missing custody — with
  nothing loaded it reports `Ok(false)`, so the panic path cannot be
  held hostage by a locked store.
- **Wire.** Gate bodies come from `decide::lovense_wire`, exactly as
  the local Lovense provider sends them. The pure merge
  `server_api_wire(body, token, uid)` adds only the `token` and
  `uid` fields and touches nothing the gate decided (a test pins a
  clamped strength reaching the wire unaltered). The real transport
  (`RealServerApiTransport`, ureq, no redirects, 10 s timeout,
  bounded response) posts to `https://api.lovense-api.com` —
  `/api/lan/v2/command` for commands, `/api/lan/getQrCode` for the
  pairing QR — the shapes recorded in the estate's distilled
  Lovense API notes and consistent with DOCS-NOTES.md's account of
  the server route; replies are parsed by the same
  `parse_command_ack` / `parse_get_toys` the local transport uses,
  so a refusal code is surfaced, never swallowed, and a malformed
  reply is refused, never acted on.
- **Pairing.** One new tool, `ache_pairing` (12 tools total):
  `action: "status"` reports presence only (token present, uid
  present, paired, QR issued — never a value); `action:
  "request_qr"` loads the token alone and asks Lovense for a QR
  code for the Owner to scan. It issues no device command and pairs
  nothing. There is deliberately no "complete pairing" action:
  pairing completes inside Lovense Remote, and the resulting user
  id is placed in the password store by the Owner as the final
  explicit act. ache runs no callback listener. The provider's
  config enablement is the approval boundary — a disabled
  provider's pairing surface refuses both actions with
  `provider_unavailable`.
- **Unified routing.** The third provider is wired through every
  shared path: `ache_devices` enumeration, `resolve_device`,
  `retarget` (strips `serverapi:` to the raw toy id, like Lovense),
  `run_output`, the score layer (selector, provider pick, and both
  failure-path zero fan-outs), `target_configured`, status
  (`providers.serverapi`), and both stop paths
  (`send_stop_strict`, `send_stop_best_effort`) — all providers are
  always zero-attempted, never short-circuited. Presets are offered
  on Server API devices: the cloud route carries the Preset
  vocabulary (COVERAGE-MATRIX [L]).
- **Wiring.** `main::server_api_provider` mirrors
  `buttplug_provider`: disabled → inert + `NoPairing`; mock → the
  `FakeServerApi` fixture; real → `ServerApiProvider` behind an
  `Arc` whose delegation impls feed both seams from one instance,
  so the credential cache and QR-issued flag stay single-instance
  state. Lovense Live Control and Control Link have no third-party
  API and are out of scope, as ruled.

## Deltas per file

- `mcp/src/provider_server_api.rs` (new, ~1,160 lines): the
  `SecretSource` seam + `PassSecretSource`; the
  `ServerApiTransport` seam + `RealServerApiTransport`;
  `server_api_wire`; `parse_qr_code`; `ServerApiProvider`;
  `PairingProvider` + `NoPairing`; `Arc` delegation impls;
  `FakeServerApi`; 13 tests.
- `mcp/src/config.rs`: `[server_api]` section, flat fields,
  pass-entry validation, 3 tests.
- `mcp/src/error.rs`: `SecretUnavailable` code
  (`secret_unavailable`, class `configuration`).
- `mcp/src/provider.rs`: `ProviderId::ServerApi`, preset support
  for the Server API route in `Device::caps()`.
- `mcp/src/server.rs`: third provider through every routing/stop/
  score path, the `ache_pairing` tool + handler, status providers
  map, surface pins updated 11 → 12, 7 tests.
- `mcp/src/service.rs`: handshake instructions name the provider
  and the new tool; surface count 12.
- `mcp/src/main.rs`: module + `server_api_provider` wiring;
  startup line reports server api state.
- `mcp/tests/stdio_smoke.rs`: expected tools/list gains
  `ache_pairing`.
- `mcp/README.md`, `README.md`: provider, contract, tool, config,
  and custody documentation; the root README's STROBE rejection
  note now records the D-1451 amendment for ache itself.

## Gates (primo, this branch, final tree)

- `cargo test`: **198 passed, 0 failed** (189 unit + 9 stdio);
  baseline 175 → **+23 new tests**, split 13 validation / 10
  adversarial across config (1/2), the new module (8/5), and the
  server battery (4/3). Named cases: disabled-by-default refuses
  cleanly in status, enumeration, and pairing while other providers
  keep serving; disarmed output to a cloud device sends nothing;
  a strength-99 request reaches the fixture wire as the gate's
  clamped 20 with credentials merged; secret-unavailable degrades
  discovery, execution, and pairing to the typed error while the
  probe still answers and `zero()` still succeeds; a send failure
  mid-output drops the session, latches off, and still drives the
  stop onto the wire; malformed QR answers are refused; unknown
  pairing actions and smuggled fields are refused.
- `cargo clippy --all-targets -- -D warnings`: **0 errors**.
- `cargo fmt --check`: **clean**.
- Theodolite (bacon-ls route, producer-first `.bacon-locations`):
  **0 diagnostics** on every touched `.rs` file (config, error,
  main, provider, provider_server_api, server, service,
  tests/stdio_smoke). Positive control in the same session shape: a
  planted type error in `server.rs` read back as 2 diagnostics
  naming the planted function; the file was restored byte-identical
  (sha256 verified) and the final reading returned to 0.
  `.bacon-locations` was removed afterwards and is not committed.

## Mocked vs live, and what remains

Everything above is fixture/mock and loopback-stub evidence. No
live token was read, no cloud endpoint was contacted, no pairing
was performed. Remaining, each separately gated:

1. **Live-token smoke** — enable `[server_api]` against the real
   store entries (`lovense/ntr/token`, `lovense/ntr/uid` presence
   verified 2026-11-14, values never read) and one `GetToys`
   round-trip through the cloud.
2. **The Owner's QR pairing act** — `ache_pairing` `request_qr`,
   scanned in Lovense Remote by the Owner, who then places the
   resulting uid in the store. Built, never performed here.
3. Pax's gate on this report, then any landing/publish decision
   (a future release cut would ride the fleet programme; nothing
   here bumps the version).
