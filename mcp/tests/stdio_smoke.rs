//! Binary-level stdio tests: spawn the built server, speak JSON-RPC over
//! its pipes. Modern era only (2026-07-28): sessions open with
//! `server/discover` and every request carries the `_meta` block the
//! era requires. The smoke writes its transcript to
//! `$CARGO_TARGET_TMPDIR/stdio-smoke.txt` as evidence.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const SERVER: &str = env!("CARGO_BIN_EXE_ache");
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The era `_meta` object; the 2026-07-28 era carries it in `params._meta`.
const META: &str = r#"{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"stdio-smoke","version":"1.0.0"},"io.modelcontextprotocol/clientCapabilities":{}}"#;

/// Run the server with `config` (None = no config file), feed `input`,
/// and collect everything it printed.
fn run(config: Option<&str>, input: &str) -> Output {
    run_staged(config, &[input], std::time::Duration::ZERO)
}

/// Like `run`, but writes `parts` in order with `gap` of silence
/// between them, so time-based behavior (the watchdog) is observable
/// at the binary level.
fn run_staged(config: Option<&str>, parts: &[&str], gap: std::time::Duration) -> Output {
    let mut command = Command::new(SERVER);
    command.env_remove("ACHE_MCP_CONFIG");
    if let Some(text) = config {
        let path = config_file(text);
        command.env("ACHE_MCP_CONFIG", path);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn server");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    for (index, part) in parts.iter().enumerate() {
        if index > 0 && !gap.is_zero() {
            std::thread::sleep(gap);
        }
        // The server may exit before reading (bad config); a broken pipe is fine.
        let _ = stdin.write_all(part.as_bytes());
        let _ = stdin.flush();
    }
    drop(stdin);
    child
        .wait_with_output()
        .expect("server exits at end of input")
}

fn config_file(text: &str) -> PathBuf {
    let digest: u64 = text.bytes().fold(14695981039346656037, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(1099511628211)
    });
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("config-{digest:x}.toml"));
    std::fs::write(&path, text).expect("write config");
    path
}

fn responses(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("each stdout line is JSON"))
        .collect()
}

fn tool_text(response: &Value) -> Value {
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

/// A modern `tools/call` frame with the era `_meta` block.
fn call(id: i64, name: &str, arguments: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{arguments},"_meta":{META}}}}}"#
    )
}

fn discover(id: i64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"server/discover","params":{{"_meta":{META}}}}}"#
    )
}

