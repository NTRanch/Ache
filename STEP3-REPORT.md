---
origin_lane: ntranch
content_label: nsfw
evidence: STEP3-BRIEF.md; FULL-CATALOG-AUDIT.md; COVERAGE-MATRIX.md; STEP2-REPORT.md; step3-evidence/ run outputs
---
# Step 3 report — ready for Pax's gate

## Base and discipline

Worktree: `/home/phaedrus/.local/share/pax/ache-step2-wt-20261113`.
Branch: `step3-score`, created with `git switch -c step3-score`, carrying the uncommitted Step 2 state. HEAD remains `c4b9612edfac2b7bc4180b5ed6d9eea14867358a` (step3-evidence/review.log:7).

Read FULL-CATALOG-AUDIT.md, COVERAGE-MATRIX.md, and the worktree-root STEP2-REPORT.md in full, in that order, before edits; then read STEP3-BRIEF.md. The Bloch-labelled report skeleton was the first repository file edited, after the untouched baseline passed. No delegated agents or custom skills used. No commits or pushes. The source checkout `/home/phaedrus/.GH/NTRanch/ache` was untouched; before/after porcelain output is empty (step3-evidence/review.log:5; source-before.txt and source-after.txt). No surface outside the NTRanch lane was written. This report's classification applies to its evidence directory as well.

All Cargo jobs, including bacon's producer, used `flock -w 7200 /home/phaedrus/.local/share/pax/night-locks/build.lock`, sourced `$HOME/.cargo/env`, set `CARGO_HOME=/home/phaedrus/.cargo` and empty `RUSTC_WRAPPER`, and invoked Cargo with `nice -n 10`. Working directory was `mcp/`, where rust-toolchain.toml lives. An initial baseline invocation from the worktree root failed to select a toolchain; it made no implementation edits. The successful baseline evidence is preserved in step3-evidence/baseline.log.

## Planning contract as built

`decide_score(snapshot, parts, targets)` in decide.rs is pure: no I/O, clocks, discovery, or dispatch. Targets carry the provider-reported capabilities and provider-native selectors as explicit inputs. Each segment is a Function intent planned through the existing `decide_plan`; the Step 2 mappings therefore remain capability data, and absent/reduced capabilities produce named refusals rather than invented support. Every segment must pass the master-arm, target, action, nonnegative-level, and bounded-duration checks. Each effective level is clamped to the server ceiling and the device/action maximum. All parts are inspected, and any failure returns one whole-score refusal naming the refused device parts and their reasons. No partial plan escapes.

Input shape: `parts: [{device, segments: [{action, level, duration}]}]`. Durations are integer seconds. Segments run sequentially within a part; parts begin concurrently. The implementation bounds a score to 1–32 parts, each with 1–128 segments, and bounds each segment and each part's total duration to 1–3600 seconds. Duplicate device streams are refused so two parts cannot race over one device. Bounds/duplicate refusals are covered by the passing adversarial tests (final-gates.log:310–311). There is no new action vocabulary.

`ache_score_start` returns the allowed per-part effective plans and clamping flags, or the gate's operational error containing the named whole-score refusal. Unknown argument fields and malformed typed parts are protocol errors. Exactly one new tool is appended; stop remains `lovense_stop`. `lovense_status` adds `score_running`. Starting a score requires idle output, and other output tools refuse while a score is running; callers explicitly stop before replacing an active performance. Existing single-device behavior when no score is running remains covered by the original suite.

Each part has an independently waiting worker. A shared output mutex covers sends and all stop paths, never segment-duration waits. Thus commands interleave across devices instead of waiting for another device's timeline to finish. Transitions explicitly zero the previous segment's action; completion zeroes the part's final action. When every part completes, output becomes inactive and the master latches off. No worker-generated segment counts as a heartbeat.

Stop/disarm cancel the score and wake every waiting worker before the existing synchronous zero of both providers. Every worker checks cancellation under the same output mutex before sending; a stop therefore returns only after both provider zeros have been attempted, with no later score segment able to follow, even after re-arm. The existing watchdog and shutdown use that cancellation path too. A worker send failure cancels the ensemble, latches the session degraded/off, and attempts both provider zeros. Reconnection resumes nothing.

Atomicity is a planning guarantee, not a hardware transaction: after an allowed plan, a runtime transport failure may occur after another device has started. That failure aborts the ensemble and attempts zeros on both providers; disconnected hardware cannot acknowledge a zero. Wire sends remain short serialized operations through the existing providers, while timelines run concurrently; no exact simultaneous physical onset is claimed.

## Deltas per file

- `mcp/src/decide.rs`: score input/plan types and pure atomic planner. Existing single-command planning and Lovense rendering unchanged.
- `mcp/src/server.rs`: one appended tool/schema, score parser, concurrent part workers, shared send/cancel synchronization, existing stop/watchdog/shutdown integration, honest status field, and the score test battery. Existing provider ownership becomes shared so workers can use the same provider instances.
- `mcp/src/service.rs`: handshake instructions name the new tool; existing surface-count assertion now includes it.
- `mcp/tests/stdio_smoke.rs`: expected tools/list appends the new tool; the existing ten keep their order.
- `STEP3-REPORT.md` and `step3-evidence/`: report and raw run evidence, bridge transcripts, source hashes, run summary, and review checks.

Inherited Step 2 changes in `DOCS-NOTES.md`, `mcp/src/provider.rs`, and `mcp/src/provider_buttplug.rs` remain byte-identical as patches to their initial snapshot (step3-evidence/review.log:4; run-summary.json). STEP2-REPORT.md and evidence/ were preserved. No package version bump: the feature milestone is ache 1.1; the supplied Cargo package still reports 0.4.0, as its original tests require.

