//! Hand-rolled MCP stdio server: newline-delimited JSON-RPC 2.0, protocol
//! era 2024-11-05. Exactly two read-only tools. Protocol misuse (unknown
//! method or tool, bad arguments) is a JSON-RPC error; an operational
//! failure (no target, unreachable app) is a tool result with
//! `isError: true` and the structured error as its text.

use std::io::{self, BufRead, Read, Write};

use serde_json::{Map, Value, json};

use crate::config::{Config, TransportKind};
use crate::error::{ErrorCode, LovenseError, truncate};
use crate::transport::ReadTransport;

pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "lovense";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Longest accepted input line, in bytes. Longer lines are discarded
/// unread and answered with an invalid-request error.
pub const LINE_BYTES_MAX: usize = 1024 * 1024;
/// Most unexpected argument names echoed back in a refusal.
const ARGUMENT_NAMES_REPORTED_MAX: usize = 8;
const ARGUMENT_NAME_BYTES_MAX: usize = 64;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

pub struct Server {
    config: Config,
    /// `None` exactly when the config names the real transport without a
    /// host. Then no transport exists and nothing can reach the wire.
    transport: Option<Box<dyn ReadTransport>>,
}

impl Server {
    pub fn new(config: Config, transport: Option<Box<dyn ReadTransport>>) -> Server {
        Server { config, transport }
    }

    /// Handle one input line. Returns the response, or `None` for a
    /// notification (a message without `id`), which is never answered.
    pub fn handle_line(&self, line: &[u8]) -> Option<Value> {
        let message: Value = match serde_json::from_slice(line) {
            Ok(message) => message,
            Err(cause) => return Some(rpc_error(Value::Null, PARSE_ERROR, cause.to_string())),
        };
        let Some(object) = message.as_object() else {
            return Some(rpc_error(
                Value::Null,
                INVALID_REQUEST,
                "message must be an object",
            ));
        };
        let id = match object.get("id") {
            None => return None,
            Some(id @ (Value::String(_) | Value::Number(_))) => id.clone(),
            Some(_) => {
                return Some(rpc_error(
                    Value::Null,
                    INVALID_REQUEST,
                    "id must be string or number",
                ));
            }
        };
        if object.get("jsonrpc") != Some(&json!("2.0")) {
            return Some(rpc_error(id, INVALID_REQUEST, "jsonrpc must be \"2.0\""));
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return Some(rpc_error(id, INVALID_REQUEST, "method must be a string"));
        };
        let outcome = match method {
            "initialize" => Ok(initialize_result()),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_list() })),
            "tools/call" => self.call_tool(object.get("params")),
            _ => Err((
                METHOD_NOT_FOUND,
                format!("unknown method `{}`", clip(method)),
            )),
        };
        Some(match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => rpc_error(id, code, message),
        })
    }

    fn call_tool(&self, params: Option<&Value>) -> Result<Value, (i64, String)> {
        let Some(params) = params.and_then(Value::as_object) else {
            return Err((INVALID_PARAMS, "params must be an object".into()));
        };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Err((INVALID_PARAMS, "params.name must be a string".into()));
        };
        if name != "lovense_status" && name != "lovense_toys" {
            return Err((INVALID_PARAMS, format!("unknown tool `{}`", clip(name))));
        }
        check_no_arguments(params.get("arguments"))?;
        let outcome = if name == "lovense_status" {
            Ok(self.status())
        } else {
            self.toys()
        };
        Ok(match outcome {
            Ok(value) => tool_result(&value, false),
            Err(error) => tool_result(&error.to_json(), true),
        })
    }

    fn status(&self) -> Value {
        let probe = self.transport.as_ref().map(|transport| transport.probe());
        let (state, note) = match (self.config.transport, &probe) {
            (TransportKind::Mock, _) => (
                "mock",
                "mock transport: fixture toys only; no device or app is contacted",
            ),
            (TransportKind::Real, None) => (
                "unconfigured",
                "real transport with no host configured; nothing can reach the wire",
            ),
            (TransportKind::Real, Some(_)) => (
                "ready",
                "target configured but not verified; lovense_toys contacts it",
            ),
        };
        let target = probe.and_then(|probe| probe.target);
        json!({
            "state": state,
            "transport": self.config.transport.as_str(),
            "target": target,
            "intensity_ceiling": self.config.intensity_ceiling,
            "watchdog_secs": self.config.watchdog_secs,
            "output_path": "none: stage S1 is read-only; ceiling and watchdog bind output from S2",
            "note": note,
            "server": { "name": SERVER_NAME, "version": SERVER_VERSION },
        })
    }

    fn toys(&self) -> Result<Value, LovenseError> {
        // Gate on the config first, so a real transport without a host can
        // never reach any transport, whatever was injected.
        let unconfigured =
            self.config.transport == TransportKind::Real && self.config.host.is_empty();
        let transport = match &self.transport {
            Some(transport) if !unconfigured => transport,
            _ => {
                return Err(LovenseError::new(
                    ErrorCode::TargetUnconfigured,
                    "transport is real but no host is configured; no request was made",
                ));
            }
        };
        let toys = transport.discover()?;
        let source = match self.config.transport {
            TransportKind::Mock => "mock_fixture",
            TransportKind::Real => "live_discovery",
        };
        Ok(json!({
            "transport": self.config.transport.as_str(),
            "source": source,
            "count": toys.len(),
            "toys": toys,
        }))
    }
}

