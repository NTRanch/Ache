---
origin_lane: ntranch
content_label: nsfw
evidence: dispatch STEP2-BRIEF.md, FULL-CATALOG-AUDIT.md, COVERAGE-MATRIX.md; evidence/ run outputs
---
# Step 2 report — ready for Pax's gate

## Base and discipline

Base commit: `c4b9612edfac2b7bc4180b5ed6d9eea14867358a`.
Branch: `step2-vocab`.
Worktree: `/home/phaedrus/.local/share/pax/ache-step2-wt-20261113`.
All three dispatch inputs read in full before implementation. Report skeleton created before edits, within ten minutes. Source checkout remained clean. No commits or pushes. No delegated agents or custom skills materially used.

Cargo ran under `flock -w 7200 /home/phaedrus/.local/share/pax/night-locks/build.lock`, sourcing `$HOME/.cargo/env`, with `CARGO_HOME=/home/phaedrus/.cargo`, empty `RUSTC_WRAPPER`, and `nice -n 10`. The bacon producer used that same lock for each Cargo job and was stopped after diagnostics; no session-owned producer remains. No surface outside the NTRanch lane was written.

## Implementation deltas

- `mcp/src/decide.rs`: Depth 0–3 and Stroke 0–100 already existed at base. Added validation/adversarial coverage and bounded reported maxima by documented API maxima. Existing 0–20 server ceiling still limits Stroke to at most 20. Heating and Tapping return typed `capability_unsupported` messages naming the action. RollingBead is a named planned capability, denied unless the provider publishes the evidenced binding; Lovense never publishes it.
- `mcp/src/provider.rs`: explicit bindings with pinned 12.0.1 Buttplug config provenance: Flexer Fingering→Rotate; Gravity/Solace/Spinel Thrusting→Oscillate; Max Pump→Constrict; Velvo RollingBead→Rotate. Each alias requires BOTH the reported config identity and enumerated native output, never an inferred per-toy support set. User-editable display names cannot enable aliases. `ache_device` data now includes `action_bindings` and `route_reductions` with evidence and Bloch labels. Missing GetToys functions now mean `unreported`/empty capabilities, rather than assuming every function.
- `mcp/src/provider_buttplug.rs`: discovery and actual dispatch consume the same bindings. Pump Function levels normalize over 3; Pattern intensity retains its pre-existing 0–20 contract. Empty-feature patterns skip duplicate aliases. Position remains unmapped; no unverified Depth/Stroke BLE route is invented.
- `DOCS-NOTES.md`: Glint/Mission 2 touch-sense and Calor depth-sensor Toy Events candidates, with matrix [S]/[A] citations. Touch/depth event emission remains UNVERIFIED. No event consumption.

Reduction records name Osci/Osci 2 oscillation, Vulse thrusting, and Tenera 2 suction gaps. A generic `Lovense Device` cannot honestly be identified as Tenera 2: its data explicitly reports the generic non-vibration/suction limitation and UNVERIFIED model identity. Osci family identity does not verify generation. Reduction metadata never grants support and named reduction records disappear if the missing action is actually enumerated.

The served tool names, argument shapes, and existing enum remain unchanged. RollingBead is an explicit provider capability/planner alias; the existing Rotate action remains available on the BLE route. The schema enum does not advertise this additional planner alias. Heating/Tapping refusals are available to requests naming those actions, rather than being silently accepted.

## Pass/fail checks and run evidence