/// One full mock session: discovery, a denied output while disarmed,
/// arm, an over-ceiling function (clamped on the wire), a pattern, a
/// preset, stop, and the final state.
fn smoke_session() -> String {
    let frames = [
        discover(1),
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":999,"reason":"smoke"}}"#
            .to_string(),
        format!(r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{"_meta":{META}}}}}"#),
        call(3, "lovense_status", "{}"),
        call(4, "lovense_toys", "{}"),
        call(5, "lovense_function", r#"{"feature":"Vibrate","strength":5,"time_sec":5}"#),
        call(6, "lovense_arm", "{}"),
        call(7, "lovense_function", r#"{"feature":"Vibrate","strength":25,"time_sec":5}"#),
        call(8, "lovense_pattern", r#"{"features":"v","interval_ms":500,"strengths":[10,20],"time_sec":4}"#),
        call(9, "lovense_preset", r#"{"name":"pulse","time_sec":9}"#),
        call(10, "lovense_stop", "{}"),
        call(11, "lovense_status", "{}"),
    ];
    let mut session = frames.join("\n");
    session.push('\n');
    session
}

// --- validation ---

#[test]
fn stdio_smoke_over_mock_transport() {
    let smoke = smoke_session();
    let output = run(None, &smoke);
    assert!(output.status.success());
    let lines = responses(&output);
    let ids: Vec<i64> = lines
        .iter()
        .map(|line| line["id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        ids,
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        "one response per request, none for the notification"
    );
    // The modern handshake: discover answers with the pinned era as
    // the only supported version, and serverInfo rides in `_meta`.
    assert_eq!(
        lines[0]["result"]["supportedVersions"],
        serde_json::json!(["2026-07-28"])
    );
    let info = &lines[0]["result"]["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(info["name"], "ache");
    assert_eq!(info["version"], VERSION);
    let names: Vec<&str> = lines[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "lovense_status",
            "lovense_toys",
            "lovense_arm",
            "lovense_disarm",
            "lovense_function",
            "lovense_pattern",
            "lovense_preset",
            "lovense_stop",
            "ache_devices",
            "ache_device",
        ]
    );
    // Status: mock, disarmed, nothing active.
    assert_eq!(lines[2]["result"]["isError"], false);
    assert_eq!(tool_text(&lines[2])["state"], "mock");
    assert_eq!(tool_text(&lines[2])["armed"], false);
    assert_eq!(tool_text(&lines[3])["count"], 2);
    // Output while disarmed: denied by the gate, code not_armed.
    assert_eq!(lines[4]["result"]["isError"], true);
    assert_eq!(tool_text(&lines[4])["error"]["code"], "not_armed");
    // Arm, then the over-ceiling function lands clamped at 20.
    assert_eq!(tool_text(&lines[5])["armed"], true);
    assert_eq!(lines[6]["result"]["isError"], false);
    assert_eq!(tool_text(&lines[6])["effective"]["strength"], 20);
    assert_eq!(tool_text(&lines[6])["gate"]["clamped"], true);
    assert_eq!(tool_text(&lines[6])["command"]["action"], "Vibrate:20");
    // Pattern and preset run; stop zeroes and latches off.
    assert_eq!(tool_text(&lines[7])["command"]["rule"], "V:1;F:v;S:500#");
    assert_eq!(tool_text(&lines[8])["command"]["name"], "pulse");
    assert_eq!(tool_text(&lines[9])["stop_sent"], true);
    assert_eq!(tool_text(&lines[10])["armed"], false);
    assert_eq!(tool_text(&lines[10])["output_active"], false);

    let transcript = format!(
        "== stdio smoke: ache, mock transport, no config file\n== requests\n{smoke}\
         == responses\n{}== stderr\n{}== result: PASS (server/discover, modern era; \
         tools/list = the ten tools; status mock/disarmed; disarmed function denied \
         not_armed; arm; function 25 clamped to 20; pattern; preset; stop; final \
         status disarmed and inactive)\n",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let evidence = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("stdio-smoke.txt");
    std::fs::write(evidence, transcript).expect("write smoke evidence");
}

/// The unified surface over the binary: mock mode wires the fixture
/// Buttplug provider, so one session discovers both providers'
/// devices, queries capabilities, runs a clamped function on a
/// Buttplug device through the same gate, and stops.
#[test]
fn stdio_unified_surface_over_mock_and_fixture() {
    let frames = [
        discover(1),
        call(2, "ache_devices", "{}"),
        call(3, "ache_device", r#"{"device":"buttplug:1"}"#),
        call(4, "lovense_arm", "{}"),
        call(
            5,
            "lovense_function",
            r#"{"feature":"Vibrate","strength":25,"time_sec":5,"device":"buttplug:0"}"#,
        ),
        call(
            6,
            "lovense_function",
            r#"{"feature":"Rotate","strength":5,"time_sec":5,"device":"buttplug:0"}"#,
        ),
        call(7, "lovense_stop", "{}"),
    ];
    let mut input = frames.join("\n");
    input.push('\n');
    let output = run(None, &input);
    assert!(output.status.success());
    let lines = responses(&output);
    assert_eq!(lines.len(), 7);
    // Unified discovery: two Lovense fixture toys + two Buttplug
    // fixture devices, each provider accounted for.
    let devices = tool_text(&lines[1]);
    assert_eq!(devices["count"], 4);
    assert_eq!(devices["providers"]["lovense"]["count"], 2);
    assert_eq!(devices["providers"]["buttplug"]["count"], 2);
    let ids: Vec<&str> = devices["devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|device| device["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "lovense:mock-toy-1",
            "lovense:mock-toy-2",
            "buttplug:0",
            "buttplug:1"
        ]
    );
    // Capability query: the Duo reports Vibrate + Rotate as data.
    let duo = tool_text(&lines[2]);
    assert_eq!(duo["name"], "Fixture Duo");
    let features: Vec<&str> = duo["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cap| cap["feature"].as_str().unwrap())
        .collect();
    assert_eq!(features, ["Vibrate", "Rotate"]);
    // A function on the Buttplug device passes the same gate: 25
    // clamps to the ceiling of 20 and the result says so.
    let ran = tool_text(&lines[4]);
    assert_eq!(lines[4]["result"]["isError"], false);
    assert_eq!(ran["provider"], "buttplug");
    assert_eq!(ran["gate"]["clamped"], true);
    assert_eq!(ran["effective"]["strength"], 20);
    // Rotate on the vibrate-only fixture is refused by the gate
    // against the device's own capabilities; nothing is executed.
    assert_eq!(lines[5]["result"]["isError"], true);
    assert_eq!(
        tool_text(&lines[5])["error"]["code"],
        "capability_unsupported"
    );
    // Stop zeroes both providers.
    assert_eq!(tool_text(&lines[6])["stop_sent"], true);
}

#[test]
fn valid_real_config_without_host_serves_unconfigured() {
    // No `arguments` key at all: the handlers accept it as before.
    let input = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{{\"name\":\"lovense_status\",\"_meta\":{META}}}}}\n\
         {{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{{\"name\":\"lovense_toys\",\"_meta\":{META}}}}}\n"
    );
    let output = run(Some("transport = \"real\"\n"), &input);
    assert!(output.status.success());
    let lines = responses(&output);
    assert_eq!(tool_text(&lines[0])["state"], "unconfigured");
    assert_eq!(lines[1]["result"]["isError"], true);
    assert_eq!(tool_text(&lines[1])["error"]["code"], "target_unconfigured");
}

// --- era refusal ---

#[test]
fn legacy_initialize_is_refused_and_the_server_survives() {
    let legacy = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"legacy","version":"0"}}}"#;
    let frames = [
        legacy.to_string(),
        discover(2),
        call(3, "lovense_status", "{}"),
    ];
    let mut input = frames.join("\n");
    input.push('\n');
    let output = run(None, &input);
    assert!(output.status.success());
    let lines = responses(&output);
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[0]["error"]["code"], -32022,
        "the legacy era is a designed refusal"
    );
    assert_eq!(
        lines[1]["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "ache"
    );
    assert_eq!(tool_text(&lines[2])["state"], "mock");
}

// --- shutdown paths ---

#[test]
fn eof_with_active_output_zeroes_and_reports_on_stderr() {
    let frames = [
        discover(1),
        call(2, "lovense_arm", "{}"),
        call(
            3,
            "lovense_function",
            r#"{"feature":"Vibrate","strength":7,"time_sec":30}"#,
        ),
    ];
    let mut input = frames.join("\n");
    input.push('\n');
    let output = run(None, &input);
    assert!(output.status.success());
    let lines = responses(&output);
    assert_eq!(lines.len(), 3);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ache: input closed with output active; stop sent, output zeroed"),
        "the shutdown zero is reported: {stderr}"
    );
}

#[test]
fn watchdog_trips_during_silence_at_the_binary_level() {
    let first = [
        discover(1),
        call(2, "lovense_arm", "{}"),
        call(
            3,
            "lovense_function",
            r#"{"feature":"Vibrate","strength":6,"time_sec":30}"#,
        ),
    ]
    .join("\n")
        + "\n";
    let second = call(4, "lovense_status", "{}") + "\n";
    let output = run_staged(
        Some("transport = \"mock\"\nwatchdog_secs = 1\n"),
        &[&first, &second],
        std::time::Duration::from_millis(1_800),
    );
    assert!(output.status.success());
    let lines = responses(&output);
    assert_eq!(lines.len(), 4);
    let status = tool_text(&lines[3]);
    assert_eq!(
        status["watchdog_trips"], 1,
        "the trip landed during the silence"
    );
    assert_eq!(status["output_active"], false);
    assert_eq!(status["armed"], false);
}

// --- adversarial ---

#[test]
fn invalid_configs_exit_2_with_structured_error_and_no_panic() {
    for text in [
        "intensity_ceiling = 21",
        "port = 0",
        "watchdog_secs = 0",
        "transport = \"cloud\"",
        "token = \"x\"",
        "host = \"http://evil/\"",
    ] {
        let output = run(Some(text), &smoke_session());
        assert_eq!(output.status.code(), Some(2), "config {text}");
        assert!(
            output.stdout.is_empty(),
            "nothing is served under a bad config"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let error: Value = serde_json::from_str(stderr.trim()).expect("stderr is one JSON error");
        assert_eq!(error["error"]["code"], "config_invalid", "config {text}");
        assert!(!stderr.contains("panicked"));
    }
}

#[test]
fn missing_config_file_exits_2() {
    let output = Command::new(SERVER)
        .env("ACHE_MCP_CONFIG", "/nonexistent/lovense-mcp.toml")
        .stdin(Stdio::null())
        .output()
        .expect("spawn server");
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stderr).expect("stderr is JSON");
    assert_eq!(error["error"]["code"], "config_missing");
}

#[test]
fn garbage_on_stdin_never_kills_the_server() {
    let tools_call = call(1, "lovense_toys", r#"{"x":1}"#);
    let ping = r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#.to_string();
    let input = format!("{{\n\u{7f}\u{1b}[2J\n[1,2,3]\n{tools_call}\n{ping}\n");
    let output = run(None, &input);
    assert!(output.status.success());
    let lines = responses(&output);
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0]["error"]["code"], -32700);
    assert_eq!(lines[1]["error"]["code"], -32700);
    assert_eq!(lines[2]["error"]["code"], -32600);
    assert_eq!(lines[3]["error"]["code"], -32602);
    assert_eq!(lines[4]["result"], serde_json::json!({}));
}