/// Serve until end of input. Each line in is at most one response out.
pub fn serve(server: &Server, mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        let limit = u64::try_from(LINE_BYTES_MAX + 1).expect("line limit fits u64");
        let read = (&mut input).take(limit).read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(());
        }
        let response = if line.len() > LINE_BYTES_MAX && line.last() != Some(&b'\n') {
            input.skip_until(b'\n')?;
            Some(rpc_error(
                Value::Null,
                INVALID_REQUEST,
                format!("line exceeds {LINE_BYTES_MAX} bytes"),
            ))
        } else if line.iter().all(u8::is_ascii_whitespace) {
            None
        } else {
            server.handle_line(&line)
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
        "instructions": "lovense — Lovense local Game Mode, stage S1 read core. \
            Tools: lovense_status, lovense_toys. There is no output path in this stage.",
    })
}

fn tool_list() -> Value {
    let no_arguments = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    json!([
        {
            "name": "lovense_status",
            "description": "Report the server state (unconfigured, mock, or ready), transport, \
                target, intensity ceiling, watchdog window, and version. Makes no network \
                call; `ready` means a target is configured, not that it answered.",
            "inputSchema": no_arguments,
            "annotations": {
                "title": "Lovense status",
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false,
            },
        },
        {
            "name": "lovense_toys",
            "description": "Discover connected toys. Mock transport: the fixture toys. Real \
                transport: one GetToys request to the configured app; an unreachable app is \
                an error, never an empty list. Real with no host: target_unconfigured, and \
                no request is made.",
            "inputSchema": no_arguments,
            "annotations": {
                "title": "Lovense toys",
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true,
            },
        },
    ])
}

/// Both tools take no arguments. Absent, null, and `{}` are accepted;
/// any field is refused, naming (a bounded list of) the fields.
fn check_no_arguments(arguments: Option<&Value>) -> Result<(), (i64, String)> {
    match arguments {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Object(fields)) if fields.is_empty() => Ok(()),
        Some(Value::Object(fields)) => Err((INVALID_PARAMS, unexpected_fields(fields))),
        Some(_) => Err((INVALID_PARAMS, "arguments must be an object".into())),
    }
}

fn unexpected_fields(fields: &Map<String, Value>) -> String {
    let names: Vec<String> = fields
        .keys()
        .take(ARGUMENT_NAMES_REPORTED_MAX)
        .map(|name| format!("`{}`", clip(name)))
        .collect();
    format!(
        "unexpected argument fields: {}; this tool takes none",
        names.join(", ")
    )
}

