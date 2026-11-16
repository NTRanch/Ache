//! Safety-surface tests for ache-tui. Roughly half are adversarial:
//! they aim at the ways a safety console could lie — assuming
//! disarmed, retrying a refusal, showing stale arm state, trusting
//! an over-max ceiling, or losing the STOP control.

use ache_tui::app::{App, RunMode, RunOutcome, Screen, StagedRun};
use ache_tui::facade::{FacadeError, FakeFacade, Reply, StatusView, status_json};
use ache_tui::panels::{fleet_lines, overview_lines};
use ache_tui::screens::execute_line;
use serde_json::json;

fn ok_status(armed: bool, ceiling: u64) -> Reply {
    Reply::Ok(status_json(armed, ceiling))
}

fn vibrate() -> StagedRun {
    StagedRun {
        tool: "lovense_function".to_string(),
        args: json!({"feature": "Vibrate", "strength": 5, "time_sec": 5}),
    }
}

fn buffer_text(lines: &[ratatui::text::Line<'static>]) -> String {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ------------------------------------------------------- validation

#[test]
fn fresh_app_is_plan_mode_disarmed_unknown() {
    let app = App::new();
    assert_eq!(app.mode, RunMode::Plan);
    assert!(matches!(app.status, StatusView::Unknown(_)));
    assert_eq!(app.currently_armed(), None);
}

#[test]
fn status_disarmed_renders_disarmed() {
    let mut fake = FakeFacade::with_status(vec![ok_status(false, 20)]);
    let mut app = App::new();
    app.refresh_status(&mut fake);
    let text = buffer_text(&overview_lines(&app));
    assert!(text.contains("DISARMED"), "{text}");
    assert!(!text.contains("*** MASTER: ARMED ***"), "{text}");
}

#[test]
fn plan_mode_sends_nothing_before_approval() {
    let mut fake = FakeFacade::with_status(vec![ok_status(true, 20), ok_status(true, 20)]);
    let mut app = App::new();
    app.stage_or_run(&mut fake, vibrate());
    assert_eq!(fake.call_count("lovense_function"), 0);
    assert!(app.staged.is_some());
    app.abort_staged();
    assert_eq!(fake.call_count("lovense_function"), 0);
}

#[test]
fn approve_sends_exactly_once_after_fresh_status() {
    let mut fake = FakeFacade::with_status(vec![
        ok_status(true, 20),
        ok_status(true, 20),
        ok_status(true, 20),
    ]);
    fake.push_call_reply(Reply::Ok(json!({"sent": true})));
    let mut app = App::new();
    app.stage_or_run(&mut fake, vibrate());
    app.approve_staged(&mut fake);
    assert_eq!(fake.call_count("lovense_function"), 1);
    assert!(matches!(app.outcome, RunOutcome::Done(_)));
}

#[test]
fn auto_mode_executes_declared_operation() {
    let mut fake = FakeFacade::with_status(vec![ok_status(true, 20), ok_status(true, 20)]);
    fake.push_call_reply(Reply::Ok(json!({"sent": true})));
    let mut app = App::new();
    app.mode = RunMode::Auto;
    app.stage_or_run(&mut fake, vibrate());
    assert_eq!(fake.call_count("lovense_function"), 1);
}

#[test]
fn stop_from_overview_calls_real_stop() {
    let mut fake = FakeFacade::with_status(vec![ok_status(false, 20)]);
    fake.push_call_reply(Reply::Ok(json!({"stopped": true})));
    let mut app = App::new();
    app.screen = Screen::Overview;
    app.on_key(&mut fake, 's');
    assert_eq!(fake.call_count("lovense_stop"), 1);
}

#[test]
fn fleet_panel_has_no_rules_and_says_why() {
    let app = App::new();
    let text = buffer_text(&fleet_lines(&app));
    assert!(text.contains("trigger rules declared: 0"), "{text}");
    assert!(text.contains("Owner decision"), "{text}");
}

#[test]
fn line_mode_status_and_stop() {
    let mut fake = FakeFacade::with_status(vec![ok_status(false, 20), ok_status(false, 20)]);
    fake.push_call_reply(Reply::Ok(json!({"stopped": true})));
    let mut app = App::new();
    let answer = execute_line(&mut app, &mut fake, "status");
    assert!(answer.contains("armed=false"), "{answer}");
    let answer = execute_line(&mut app, &mut fake, "stop");
    assert!(answer.starts_with("DONE"), "{answer}");
    assert_eq!(fake.call_count("lovense_stop"), 1);
}

#[test]
fn headless_first_screen_renders() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let app = App::new();
    terminal
        .draw(|frame| {
            let area = frame.area();
            ache_tui::panels::render(&app, frame, area);
        })
        .expect("draw");
    let text = format!("{:?}", terminal.backend().buffer());
    assert!(text.contains("ache-tui"), "{text}");
    assert!(text.contains("UNKNOWN"), "{text}");
}

// ------------------------------------------------------- adversarial

#[test]
fn adversarial_starts_armed_renders_armed_truthfully() {
    // The server reports ARMED at startup: the TUI must not assume
    // the disarmed default.
    let mut fake = FakeFacade::with_status(vec![ok_status(true, 20)]);
    let mut app = App::new();
    app.refresh_status(&mut fake);
    assert_eq!(app.currently_armed(), Some(true));
    let text = buffer_text(&overview_lines(&app));
    assert!(text.contains("*** MASTER: ARMED ***"), "{text}");
}

#[test]
fn adversarial_gate_refusal_rendered_not_retried() {
    let mut fake = FakeFacade::with_status(vec![
        ok_status(false, 20),
        ok_status(false, 20),
        ok_status(false, 20),
    ]);
    fake.push_call_reply(Reply::Err(FacadeError::new(
        "gate_refused",
        "master is disarmed",
    )));
    let mut app = App::new();
    app.mode = RunMode::Auto;
    app.stage_or_run(&mut fake, vibrate());
    assert_eq!(
        fake.call_count("lovense_function"),
        1,
        "exactly one attempt, no retry"
    );
    match &app.outcome {
        RunOutcome::Refused(msg) => assert!(msg.contains("disarmed")),
        other => panic!("expected Refused, got {other:?}"),
    }
    assert!(
        app.log.iter().any(|l| l.contains("REFUSED")),
        "{:?}",
        app.log
    );
}

#[test]
fn adversarial_server_dies_status_becomes_unknown_not_stale() {
    let mut fake = FakeFacade::with_status(vec![ok_status(true, 20)]);
    let mut app = App::new();
    app.refresh_status(&mut fake);
    assert_eq!(app.currently_armed(), Some(true));
    // Queue exhausted: next status reads as a dead server.
    app.refresh_status(&mut fake);
    assert_eq!(app.currently_armed(), None);
    let text = buffer_text(&overview_lines(&app));
    assert!(text.contains("UNKNOWN"), "{text}");
    assert!(
        !text.contains("*** MASTER: ARMED ***"),
        "stale ARMED must vanish: {text}"
    );
}

#[test]
fn adversarial_run_blocked_when_gate_state_unknown() {
    let mut fake = FakeFacade::with_status(vec![]);
    fake.push_call_reply(Reply::Ok(json!({"sent": true})));
    let mut app = App::new();
    app.mode = RunMode::Auto;
    app.stage_or_run(&mut fake, vibrate());
    assert_eq!(fake.call_count("lovense_function"), 0, "no blind run");
    assert!(matches!(app.outcome, RunOutcome::Failed(_)));
}

#[test]
fn adversarial_ceiling_above_max_clamped_and_flagged() {
    let mut fake = FakeFacade::with_status(vec![ok_status(false, 99)]);
    let mut app = App::new();
    app.refresh_status(&mut fake);
    match &app.status {
        StatusView::Known(s) => {
            assert_eq!(s.ceiling_reported, 99);
            assert_eq!(s.ceiling_display, 20);
            assert!(s.ceiling_anomaly);
        }
        other => panic!("expected Known, got {other:?}"),
    }
    let text = buffer_text(&overview_lines(&app));
    assert!(text.contains("ANOMALY"), "{text}");
    assert!(
        app.log.iter().any(|l| l.contains("ANOMALY")),
        "{:?}",
        app.log
    );
}

#[test]
fn adversarial_stop_key_from_every_screen_state() {
    for screen in Screen::ALL {
        let mut fake = FakeFacade::with_status(vec![ok_status(false, 20), ok_status(false, 20)]);
        fake.push_call_reply(Reply::Ok(json!({"stopped": true})));
        let mut app = App::new();
        app.screen = screen;
        // Even with a run staged, STOP wins and drops the stage.
        app.staged = Some(vibrate());
        app.on_key(&mut fake, 's');
        assert_eq!(fake.call_count("lovense_stop"), 1, "screen {screen:?}");
        assert!(
            app.staged.is_none(),
            "staged run dropped by STOP on {screen:?}"
        );
        assert_eq!(fake.call_count("lovense_function"), 0);
    }
}

#[test]
fn adversarial_malformed_status_is_unknown_not_panic() {
    let mut fake = FakeFacade::with_status(vec![Reply::Ok(json!({"armed": "yes"}))]);
    let mut app = App::new();
    app.refresh_status(&mut fake);
    assert!(matches!(app.status, StatusView::Unknown(_)));
}

#[test]
fn adversarial_oversized_log_stays_bounded() {
    let mut app = App::new();
    for i in 0..(ache_tui::LOG_LINES_MAX + 50) {
        app.log_line(format!("line {i} {}", "x".repeat(500)));
    }
    assert!(app.log.len() <= ache_tui::LOG_LINES_MAX);
    assert!(
        app.log
            .iter()
            .all(|l| l.chars().count() <= ache_tui::LOG_LINE_CHARS_MAX)
    );
}

#[test]
fn adversarial_line_mode_plan_then_abort_sends_nothing() {
    let mut fake = FakeFacade::with_status(vec![ok_status(true, 20)]);
    let mut app = App::new();
    let answer = execute_line(
        &mut app,
        &mut fake,
        "plan lovense_function {\"feature\":\"Vibrate\",\"strength\":5,\"time_sec\":5}",
    );
    assert!(answer.contains("STAGED"), "{answer}");
    let answer = execute_line(&mut app, &mut fake, "abort");
    assert!(answer.contains("ABORTED"), "{answer}");
    assert_eq!(fake.call_count("lovense_function"), 0);
}

#[test]
fn adversarial_unknown_line_command_refused() {
    let mut fake = FakeFacade::with_status(vec![]);
    let mut app = App::new();
    let answer = execute_line(&mut app, &mut fake, "arm_everything_now");
    assert!(answer.starts_with("ERR"), "{answer}");
    assert!(fake.calls.is_empty());
}