## Gates and run evidence

All paths below are relative to `step3-evidence/`.

| Gate | Result | Run evidence |
|---|---|---|
| Untouched Step 2 baseline | PASS: **146 unit + 9 stdio = 155**, zero failures | baseline.log:413; baseline.log:428 |
| Final full suite | PASS: **166 unit + 9 stdio = 175**, zero failures | final-gates.log:398; final-gates.log:413 |
| New-test balance | PASS: **20 = 10 validation + 10 adversarial**, all passing | review.log:6; run-summary.json lists each test and its final-gates.log line |
| Quartet plans clean | PASS: Lush 4 / Domi 2 / Flexer / Nora, distinct levels and Vibrate / Vibrate / mapped Fingering / Rotate | final-gates.log:314 |
| Whole refusal, named parts, zero sends on both mocks | PASS: invalid Flexer and Nora parts name the unsupported actions; no output sent on either provider | final-gates.log:313 |
| Per-part ceiling | PASS: each planned part clamps independently; tool result reports effective clamping | final-gates.log:309; final-gates.log:315 |
| Disarmed score | PASS: whole refusal, no sends | final-gates.log:312 |
| Bounds, unknown device, reduced capability, duplicate stream | PASS | final-gates.log:310–311 |
| Concurrent dispatch | PASS: first sends from **4 distinct devices** precede second segments; all second segments subsequently arrive | final-gates.log:394; test assertions in server.rs `validation_concurrent_streams_interleave_before_any_part_finishes` |
| One-part parity | PASS: identical initial provider plan and Lovense wire rendering to equivalent single command | final-gates.log:346 |
| Ensemble stop | PASS: every fixture device's final recorded output is zero before stop returns; Lovense Stop also recorded | final-gates.log:353 |
| Mixed-provider ensemble | PASS: both provider routes start and stop together | final-gates.log:345 |
| Watchdog / heartbeat | PASS: expired heartbeat cancels and zeroes the ensemble; healthy heartbeat preserves it | final-gates.log:359; final-gates.log:352 |
| Reconnect non-resume | PASS: provider recovery cannot resume a failed score | final-gates.log:383 |
| No stale segment after stop/re-arm | PASS: waiting streams stay cancelled past their next segment deadline | final-gates.log:396 |
| Completion / action transitions | PASS: natural completion zeroes and reports idle; previous action zero precedes next action | final-gates.log:392–393 |
| Stop failure / shutdown | PASS: failed Lovense zero still attempts the other provider and cancels; shutdown cancels all parts | final-gates.log:337–338 |
| Argument strictness | PASS: malformed score and unknown fields send nothing | final-gates.log:303 |
| Existing Lovense wire invariant | PASS: original wire-equality and gate-only-body tests unchanged | final-gates.log:272; final-gates.log:339 |
| Tool surface | PASS: **11 tools**, exactly one append, existing stop retained | final-gates.log:315; original schema/service/stdio tests in the same final run |
| Clippy | PASS: `cargo clippy --all-targets -- -D warnings`, exit 0 | final-gates.log:641; review.log:2 |
| Formatting | PASS: `cargo fmt -- --check`, exit 0 | format.log:1; review.log:2 |
| Diff whitespace | PASS: `git diff --check`, exit 0 | review.log:3 |
| Theodolite Rust | PASS with qualification below: **1 planted error**, then **0 diagnostics on each of 6 Rust files** | theodolite.jsonl:5–14 |

## Theodolite evidence strength

The bacon producer started before the bridge (theodolite.jsonl:1), using `cargo check --all-targets` with cargo-json export. In the same bridge session, `compile_error!("ACHE_STEP3_POSITIVE_CONTROL")` was planted in decide.rs, observed as a severity-1 diagnostic, and removed by restoring the exact original bytes (theodolite.jsonl:4–7). After the producer rebuilt successfully and exported an empty locations file, the bridge returned zero diagnostics for decide.rs, server.rs, service.rs, tests/stdio_smoke.rs, provider.rs, and provider_buttplug.rs (theodolite.jsonl:8–13). Final source hashes are recorded in run-summary.json. Bridge and producer processes owned by this session were stopped (theodolite.jsonl:16).

Qualification: responses explicitly carry `possibly_incomplete: server_not_quiescent`; this is observed zero from a proven live producer with a same-session planted-error control, not a claim of LSP quiescence. Shared config hash and server versions are preserved in the session-status response (theodolite.jsonl:14). Markdown, logs, JSON, JSONL, and text evidence have no applicable shared theodolite route and are **not-LSP-validated**. No TOML source files changed.

## Failures encountered and resolved

- Baseline launched above the toolchain file: toolchain selection failed; rerunning from mcp/ passed before implementation edits.
- First implementation compile hit the JSON macro recursion limit. Factored the new schema into its own JSON value rather than changing crate-wide compiler limits (initial.log).
- Provider sharing initially needed explicit Arc dereferencing at trait-object read sites; fixed those casts (second.log).
- First new-test run: one disarmed-refusal assertion inspected Debug spelling instead of the structured error code; corrected the assertion. Final suite passes without removing or weakening the refusal/silence checks (tests.log:394–406; final-gates.log:312).

## Residuals and scope limits

No unresolved implementation gate. The theodolite quiescence qualification remains explicit for review. No hardware, Server API provider, Toy Events consumption, new vocabulary, commits, or pushes. Runtime capability reporting remains authoritative; fixture model names do not upgrade any matrix claim to hardware-verified support. Existing transport.rs fallback-documentation debt noted in STEP2-REPORT.md remains untouched. Provider I/O latency still bounds how quickly an in-flight send can release the shared mutex for stop; segment waits never hold it. Worker scheduling does not promise sample-accurate physical synchronization.