| Gate | Result | Run evidence |
|---|---|---|
| Untouched baseline | PASS: 130 unit + 9 stdio = **139**, zero failures | evidence/baseline.log:677; evidence/baseline.log:692 |
| Final suite | PASS: 146 unit + 9 stdio = **155**, zero failures | evidence/final-gates-rerun.log:382; evidence/final-gates-rerun.log:397 |
| New test balance | **16: 8 validation / 8 adversarial** | decide::step2_tests (2/2), provider::step2_tests (5/5), provider_buttplug::step2_tests (1/1), all in final run |
| Mapped-action clamping parity | PASS: 25 equal-plan cases across named mappings, both mock providers execute allowed plans | evidence/final-gates-rerun.log:303 |
| Named-refusal parity | PASS: Heating/Tapping/RollingBead unsupported on both ordinary mock routes, same code, named messages, zero sends | evidence/final-gates-rerun.log:293 |
| Lovense wire invariant | PASS; original apiVer architectural test and wire-equality test unmodified | evidence/final-gates-rerun.log:338; evidence/final-gates-rerun.log:270 |
| Clippy | PASS: `cargo clippy --all-targets -- -D warnings`, exit 0 | evidence/final-gates-rerun.log:626 |
| Formatting | PASS: `cargo fmt -- --check`, exit 0, no output | same successful chained run, evidence/gate-summary.json |
| Diff whitespace | PASS: `git diff --check`, exit 0 | final review; no whitespace diagnostics |
| Theodolite Rust | planted control **1 severity-1 error**, removal followed by **0 diagnostics for each of 3 touched Rust files** | evidence/theodolite.jsonl records 3–9; producer evidence/bacon.log |

The requested 118-test/v1.0.0 reference differs from the supplied HEAD: its untouched suite has 139 tests and Cargo package version 0.4.0. No tests were removed or weakened to reconcile that discrepancy. `server.rs` and `service.rs` are unchanged; all original tests pass.

## Theodolite evidence strength

Shared config: `/home/phaedrus/.local/share/theodolite/bridge.toml`; config hash `c798b2018ea1e1509bbbdc612b88c66a8b7cd440d68a394403a59148e8e984d9`. Bridge 1.0.0, bacon-ls 0.23.0. Final run used a live producer BEFORE requesting diagnostics, planted `compile_error!("ACHE_STEP2_POSITIVE_CONTROL")` in decide.rs, observed it through the bridge, restored the exact original bytes, waited for a successful producer rebuild/empty export, and obtained zero counts for decide.rs/provider.rs/provider_buttplug.rs in the SAME session. Source hashes after restoration are in evidence/gate-summary.json.

Qualification: bridge results carry `possibly_incomplete: server_not_quiescent`; bacon-ls reports zero progress cycles. This is **observed zero with a proven live producer and same-session positive control**, not a claim of LSP quiescence. Both initial and final bridge transcripts preserve that qualification. No TOML files changed. Markdown, logs, JSON and JSONL have no applicable shared route and are **not-LSP-validated**.

## Failures encountered and resolved

- First bacon configuration failed to export the planted diagnostic. Replaced its job configuration with explicit `analyzer = "cargo_json"`, `need_stdout = true`, and cargo-json-spans export. Positive control then passed, twice, including final source bytes.
- Intermediate schema experiment added RollingBead to the served enum: 145 unit tests passed, 1 existing exact-schema test failed (`evidence/final-gates.log`). Reverted that enum change. Final unmodified schema test and all other tests pass. The final schema remains the shipped schema, as requested.

No unresolved implementation blocker. The bridge quiescence caveat remains explicit for Pax's gate.

## Scope limits and review notes

No score/ensemble layer, Server API provider, Toy Events consumption, hardware rung, commits, or pushes. Per-toy API support and unverified matrix identities remain runtime-reported/UNVERIFIED. No live hardware coverage claimed. Existing transport.rs documentation still describes the old missing-functions fallback; flagged as pre-existing adjacent documentation debt rather than changing another source file. Existing generic native output mappings remain; the new aliases are qualified and stated data.

Changed tracked files: 3 Rust + 1 Markdown. Created: this report and evidence/ (raw logs, bridge transcripts, gate summary). The failed intermediate run is retained for review. All new records inherit or explicitly carry origin_lane=ntranch, content_label=nsfw and evidence.
