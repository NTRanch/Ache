# ache-tui — safety console for the ache MCP server

Rung 1 of the fleet TUI programme (D-1562). A facade over the ache
server binary via an MCP stdio client (modern era 2026-07-28,
NDJSON JSON-RPC — the framing the server's own stdio smoke test
uses). The TUI talks to the server only; it never touches hardware,
never reads credentials, and holds no gate authority.

Safety contract:

- Overview renders the master arm state unmissably: DISARMED
  (default), ARMED, or UNKNOWN. Arm state is re-read from the
  server for every run — the TUI caches no arm state, and when the
  server dies the view becomes UNKNOWN (last-known state is
  discarded, never displayed as current).
- Plan mode is the default: operations stage with exact tool +
  args and send nothing until approved. Auto mode executes
  declared operations directly, but only what the server's
  `decide()` gate independently allows — a refused gate decision
  is rendered as a refusal and never retried.
- A server-reported ceiling above 20 is displayed clamped and
  flagged as an anomaly.
- STOP (`s`) is available from every screen and calls the
  server's real `lovense_stop` (zeroes all providers, latches
  the master off).
- Log/Live shows every command sent and every gate decision
  returned. The Fleet panel declares zero outbound trigger
  rules by design in rung 1 (reason shown in the UI). No Media
  screen: ache has no media use case.

Modes: full-screen ratatui on a TTY; line mode otherwise
(`status`, `devices`, `tools`, `plan`, `approve`, `abort`,
`auto`, `stop`, `log`, `quit`), sharing the same execute path.

Run: `ache-tui [--server <path-to-ache-binary>] [--line]`.
Tests run against `FakeFacade` only — no live hardware, ever.