fn tool_result(value: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|cause| {
        json!({ "error": { "code": "output_write_failed", "message": cause.to_string() } })
            .to_string()
    });
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    let message = truncate(message.into());
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn clip(text: &str) -> &str {
    let mut cut = text.len().min(ARGUMENT_NAME_BYTES_MAX);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_stub::{AppStub, closed_port};
    use crate::transport::{MockTransport, ReadRequest, from_config};

    fn server_for(config: Config) -> Server {
        let transport = from_config(&config);
        Server::new(config, transport)
    }

    fn real(host: &str, port: u16) -> Config {
        Config {
            transport: TransportKind::Real,
            host: host.into(),
            port,
            ..Config::default()
        }
    }

    fn request(server: &Server, line: &str) -> Value {
        server
            .handle_line(line.as_bytes())
            .expect("a request is answered")
    }

    fn call(server: &Server, name: &str, arguments: Value) -> Value {
        let line = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}});
        request(server, &line.to_string())
    }

    /// The tool payload and its isError flag.
    fn payload(response: &Value) -> (Value, bool) {
        let result = &response["result"];
        let text = result["content"][0]["text"].as_str().expect("text content");
        (
            serde_json::from_str(text).unwrap(),
            result["isError"].as_bool().unwrap(),
        )
    }

    // --- validation ---

    #[test]
    fn initialize_reports_legacy_era_and_server_info() {
        let response = request(
            &server_for(Config::default()),
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        );
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(
            response["result"]["serverInfo"],
            json!({"name": "lovense", "version": "0.1.0"})
        );
        assert_eq!(response["result"]["capabilities"], json!({"tools": {}}));
    }

    #[test]
    fn tools_list_is_exactly_two_read_only_tools_with_full_schemas() {
        let response = request(
            &server_for(Config::default()),
            r#"{"jsonrpc":"2.0","id":"a","method":"tools/list"}"#,
        );
        let tools = response["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["lovense_status", "lovense_toys"]);
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(tool["inputSchema"]["properties"].is_object());
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert_eq!(tool["annotations"]["readOnlyHint"], true);
            assert_eq!(tool["annotations"]["destructiveHint"], false);
        }
    }

    #[test]
    fn mock_status_is_labeled_mock_never_connected() {
        let (status, is_error) = payload(&call(
            &server_for(Config::default()),
            "lovense_status",
            json!({}),
        ));
        assert!(!is_error);
        assert_eq!(status["state"], "mock");
        assert_eq!(status["transport"], "mock");
        assert_eq!(status["target"], Value::Null);
        assert_eq!(status["intensity_ceiling"], 20);
        assert_eq!(status["watchdog_secs"], 5);
        assert_eq!(status["server"]["version"], "0.1.0");
        assert!(!status.to_string().contains("connected"));
    }

    #[test]
    fn real_status_reports_unconfigured_or_ready_with_target() {
        let (status, _) = payload(&call(
            &server_for(real("", 20010)),
            "lovense_status",
            Value::Null,
        ));
        assert_eq!(status["state"], "unconfigured");
        assert_eq!(status["target"], Value::Null);
        let config = Config {
            use_https: true,
            intensity_ceiling: 7,
            watchdog_secs: 9,
            ..real("10.1.2.3", 30010)
        };
        let (status, _) = payload(&call(&server_for(config), "lovense_status", json!({})));
        assert_eq!(status["state"], "ready");
        assert_eq!(
            status["target"],
            json!({"scheme": "https", "host": "10.1.2.3", "port": 30010})
        );
        assert_eq!(
            (
                status["intensity_ceiling"].clone(),
                status["watchdog_secs"].clone()
            ),
            (json!(7), json!(9))
        );
    }

    #[test]
    fn mock_toys_returns_the_fixture() {
        let (toys, is_error) = payload(&call(
            &server_for(Config::default()),
            "lovense_toys",
            json!({}),
        ));
        assert!(!is_error);
        assert_eq!(toys["source"], "mock_fixture");
        assert_eq!(toys["count"], 2);
        assert_eq!(toys["toys"][0]["type"], "lush");
        assert_eq!(toys["toys"][1]["battery"], 42);
    }

    #[test]
    fn real_toys_runs_live_discovery_against_the_stub() {
        let mut stub = AppStub::start(
            r#"{"code":200,"data":{"toys":"{\"z\":{\"name\":\"nora\"}}"}}"#,
            1,
            200,
        );
        let (toys, is_error) = payload(&call(
            &server_for(real("127.0.0.1", stub.port)),
            "lovense_toys",
            json!({}),
        ));
        assert!(!is_error);
        assert_eq!(toys["source"], "live_discovery");
        assert_eq!(toys["toys"][0]["name"], "nora");
        assert_eq!(stub.received()["body"], r#"{"command":"GetToys"}"#);
    }

    #[test]
    fn ping_answers_and_notifications_get_no_response() {
        let server = server_for(Config::default());
        assert_eq!(
            request(&server, r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#)["result"],
            json!({})
        );
        assert!(
            server
                .handle_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_none()
        );
        assert!(
            server
                .handle_line(br#"{"jsonrpc":"2.0","method":"no/such"}"#)
                .is_none()
        );
    }

    // --- adversarial ---

    #[test]
    fn real_with_empty_host_makes_no_network_call() {
        // Inject a recording transport under a real/no-host config: the
        // config gate must refuse before any transport is touched.
        let mock = MockTransport::new();
        let log = mock.request_log();
        let server = Server::new(real("", 20010), Some(Box::new(mock)));
        let (error, is_error) = payload(&call(&server, "lovense_toys", json!({})));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "target_unconfigured");
        assert!(log.lock().unwrap().is_empty());
        // And production wiring builds no transport at all for it.
        assert!(from_config(&real("", 20010)).is_none());
        let (error, _) = payload(&call(
            &server_for(real("", 20010)),
            "lovense_toys",
            json!({}),
        ));
        assert_eq!(error["error"]["code"], "target_unconfigured");
    }

    #[test]
    fn unreachable_target_is_a_structured_tool_error() {
        let (error, is_error) = payload(&call(
            &server_for(real("127.0.0.1", closed_port())),
            "lovense_toys",
            json!({}),
        ));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "target_unreachable");
        assert_eq!(error["error"]["class"], "transport");
        assert!(error.get("toys").is_none());
    }

    #[test]
    fn stub_malformed_json_is_a_structured_tool_error() {
        let mut stub = AppStub::start("<html>nope", 1, 200);
        let (error, is_error) = payload(&call(
            &server_for(real("127.0.0.1", stub.port)),
            "lovense_toys",
            json!({}),
        ));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "response_invalid");
        stub.received();
    }

    #[test]
    fn unknown_tool_is_refused_and_touches_nothing() {
        let mock = MockTransport::new();
        let log = mock.request_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        for name in [
            "lovense_stop",
            "lovense_arm",
            "lovense_function",
            "LOVENSE_TOYS",
            "",
        ] {
            let response = call(&server, name, json!({}));
            assert_eq!(response["error"]["code"], INVALID_PARAMS, "tool {name}");
            assert!(
                response["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("unknown tool")
            );
        }
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn extra_argument_fields_are_refused_before_any_transport_call() {
        let mock = MockTransport::new();
        let log = mock.request_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        for tool in ["lovense_status", "lovense_toys"] {
            let response = call(&server, tool, json!({"strength": 20, "host": "evil"}));
            assert_eq!(response["error"]["code"], INVALID_PARAMS);
            let message = response["error"]["message"].as_str().unwrap();
            assert!(
                message.contains("`strength`") && message.contains("`host`"),
                "{message}"
            );
            for bad in [json!([]), json!("x"), json!(1)] {
                assert_eq!(call(&server, tool, bad)["error"]["code"], INVALID_PARAMS);
            }
        }
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn malformed_json_rpc_is_answered_with_protocol_errors() {
        let server = server_for(Config::default());
        let cases: &[(&str, i64)] = &[
            ("{not json", PARSE_ERROR),
            ("\u{0}\u{1}", PARSE_ERROR),
            ("[]", INVALID_REQUEST),
            (
                r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
                INVALID_REQUEST,
            ),
            ("42", INVALID_REQUEST),
            (
                r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
                INVALID_REQUEST,
            ),
            (r#"{"id":1,"method":"ping"}"#, INVALID_REQUEST),
            (
                r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
                INVALID_REQUEST,
            ),
            (
                r#"{"jsonrpc":"2.0","id":{},"method":"ping"}"#,
                INVALID_REQUEST,
            ),
            (r#"{"jsonrpc":"2.0","id":1,"method":7}"#, INVALID_REQUEST),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/destroy"}"#,
                METHOD_NOT_FOUND,
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#,
                INVALID_PARAMS,
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":[]}"#,
                INVALID_PARAMS,
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":3}}"#,
                INVALID_PARAMS,
            ),
        ];
        for (line, code) in cases {
            assert_eq!(
                request(&server, line)["error"]["code"],
                *code,
                "line {line}"
            );
        }
    }

    #[test]
    fn serve_bounds_lines_and_survives_garbage() {
        let server = server_for(Config::default());
        let mut input = Vec::new();
        input.extend_from_slice(&vec![b'x'; LINE_BYTES_MAX + 10]);
        input.extend_from_slice(b"\n\n   \n{oops\n");
        input.extend_from_slice(br#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#);
        let mut output = Vec::new();
        serve(&server, input.as_slice(), &mut output).unwrap();
        let lines: Vec<Value> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(
            lines.len(),
            3,
            "oversized, parse error, ping; blank lines unanswered"
        );
        assert_eq!(lines[0]["error"]["code"], INVALID_REQUEST);
        assert_eq!(lines[1]["error"]["code"], PARSE_ERROR);
        assert_eq!(lines[2]["id"], 5);
    }

    #[test]
    fn s1_has_no_output_path_in_source() {
        // Production code only: each file up to its test module.
        let sources = [
            include_str!("transport.rs"),
            include_str!("server.rs"),
            include_str!("main.rs"),
        ];
        let words = [
            "Vibrate",
            "Function",
            "Pattern",
            "Preset",
            "fn arm",
            "fn stop",
            "fn disarm",
        ];
        for source in sources {
            let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
            for word in words {
                assert!(
                    !production.contains(word),
                    "output-path word `{word}` in S1 source"
                );
            }
        }
        let tools = tool_list().to_string();
        for word in ["function", "pattern", "preset", "arm", "stop"] {
            assert!(!tools.contains(&format!("lovense_{word}")));
        }
    }

    #[test]
    fn transport_is_probed_for_status_and_not_discovered() {
        let mock = MockTransport::new();
        let log = mock.request_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        call(&server, "lovense_status", json!({}));
        assert_eq!(*log.lock().unwrap(), vec![ReadRequest::Probe]);
    }
}
