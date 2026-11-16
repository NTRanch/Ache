//! ache-tui — the safety console for the ache haptics MCP server.
//!
//! The console is a facade over the ache server (an MCP stdio server
//! binary): it never talks to hardware, never reads credentials, and
//! holds no gate authority of its own. Every output operation is sent
//! to the server, whose pure `decide()` gate independently allows or
//! refuses it. The TUI's first job is that gate state can never be
//! misread:
//!
//! - Arm state is re-read from the server for every run; the TUI
//!   caches no arm state.
//! - A refused gate decision is rendered as a refusal, never retried.
//! - When the server dies, status becomes `Unknown` — the last-known
//!   arm state is never displayed as current.
//! - A ceiling reported above [`CEILING_MAX`] is displayed clamped
//!   and flagged as a server anomaly.
//! - The synchronous STOP control (`s`) is available from every
//!   screen and calls the server's real `lovense_stop`.
//!
//! Modes: full-screen ratatui where a TTY exists, plus a non-TTY
//! line mode sharing the same execute path.

#![forbid(unsafe_code)]

pub mod app;
pub mod facade;
pub mod panels;
pub mod screens;

/// Hard ceiling for device intensity, mirrored from the server's
/// config contract (`intensity_ceiling` is validated `0..=20`).
pub const CEILING_MAX: u8 = 20;
/// Maximum number of audit/event lines retained in memory.
pub const LOG_LINES_MAX: usize = 512;
/// Maximum characters of one rendered log line.
pub const LOG_LINE_CHARS_MAX: usize = 240;
/// Maximum devices shown on the Workflows screen.
pub const DEVICES_SHOWN_MAX: usize = 64;
/// Maximum length of a staged tool name.
pub const TOOL_NAME_CHARS_MAX: usize = 64;
/// Fleet trigger rules declared for ache in rung 1: none, by design.
/// Hardware-adjacent triggers into ache are an Owner decision, and
/// ache firing other MCPs is out of scope for this rung.
pub const TRIGGER_RULES: &[&str] = &[];
/// Why the Fleet panel's rule list is empty (shown verbatim in UI).
pub const TRIGGER_EMPTY_REASON: &str = "No outbound trigger rules by design (rung 1): \
    triggers into ache from other MCPs are an Owner decision, and ache \
    firing other MCPs is out of scope. A rule not listed here does not exist.";
