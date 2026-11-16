//! The facade the UI programs against: a narrow trait over the ache
//! server, plus the real MCP stdio client and the scripted fake used
//! by every test.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::CEILING_MAX;

/// One parsed view of the server's `lovense_status` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateStatus {
    /// Master arm state exactly as the server reported it.
    pub armed: bool,
    /// Whether any output is currently active.
    pub output_active: bool,
    /// Ceiling as reported, before clamping.
    pub ceiling_reported: u64,
    /// Ceiling to display: reported value clamped to `CEILING_MAX`.
    pub ceiling_display: u8,
    /// True when the server reported a ceiling above `CEILING_MAX`.
    pub ceiling_anomaly: bool,
    /// Watchdog window in seconds, as reported.
    pub watchdog_secs: u64,
    /// Number of watchdog trips, as reported.
    pub watchdog_trips: u64,
    /// Session health string, as reported.
    pub session_health: String,
    /// Server name/version string.
    pub server_label: String,
    /// Per-provider `(id, state, available, note)` rows.
    pub providers: Vec<(String, String, bool, String)>,
    /// Server API opt-in: true only when the serverapi provider
    /// reports itself enabled/ready. A disabled provider is inert.
    pub server_api_enabled: bool,
}

/// Status as the UI may know it. `Unknown` is the only honest state
/// once the server cannot be reached: last-known values are dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusView {
    /// The server answered; these values are current.
    Known(GateStatus),
    /// The server did not answer; nothing about the gate is known.
    Unknown(String),
}

/// A typed facade failure. The UI renders these; it never retries a
/// call that the server's gate refused (see `is_gate_refusal`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacadeError {
    /// Short machine-stable kind string.
    pub kind: String,
    /// Bounded human-readable detail.
    pub message: String,
}

impl FacadeError {
    /// Build an error from a kind and detail.
    pub fn new(kind: &str, message: impl Into<String>) -> FacadeError {
        FacadeError {
            kind: kind.to_string(),
            message: message.into(),
        }
    }
    /// True when the server answered with an error result — for an
    /// output tool that is the `decide()` gate refusing. Refusals are
    /// final: the TUI renders them and never retries around them.
    pub fn is_gate_refusal(&self) -> bool {
        self.kind == "gate_refused" || self.kind == "tool_error"
    }
}

impl std::fmt::Display for FacadeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}
impl std::error::Error for FacadeError {}

/// Parse one `lovense_status` payload into a [`GateStatus`].
/// A ceiling above [`CEILING_MAX`] is clamped for display and
/// flagged; the reported value is preserved for the audit log.
pub fn parse_status(value: &Value) -> Result<GateStatus, FacadeError> {
    let armed = value["armed"]
        .as_bool()
        .ok_or_else(|| FacadeError::new("malformed_status", "status missing bool `armed`"))?;
    let ceiling_reported = value["intensity_ceiling"].as_u64().ok_or_else(|| {
        FacadeError::new("malformed_status", "status missing `intensity_ceiling`")
    })?;
    let ceiling_anomaly = ceiling_reported > u64::from(CEILING_MAX);
    let ceiling_display = u8::try_from(ceiling_reported.min(u64::from(CEILING_MAX)))
        .map_err(|_| FacadeError::new("malformed_status", "ceiling conversion failed"))?;
    let mut providers = Vec::new();
    if let Some(map) = value["providers"].as_object() {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        for key in keys {
            let row = &map[key];
            providers.push((
                key.clone(),
                row["state"].as_str().unwrap_or("?").to_string(),
                row["available"].as_bool().unwrap_or(false),
                row["note"].as_str().unwrap_or("").to_string(),
            ));
        }
    }
    let server_api_enabled = providers.iter().any(|(id, state, available, note)| {
        id == "serverapi" && *available && (state == "ready" || note.contains("enabled"))
    });
    let server_label = format!(
        "{} {}",
        value["server"]["name"].as_str().unwrap_or("ache"),
        value["server"]["version"].as_str().unwrap_or("?")
    );
    Ok(GateStatus {
        armed,
        output_active: value["output_active"].as_bool().unwrap_or(false),
        ceiling_reported,
        ceiling_display,
        ceiling_anomaly,
        watchdog_secs: value["watchdog_secs"].as_u64().unwrap_or(0),
        watchdog_trips: value["watchdog_trips"].as_u64().unwrap_or(0),
        session_health: value["session_health"]
            .as_str()
            .unwrap_or("unknown")
            .to_string(),
        server_label,
        providers,
        server_api_enabled,
    })
}

