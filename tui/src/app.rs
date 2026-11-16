//! App state machine: screens, run modes, staged workflows, the
//! audit log, and the STOP control that works from every screen.
//!
//! Safety invariants enforced here:
//! - Plan mode is the default; a staged call sends nothing until it
//!   is explicitly approved.
//! - Before any approved/auto run executes, arm state is re-read
//!   from the server (`refresh_status`); no cached arm state is
//!   ever consulted for a run.
//! - A gate refusal is logged and shown; the run ends. There is no
//!   retry path.
//! - If a status read fails, the view becomes `Unknown` and staged
//!   or auto runs are blocked until the server answers again.

use serde_json::{Value, json};

use crate::facade::{Facade, FacadeError, StatusView};
use crate::{LOG_LINE_CHARS_MAX, LOG_LINES_MAX};

/// The four screens. No Media screen: ache has no media use case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Gate state overview.
    Overview,
    /// Device list + staged operations.
    Workflows,
    /// Audit log / live event stream.
    Log,
    /// Fleet panel (trigger rules empty by design in rung 1).
    Fleet,
}

impl Screen {
    /// All screens, in tab order.
    pub const ALL: [Screen; 4] = [
        Screen::Overview,
        Screen::Workflows,
        Screen::Log,
        Screen::Fleet,
    ];
    /// Short label for the tab bar.
    pub fn label(self) -> &'static str {
        match self {
            Screen::Overview => "Overview",
            Screen::Workflows => "Workflows",
            Screen::Log => "Log/Live",
            Screen::Fleet => "Fleet",
        }
    }
}

/// Run mode. Plan is the default and the safer state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Stage the call, show exact tool + args, wait for approval.
    Plan,
    /// Execute declared operations directly — still only what the
    /// server's `decide()` gate independently allows.
    Auto,
}

/// One staged workflow operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedRun {
    /// MCP tool name to call.
    pub tool: String,
    /// Exact arguments that will be sent on approval.
    pub args: Value,
}

/// Outcome of the most recent run, for the Workflows screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// Nothing has run yet.
    None,
    /// The server executed the call.
    Done(String),
    /// The server's gate refused; rendered as a refusal, final.
    Refused(String),
    /// The call never reached a gate decision (transport failure).
    Failed(String),
}

/// Application state. Invalid states are kept unrepresentable: a
/// staged run exists only in plan mode awaiting an explicit
/// decision, and `status` is either freshly known or `Unknown`.
pub struct App {
    /// Current screen.
    pub screen: Screen,
    /// Current gate status view (fresh or Unknown, never stale).
    pub status: StatusView,
    /// Plan (default) or Auto.
    pub mode: RunMode,
    /// Operation staged for approval, if any.
    pub staged: Option<StagedRun>,
    /// Outcome of the last executed run.
    pub outcome: RunOutcome,
    /// Last device list payload, if fetched.
    pub devices: Option<Value>,
    /// Bounded audit log: every command sent and every gate
    /// decision returned, in order.
    pub log: Vec<String>,
    /// Set when the operator asks to quit.
    pub should_quit: bool,
}

impl App {
    /// A fresh app on the Overview screen, plan mode, status not
    /// yet read (`Unknown` until the first refresh lands).
    pub fn new() -> App {
        App {
            screen: Screen::Overview,
            status: StatusView::Unknown("status not yet read from server".to_string()),
            mode: RunMode::Plan,
            staged: None,
            outcome: RunOutcome::None,
            devices: None,
            log: Vec::new(),
            should_quit: false,
        }
    }

    /// Append one bounded line to the audit log.
    pub fn log_line(&mut self, line: impl Into<String>) {
        let mut text = line.into();
        if text.chars().count() > LOG_LINE_CHARS_MAX {
            text = text.chars().take(LOG_LINE_CHARS_MAX).collect();
        }
        self.log.push(text);
        if self.log.len() > LOG_LINES_MAX {
            let excess = self.log.len() - LOG_LINES_MAX;
            self.log.drain(0..excess);
        }
    }

    /// Re-read gate status from the server. On any failure the view
    /// becomes `Unknown` — last-known arm state is discarded, never
    /// kept on screen as if current.
    pub fn refresh_status(&mut self, facade: &mut dyn Facade) {
        match facade.status() {
            Ok(status) => {
                if status.ceiling_anomaly {
                    self.log_line(format!(
                        "ANOMALY server reported ceiling {} above max {}; display clamped",
                        status.ceiling_reported,
                        crate::CEILING_MAX
                    ));
                }
                self.status = StatusView::Known(status);
            }
            Err(error) => {
                self.log_line(format!("STATUS UNKNOWN: {error}"));
                self.status = StatusView::Unknown(error.message);
            }
        }
    }

    /// True only when the server *currently* reports armed.
    pub fn currently_armed(&self) -> Option<bool> {
        match &self.status {
            StatusView::Known(status) => Some(status.armed),
            StatusView::Unknown(_) => None,
        }
    }

    /// Stage an operation. In plan mode nothing is sent; in auto
    /// mode it executes immediately through `execute`.
    pub fn stage_or_run(&mut self, facade: &mut dyn Facade, run: StagedRun) {
        self.log_line(format!("STAGE {} {}", run.tool, run.args));
        match self.mode {
            RunMode::Plan => {
                self.staged = Some(run);
                self.log_line("PLAN staged; awaiting approval (y=approve, n=abort)");
            }
            RunMode::Auto => self.execute(facade, run),
        }
    }

