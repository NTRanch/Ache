# Changelog

All notable changes to this repository's MCP server. Versions are the
crate version in `mcp/Cargo.toml`.

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