/// The narrow surface the UI uses. All methods go to the server;
/// none is answered from a TUI-side cache.
pub trait Facade {
    /// Fresh `lovense_status`, parsed.
    fn status(&mut self) -> Result<GateStatus, FacadeError>;
    /// The server's tool list (`tools/list`).
    fn tools(&mut self) -> Result<Value, FacadeError>;
    /// One tool call. Error results become typed errors.
    fn call(&mut self, tool: &str, args: Value) -> Result<Value, FacadeError>;
    /// Unified device list (`ache_devices`).
    fn devices(&mut self) -> Result<Value, FacadeError> {
        self.call("ache_devices", json!({}))
    }
    /// Synchronous stop: the server's real `lovense_stop`, which
    /// zeroes every provider and latches the master off.
    fn stop(&mut self) -> Result<Value, FacadeError> {
        self.call("lovense_stop", json!({}))
    }
}

// ------------------------------------------------------------ stdio

const PROTOCOL_VERSION: &str = "2026-07-28";
const RESPONSE_WAIT: Duration = Duration::from_secs(10);

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": {"name": "ache-tui", "version": "0.1.0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

/// The real facade: an MCP stdio client spawning the ache server
/// binary and speaking modern-era (2026-07-28) NDJSON JSON-RPC,
/// the same framing the server's own stdio smoke test uses.
/// The child owns no credentials: the TUI passes no config and no
/// secrets; the server resolves its own configuration.
pub struct StdioFacade {
    child: Child,
    stdin: ChildStdin,
    replies: mpsc::Receiver<String>,
    next_id: i64,
    discovered: bool,
}

impl StdioFacade {
    /// Spawn `server_bin` with piped stdio. The reader thread owns
    /// stdout and forwards lines; EOF on stdout ends the stream,
    /// which later calls observe as a dead server.
    pub fn spawn(server_bin: &str) -> Result<StdioFacade, FacadeError> {
        let mut child = Command::new(server_bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| FacadeError::new("spawn_failed", e.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| FacadeError::new("spawn_failed", "child stdin missing"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| FacadeError::new("spawn_failed", "child stdout missing"))?;
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(text) => {
                        if tx.send(text).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(StdioFacade {
            child,
            stdin,
            replies: rx,
            next_id: 0,
            discovered: false,
        })
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, FacadeError> {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut text = serde_json::to_string(&frame)
            .map_err(|e| FacadeError::new("encode_failed", e.to_string()))?;
        text.push('\n');
        self.stdin
            .write_all(text.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|e| FacadeError::new("server_gone", format!("write failed: {e}")))?;
        let deadline = std::time::Instant::now() + RESPONSE_WAIT;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(FacadeError::new("timeout", "server did not answer in time"));
            }
            let line = self
                .replies
                .recv_timeout(remaining)
                .map_err(|_| FacadeError::new("server_gone", "server stdout closed"))?;
            let value: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if value["id"].as_i64() == Some(id) {
                if !value["error"].is_null() {
                    return Err(FacadeError::new(
                        "gate_refused",
                        value["error"]["message"]
                            .as_str()
                            .unwrap_or("server returned an error")
                            .to_string(),
                    ));
                }
                return Ok(value["result"].clone());
            }
        }
    }

    fn ensure_discovered(&mut self) -> Result<(), FacadeError> {
        if !self.discovered {
            self.request("server/discover", json!({"_meta": meta()}))?;
            self.discovered = true;
        }
        Ok(())
    }

    /// Whether the spawned server process has exited.
    pub fn server_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }
}

impl Facade for StdioFacade {
    fn status(&mut self) -> Result<GateStatus, FacadeError> {
        let raw = self.call("lovense_status", json!({}))?;
        parse_status(&raw)
    }

    fn tools(&mut self) -> Result<Value, FacadeError> {
        self.ensure_discovered()?;
        self.request("tools/list", json!({"_meta": meta()}))
    }