    /// Approve the staged run (plan mode). Arm state is re-read
    /// from the server first; if the server cannot be read, the
    /// run is blocked, not executed on stale knowledge.
    pub fn approve_staged(&mut self, facade: &mut dyn Facade) {
        let Some(run) = self.staged.take() else {
            return;
        };
        self.execute(facade, run);
    }

    /// Abort the staged run. Nothing was or will be sent for it.
    pub fn abort_staged(&mut self) {
        if let Some(run) = self.staged.take() {
            self.log_line(format!("ABORT staged {} (never sent)", run.tool));
            self.outcome = RunOutcome::None;
        }
    }

    /// Execute one operation against the server. Re-reads status
    /// first (no cached arm state); a refusal is final.
    fn execute(&mut self, facade: &mut dyn Facade, run: StagedRun) {
        self.refresh_status(facade);
        if !matches!(self.status, StatusView::Known(_)) {
            self.log_line(format!(
                "BLOCKED {}: gate state unknown; refusing to run blind",
                run.tool
            ));
            self.outcome = RunOutcome::Failed("gate state unknown; run blocked".to_string());
            return;
        }
        self.log_line(format!("SEND {} {}", run.tool, run.args));
        match facade.call(&run.tool, run.args.clone()) {
            Ok(value) => {
                self.log_line(format!("OK {} -> {}", run.tool, value));
                self.outcome = RunOutcome::Done(format!("{} completed", run.tool));
            }
            Err(error) => {
                if error.is_gate_refusal() {
                    self.log_line(format!("REFUSED {}: {}", run.tool, error.message));
                    self.outcome = RunOutcome::Refused(error.message);
                } else {
                    self.log_line(format!("FAILED {}: {}", run.tool, error));
                    self.outcome = RunOutcome::Failed(error.message);
                }
            }
        }
        self.refresh_status(facade);
    }

    /// Fetch the unified device list into the Workflows screen.
    pub fn refresh_devices(&mut self, facade: &mut dyn Facade) {
        self.log_line("SEND ache_devices {}");
        match facade.devices() {
            Ok(value) => {
                self.log_line(format!("OK ache_devices -> count {}", value["count"]));
                self.devices = Some(value);
            }
            Err(error) => {
                self.log_line(format!("FAILED ache_devices: {error}"));
                self.devices = None;
            }
        }
    }

    /// The synchronous STOP control: the server's real
    /// `lovense_stop`. Available from every screen, in every state
    /// (including with a run staged — the staged run is dropped).
    pub fn stop(&mut self, facade: &mut dyn Facade) {
        self.staged = None;
        self.log_line("SEND lovense_stop {} (STOP control)");
        match facade.stop() {
            Ok(value) => {
                self.log_line(format!("STOPPED: {value}"));
                self.outcome =
                    RunOutcome::Done("stop sent; server zeroed all providers".to_string());
            }
            Err(error) => {
                self.log_line(format!("STOP FAILED: {error}"));
                self.outcome = RunOutcome::Failed(format!("stop failed: {}", error.message));
            }
        }
        self.refresh_status(facade);
    }

    /// Handle one key. `s` is STOP from every screen; `q` quits;
    /// Tab cycles screens; `m` toggles plan/auto; `y`/`n` decide a
    /// staged run; `d` refreshes devices; `r` refreshes status.
    pub fn on_key(&mut self, facade: &mut dyn Facade, key: char) {
        match key {
            's' | 'S' => self.stop(facade),
            'q' => self.should_quit = true,
            '\t' => {
                let idx = Screen::ALL
                    .iter()
                    .position(|s| *s == self.screen)
                    .unwrap_or(0);
                self.screen = Screen::ALL[(idx + 1) % Screen::ALL.len()];
            }
            'm' => {
                self.mode = match self.mode {
                    RunMode::Plan => RunMode::Auto,
                    RunMode::Auto => RunMode::Plan,
                };
                self.log_line(format!("MODE {:?}", self.mode));
            }
            'y' if self.staged.is_some() => self.approve_staged(facade),
            'n' if self.staged.is_some() => self.abort_staged(),
            'd' => self.refresh_devices(facade),
            'r' => self.refresh_status(facade),
            _ => {}
        }
    }
}

impl Default for App {
    fn default() -> App {
        App::new()
    }
}

/// The declared workflow operations the Workflows screen offers.
/// Each is an ordinary server tool call; the gate decides.
pub fn declared_workflows() -> Vec<StagedRun> {
    vec![
        StagedRun {
            tool: "lovense_function".to_string(),
            args: json!({"feature": "Vibrate", "strength": 5, "time_sec": 5}),
        },
        StagedRun {
            tool: "lovense_preset".to_string(),
            args: json!({"name": "pulse", "time_sec": 5}),
        },
        StagedRun {
            tool: "lovense_disarm".to_string(),
            args: json!({}),
        },
    ]
}

/// Convenience error constructor for tests and line mode.
pub fn server_gone(message: &str) -> FacadeError {
    FacadeError::new("server_gone", message)
}
