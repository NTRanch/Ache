# Testing the Lovense Integration

A ladder: cheapest and safest first, each rung gated before the
next begins. The governing principle is that the real transport
never meets reality for the first time with a live toy at the end
of it.

General rules for every rung:

- Tests split evenly between validation (does the correct thing
  happen) and adversarial (does the wrong thing fail safely).
- Success criteria are written before the run, not after.
- Every request and response is logged with timestamps. A rung
  without a log did not happen.
- Ports are configuration, never trusted defaults. Lovense has
  moved them before (34567 era; currently HTTP 20010 / HTTPS 30010)
  and per their own staff will move them again.
- The official docs are the oracle. Third-party protocol write-ups
  are never evidence where they disagree with the docs.

## Rung 1 — Contract tests against the mock

Status: largely in place for STROBE's bridge (the LovenseBridge
state machine and its test suite); extend as below.

- Every documented local error code mapped to a bridge state:
  500 server not started, 400 invalid command, 401 toy not found,
  402 toy not connected, 403 unsupported command, 404 invalid
  parameter, 506 restart Lovense Connect. Raw codes never surface
  to the player.
- Ceiling enforcement proven in the bridge: output values clamped
  to 0–20 on every call, including per-value inside Pattern
  strength lists; pattern lists capped at 50 values; intervals
  above 100 ms; loop fields valid.
- Indefinite output proven impossible: no code path emits
  `timeSec: 0`.

Gate: full suite green, with counts reported.

## Rung 2 — Fake Remote endpoint

A local stub server impersonating the Lovense Remote app's
`/command` endpoint, so the real transport code runs end to end
before any Lovense software is involved.

- Answers `GetToys` / `GetToyName` with configurable toy sets,
  including the empty set.
- Accepts `Function`, `Pattern`, `PatternV2`, `Preset`, `Position`
  and records exact bytes received (this is where clamp evidence
  comes from at later rungs).
- Misbehavior modes, each a test: slow responses, malformed JSON,
  connection drop mid-pattern, wrong-port listener (the transport
  must fail visibly, not hang or silently succeed).
- Port configurability proven: the same suite passes against the
  stub on nonstandard ports, because the transport read them from
  config.

Gate: transport passes the full suite against the stub, including
every misbehavior mode ending in a safe, player-visible state.

## Rung 3 — The real Remote app, no toy

Lovense Remote on PC, Game Mode enabled, LAN enabled, bridge
pointed at it. No toy connected.

- `GetToys` returns the real empty-set response; compare byte-level
  shape against the Rung 2 stub and update the stub fixtures from
  the real responses.
- The `X-platform` header value ("Strobe") appears on the Remote
  screen as the connecting app name.
- Connection lifecycle exercised: connect, idle to heartbeat
  timeout, reconnect. Reconnect never resumes output (there is
  none at this rung; verify the state machine agrees).
- Error paths: commands sent with no toy produce the documented
  codes, and the bridge maps them as Rung 1 requires.

Gate: stub fixtures updated from real captures; no discrepancy
between stub behavior and app behavior left unexplained.

## Rung 4 — Live toy, bounded matrix

Live work. This rung requires an explicit go from the studio
before it runs, every time the matrix changes materially.

- Single `Function` command at minimal intensity, bounded
  `timeSec` of 1–2 seconds. Confirm output starts and stops.
- Stop latency: trigger each stop path (pause, menu, focus loss,
  panic input) mid-output; measure trigger-to-zero from the log.
  Panic stop latches; recovery requires fresh connect and explicit
  arm.
- Clamp proof on the wire: request 25 through the test harness;
  the Rung 2 recorder (or a logging proxy) must show 20 as the
  maximum value the transport emitted.
- Disconnect mid-pattern: toy or app drops; output zeroes; state
  lands on Disconnected; no retry storm (bounded backoff, visible
  state).
- Reconnect during an armed session: output does not resume.

Gate: every matrix row passes with log evidence, or the rung
stops and the failure is written up before any retry.

## Rung 5 — Server-side credential flows (parallel track)

Independent of STROBE's route; validates the credentials stored at
`lovense/ntr/` in a throwaway harness, never in the game plugin.

- Request a QR code from the Lovense API using the developer
  token; scan it in Lovense Remote; verify the callback payload
  arrives at a local listener with the documented fields (domain,
  httpPort, httpsPort, wsPort, toys, utoken).
- Verify discovered ports match what the app actually listens on
  (this is the sanctioned way to learn ports instead of assuming
  them).
- Exercise the ctoken flow for Media Sync if Phase 5 of the
  checklist is adopted.
- Credentials appear only in the harness's local environment,
  loaded from the password store at run time; never in source,
  fixtures, or logs (logs redact token and utoken values).

Gate: callback received and fields verified; a redacted log
proves the flow without retaining secrets.

## Rung 6 — Unity plugins (spike project only)

Per CHECKLIST.md Phase 3, in a throwaway copy of the project:

- Import compiles against the project's Unity version.
- Connect, one bounded command, disconnect; button, shake, and
  shake-times events observed for the Windows and Dongle plugins.
- Behavior compared against the bridge constraints (master toggle,
  clamp in our code, synchronous stop). A plugin that cannot honor
  them is rejected on evidence, not on preference.

## What not to do

- Do not test STROBE through the cloud server route. It was
  rejected on latency and token-custody grounds, and testing there
  measures Lovense's servers more than our code.
- Do not treat a successful demo call as a passed rung. Rungs pass
  on logged evidence against criteria written in advance.
- Do not blur mock and hardware results in any report. Every result
  states which it ran against.