    fn call(&mut self, tool: &str, args: Value) -> Result<Value, FacadeError> {
        self.ensure_discovered()?;
        let result = self.request(
            "tools/call",
            json!({"name": tool, "arguments": args, "_meta": meta()}),
        )?;
        // MCP tool results carry the payload as JSON text content.
        if let Some(text) = result["content"][0]["text"].as_str() {
            let payload: Value = serde_json::from_str(text)
                .map_err(|e| FacadeError::new("malformed_result", e.to_string()))?;
            if result["isError"].as_bool() == Some(true) {
                return Err(FacadeError::new(
                    "gate_refused",
                    payload["error"]["message"]
                        .as_str()
                        .unwrap_or("tool refused")
                        .to_string(),
                ));
            }
            return Ok(payload);
        }
        Ok(result)
    }
}

// -------------------------------------------------------------- fake

/// One scripted reply for [`FakeFacade`].
#[derive(Debug, Clone)]
pub enum Reply {
    /// A successful payload.
    Ok(Value),
    /// A typed failure (gate refusal, dead server, ...).
    Err(FacadeError),
}

/// In-memory scripted facade for tests: records every call, answers
/// status from a queue (so tests can script arm-state changes and
/// server death), and never touches a process or a device.
#[derive(Debug, Default)]
pub struct FakeFacade {
    /// Every `(tool, args)` call, in order.
    pub calls: Vec<(String, Value)>,
    status_queue: VecDeque<Reply>,
    call_replies: VecDeque<Reply>,
    tools_value: Value,
}

impl FakeFacade {
    /// A fake whose status calls answer from `queue` in order; when
    /// the queue empties, status fails as a dead server.
    pub fn with_status(queue: Vec<Reply>) -> FakeFacade {
        FakeFacade {
            status_queue: queue.into(),
            tools_value: json!({"tools": []}),
            ..FakeFacade::default()
        }
    }
    /// Queue one reply for the next non-status tool call.
    pub fn push_call_reply(&mut self, reply: Reply) {
        self.call_replies.push_back(reply);
    }
    /// How many times `tool` was called.
    pub fn call_count(&self, tool: &str) -> usize {
        self.calls.iter().filter(|(name, _)| name == tool).count()
    }
}

impl Facade for FakeFacade {
    fn status(&mut self) -> Result<GateStatus, FacadeError> {
        self.calls.push(("lovense_status".to_string(), json!({})));
        match self.status_queue.pop_front() {
            Some(Reply::Ok(value)) => parse_status(&value),
            Some(Reply::Err(error)) => Err(error),
            None => Err(FacadeError::new("server_gone", "fake: no scripted status")),
        }
    }

    fn tools(&mut self) -> Result<Value, FacadeError> {
        self.calls.push(("tools/list".to_string(), json!({})));
        Ok(self.tools_value.clone())
    }

    fn call(&mut self, tool: &str, args: Value) -> Result<Value, FacadeError> {
        self.calls.push((tool.to_string(), args));
        if tool == "lovense_status" {
            return match self.status_queue.pop_front() {
                Some(Reply::Ok(value)) => Ok(value),
                Some(Reply::Err(error)) => Err(error),
                None => Err(FacadeError::new("server_gone", "fake: no scripted status")),
            };
        }
        match self.call_replies.pop_front() {
            Some(Reply::Ok(value)) => Ok(value),
            Some(Reply::Err(error)) => Err(error),
            None => Ok(json!({"ok": true})),
        }
    }
}

/// Build a well-formed status payload for tests and demos.
pub fn status_json(armed: bool, ceiling: u64) -> Value {
    json!({
        "armed": armed,
        "output_active": false,
        "intensity_ceiling": ceiling,
        "watchdog_secs": 5,
        "watchdog_trips": 0,
        "session_health": "ok",
        "server": {"name": "ache", "version": "1.2.0"},
        "providers": {
            "lovense": {"provider": "lovense", "state": "mock", "available": true, "note": "mock transport"},
            "buttplug": {"provider": "buttplug", "state": "ready", "available": true, "note": "embedded"},
            "serverapi": {"provider": "serverapi", "state": "disabled", "available": false, "note": "server api disabled: opt-in is off"}
        }
    })
}
