//! Line mode: the non-TTY surface sharing the same execute path
//! as the full-screen TUI (phlow precedent). One command per line
//! on stdin, one answer per line on stdout. Scriptable and used by
//! the headless smoke test.
//!
//! Commands: `status`, `devices`, `tools`, `plan <tool> <json>`,
//! `approve`, `abort`, `auto <tool> <json>`, `stop`, `quit`.
//! `plan` stages without sending; `approve` sends the staged call
//! (after a fresh status re-read inside `App`); `auto` sends a
//! declared call directly. STOP works in any state.

use serde_json::Value;

use crate::app::{App, StagedRun};
use crate::facade::Facade;

/// Execute one line-mode command against `app`/`facade` and return
/// the answer line. Unknown commands are a typed refusal, never a
/// panic.
pub fn execute_line(app: &mut App, facade: &mut dyn Facade, line: &str) -> String {
    let line = line.trim();
    if line.is_empty() {
        return "ERR empty command".to_string();
    }
    let (verb, rest) = match line.split_once(' ') {
        Some((v, r)) => (v, r.trim()),
        None => (line, ""),
    };
    match verb {
        "status" => {
            app.refresh_status(facade);
            match &app.status {
                crate::facade::StatusView::Known(s) => format!(
                    "STATUS armed={} ceiling={} anomaly={} server_api_enabled={} server={}",
                    s.armed,
                    s.ceiling_display,
                    s.ceiling_anomaly,
                    s.server_api_enabled,
                    s.server_label
                ),
                crate::facade::StatusView::Unknown(reason) => {
                    format!("STATUS unknown: {reason}")
                }
            }
        }
        "devices" => {
            app.refresh_devices(facade);
            match &app.devices {
                Some(d) => format!("DEVICES count={}", d["count"]),
                None => "DEVICES unavailable".to_string(),
            }
        }
        "tools" => match facade.tools() {
            Ok(value) => {
                let count = value["tools"].as_array().map_or(0, Vec::len);
                format!("TOOLS count={count}")
            }
            Err(error) => format!("ERR {error}"),
        },
        "plan" | "auto" => {
            let Some(run) = parse_run(rest) else {
                return format!("ERR usage: {verb} <tool> <json-args>");
            };
            if verb == "plan" {
                let previous_mode = app.mode;
                app.mode = crate::app::RunMode::Plan;
                app.stage_or_run(facade, run);
                app.mode = previous_mode;
                "STAGED (not sent; use approve/abort)".to_string()
            } else {
                let previous_mode = app.mode;
                app.mode = crate::app::RunMode::Auto;
                app.stage_or_run(facade, run);
                app.mode = previous_mode;
                outcome_text(app)
            }
        }
        "approve" => {
            app.approve_staged(facade);
            outcome_text(app)
        }
        "abort" => {
            app.abort_staged();
            "ABORTED (nothing sent)".to_string()
        }
        "stop" => {
            app.stop(facade);
            outcome_text(app)
        }
        "log" => app.log.join(" | "),
        "quit" => {
            app.should_quit = true;
            "BYE".to_string()
        }
        other => format!("ERR unknown command: {other}"),
    }
}

fn outcome_text(app: &App) -> String {
    match &app.outcome {
        crate::app::RunOutcome::None => "NOOP".to_string(),
        crate::app::RunOutcome::Done(text) => format!("DONE {text}"),
        crate::app::RunOutcome::Refused(text) => format!("REFUSED {text}"),
        crate::app::RunOutcome::Failed(text) => format!("FAILED {text}"),
    }
}

fn parse_run(rest: &str) -> Option<StagedRun> {
    let (tool, args_text) = rest.split_once(' ')?;
    if tool.is_empty() || tool.len() > crate::TOOL_NAME_CHARS_MAX {
        return None;
    }
    let args: Value = serde_json::from_str(args_text.trim()).ok()?;
    if !args.is_object() {
        return None;
    }
    Some(StagedRun {
        tool: tool.to_string(),
        args,
    })
}
