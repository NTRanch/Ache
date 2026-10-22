//! Binary-level stdio tests: spawn the built server, speak JSON-RPC over
//! its pipes. The smoke writes its transcript to
//! `$CARGO_TARGET_TMPDIR/stdio-smoke.txt` as evidence.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const SERVER: &str = env!("CARGO_BIN_EXE_lovense-mcp");

/// Run the server with `config` (None = no config file), feed `input`,
/// and collect everything it printed.
fn run(config: Option<&str>, input: &str) -> Output {
    let mut command = Command::new(SERVER);
    command.env_remove("LOVENSE_MCP_CONFIG");
    if let Some(text) = config {
        let path = config_file(text);
        command.env("LOVENSE_MCP_CONFIG", path);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn server");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    // The server may exit before reading (bad config); a broken pipe is fine.
    let _ = stdin.write_all(input.as_bytes());
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

const SMOKE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"lovense_status","arguments":{}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"lovense_toys","arguments":{}}}
"#;

// --- validation ---

#[test]
fn stdio_smoke_over_mock_transport() {
    let output = run(None, SMOKE);
    assert!(output.status.success());
    let lines = responses(&output);
    let ids: Vec<i64> = lines
        .iter()
        .map(|line| line["id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        ids,
        [1, 2, 3, 4],
        "one response per request, none for the notification"
    );
    assert_eq!(lines[0]["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], "lovense");
    assert_eq!(lines[0]["result"]["serverInfo"]["version"], "0.1.0");
    let names: Vec<&str> = lines[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["lovense_status", "lovense_toys"]);
    assert_eq!(lines[2]["result"]["isError"], false);
    assert_eq!(tool_text(&lines[2])["state"], "mock");
    assert_eq!(lines[3]["result"]["isError"], false);
    assert_eq!(tool_text(&lines[3])["count"], 2);

    let transcript = format!(
        "== stdio smoke: lovense-mcp, mock transport, no config file\n== requests\n{SMOKE}\
         == responses\n{}== stderr\n{}== result: PASS (initialize; tools/list = exactly \
         lovense_status, lovense_toys; lovense_status state=mock; lovense_toys 2 fixture toys)\n",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let evidence = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("stdio-smoke.txt");
    std::fs::write(evidence, transcript).expect("write smoke evidence");
}

#[test]
fn valid_real_config_without_host_serves_unconfigured() {
    let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"lovense_status"}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"lovense_toys"}}
"#;
    let output = run(Some("transport = \"real\"\n"), input);
    assert!(output.status.success());
    let lines = responses(&output);
    assert_eq!(tool_text(&lines[0])["state"], "unconfigured");
    assert_eq!(lines[1]["result"]["isError"], true);
    assert_eq!(tool_text(&lines[1])["error"]["code"], "target_unconfigured");
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
        let output = run(Some(text), SMOKE);
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
        .env("LOVENSE_MCP_CONFIG", "/nonexistent/lovense-mcp.toml")
        .stdin(Stdio::null())
        .output()
        .expect("spawn server");
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stderr).expect("stderr is JSON");
    assert_eq!(error["error"]["code"], "config_missing");
}

#[test]
fn garbage_on_stdin_never_kills_the_server() {
    let input = "{\n\u{7f}\u{1b}[2J\n[1,2,3]\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\
                 \"params\":{\"name\":\"lovense_toys\",\"arguments\":{\"x\":1}}}\n\
                 {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n";
    let output = run(None, input);
    assert!(output.status.success());
    let lines = responses(&output);
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0]["error"]["code"], -32700);
    assert_eq!(lines[1]["error"]["code"], -32700);
    assert_eq!(lines[2]["error"]["code"], -32600);
    assert_eq!(lines[3]["error"]["code"], -32602);
    assert_eq!(lines[4]["result"], serde_json::json!({}));
}
