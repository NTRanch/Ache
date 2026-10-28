//! Byte-level era guard in front of the rmcp SDK.
//!
//! The modern era (2026-07-28) has no `initialize` handshake: clients
//! open with `server/discover` carrying their era in the request
//! `_meta`. rmcp 3.8.0 has no server-side era pin — it accepts a
//! legacy `initialize`, silently drops lines it cannot parse, and
//! silently drops oversize lines. The pre-migration server instead
//! answered every malformed line with the baseline's exact error
//! frames. This guard restores that contract at the byte level: it
//! classifies each client line, forwards only `server/discover`,
//! `tools/list`, and shape-valid `tools/call` frames to the SDK over
//! a duplex, and answers everything else itself with the baseline's
//! codes and messages (legacy `initialize` gets the typed modern-era
//! refusal, -32022).
//!
//! The guard is also the sequencer. The baseline read and answered
//! one line at a time; ache's tool calls do not commute (a pipelined
//! `arm` then `function` must apply in wire order). rmcp dispatches
//! concurrently, so the guard forwards a single request at a time,
//! in arrival order, queueing later requests and notifications until
//! the in-flight response has passed. Guard-answered frames
//! likewise wait until every earlier request has been answered, so
//! the response stream matches the baseline's order exactly.
//!
//! One baseline behavior is deliberately not reproduced: a
//! notification-shaped `tools/call` (no `id`) was executed by the
//! baseline despite having nowhere to send its result. The guard
//! drops id-less frames unexecuted, as WS2 Wave 1 established for
//! the fleet.

use std::collections::VecDeque;
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf,
    WriteHalf,
};
use tokio::sync::{Notify, oneshot};

use crate::server;

/// Stdout shared between the guard's own frames and the response
/// pump, so the two writers can never interleave mid-line.
pub type SharedStdout = Arc<Mutex<std::io::Stdout>>;

/// Bound on one client line, matching the WS1 fleet template. The
/// baseline bounded lines at 1 MiB and answered -32600; see the
/// migration gate report for the accepted framing delta.
pub const REQUEST_BYTES_MAX: usize = 4 * 1024 * 1024;

/// How long a guard-answered frame waits for earlier responses
/// before writing anyway (the WS1 ordering bound: never deadlock the
/// stream on a wedged handler).
const DRAIN_WAIT_MAX: Duration = Duration::from_secs(5);

/// JSON-RPC error code: message is not valid JSON.
const PARSE_ERROR: i64 = -32700;
/// JSON-RPC error code: message is not a valid request object.
const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC error code: the method does not exist.
const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC error code: invalid method parameter(s).
const INVALID_PARAMS: i64 = -32602;
/// Modern-era code: the client asked for a legacy protocol era.
const ERA_MISMATCH: i64 = -32022;

/// What the guard decided to do with one client line.
#[derive(Debug, PartialEq)]
enum Verdict {
    /// Drop the line: never answered, never executed.
    Drop,
    /// Forward the raw line to the SDK.
    Forward,
    /// Answer from the guard with this JSON-RPC error frame.
    Reply(Value),
}

/// Shared sequencing state between the guard's read loop and the
/// response pump: which request ids are still owed a response (in
/// arrival order, queued or in flight), which one is in flight, and
/// whether the client stream has ended.
struct Ordering {
    state: Mutex<OrderingState>,
    wake: Notify,
}

struct OrderingState {
    /// Ids accepted from the client and not yet answered, in
    /// arrival order.
    awaiting: Vec<Value>,
    /// The id currently forwarded to the SDK, if any.
    inflight: Option<Value>,
    /// The client stream reached EOF.
    eof_seen: bool,
}

impl Ordering {
    fn new() -> Self {
        Self {
            state: Mutex::new(OrderingState {
                awaiting: Vec::new(),
                inflight: None,
                eof_seen: false,
            }),
            wake: Notify::new(),
        }
    }

    /// Record a request id accepted from the client.
    fn on_accept(&self, id: &Value) {
        self.state.lock().unwrap().awaiting.push(id.clone());
    }

    /// Mark `id` as the request now in flight to the SDK.
    fn on_forward(&self, id: &Value) {
        self.state.lock().unwrap().inflight = Some(id.clone());
    }

    /// A response came back from the SDK: retire its id, release
    /// the in-flight slot, and wake the read loop so it can forward
    /// the next queued item.
    fn on_response(&self, line: &[u8]) {
        if let Ok(message) = serde_json::from_slice::<Value>(line)
            && let Some(id) = message.get("id")
        {
            let mut state = self.state.lock().unwrap();
            if let Some(position) = state.awaiting.iter().position(|p| p == id) {
                state.awaiting.remove(position);
            }
            if state.inflight.as_ref() == Some(id) {
                state.inflight = None;
            }
            drop(state);
            self.wake.notify_one();
        }
    }

    /// Whether any earlier request is still owed a response.
    fn has_outstanding(&self) -> bool {
        !self.state.lock().unwrap().awaiting.is_empty()
    }

    /// Whether the in-flight slot is free.
    fn slot_free(&self) -> bool {
        self.state.lock().unwrap().inflight.is_none()
    }

    /// Record end of client input; wakes the read loop.
    fn mark_eof(&self) {
        self.state.lock().unwrap().eof_seen = true;
        self.wake.notify_one();
    }

    /// Whether input ended and every accepted request is answered:
    /// the point at which the SDK side is shut down.
    fn drained_after_eof(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.eof_seen && state.awaiting.is_empty()
    }
}

/// Classify one raw client line (newline already stripped).
fn classify(line: &[u8]) -> Verdict {
    let message: Value = match serde_json::from_slice(line) {
        Ok(message) => message,
        Err(error) => {
            return Verdict::Reply(error_frame(Value::Null, PARSE_ERROR, &error.to_string()));
        }
    };
    let Some(object) = message.as_object() else {
        return Verdict::Reply(error_frame(
            Value::Null,
            INVALID_REQUEST,
            "message must be an object",
        ));
    };
    // Notification-shaped frames (no id) are dropped unexecuted.
    let Some(id) = object.get("id") else {
        return Verdict::Drop;
    };
    if !id.is_string() && !id.is_number() {
        return Verdict::Reply(error_frame(
            Value::Null,
            INVALID_REQUEST,
            "id must be string or number",
        ));
    }
    let id = id.clone();
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Verdict::Reply(error_frame(id, INVALID_REQUEST, "jsonrpc must be \"2.0\""));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Verdict::Reply(error_frame(id, INVALID_REQUEST, "method must be a string"));
    };
    match method {
        "initialize" => Verdict::Reply(error_frame(
            id,
            ERA_MISMATCH,
            "legacy protocol era not supported by this server; \
             use protocol 2026-07-28 (server/discover)",
        )),
        // `ping` is answered in every state, exactly as the baseline
        // answered it, and never reaches the SDK (whose handler
        // would refuse it once the session is established).
        "ping" => Verdict::Reply(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
        "server/discover" | "tools/list" => Verdict::Forward,
        "tools/call" => classify_call(id, object.get("params")),
        _ => Verdict::Reply(error_frame(
            id,
            METHOD_NOT_FOUND,
            &format!("unknown method `{}`", server::clip(method)),
        )),
    }
}

/// Shape checks for `tools/call`, mirroring the baseline's argument
/// handling: absent or null `arguments` is an empty argument object;
/// a missing or non-object `params`, a non-string `name`, an unknown
/// tool, or a non-object `arguments` is refused before dispatch.
fn classify_call(id: Value, params: Option<&Value>) -> Verdict {
    let Some(params) = params.and_then(Value::as_object) else {
        return Verdict::Reply(error_frame(id, INVALID_PARAMS, "params must be an object"));
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Verdict::Reply(error_frame(
            id,
            INVALID_PARAMS,
            "params.name must be a string",
        ));
    };
    if !server::is_known_tool(name) {
        return Verdict::Reply(error_frame(
            id,
            INVALID_PARAMS,
            &format!("unknown tool `{}`", server::clip(name)),
        ));
    }
    match params.get("arguments") {
        None | Some(Value::Null) | Some(Value::Object(_)) => Verdict::Forward,
        Some(_) => Verdict::Reply(error_frame(
            id,
            INVALID_PARAMS,
            "arguments must be an object",
        )),
    }
}

/// One JSON-RPC error frame; the message passes through the same
/// truncation the baseline applied.
fn error_frame(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": crate::error::truncate(message.to_string())},
    })
}

/// The request id of a line already classified `Forward`.
fn request_id(line: &[u8]) -> Option<Value> {
    serde_json::from_slice::<Value>(line)
        .ok()?
        .get("id")
        .cloned()
}

/// One queued client item waiting for the in-flight slot.
enum Queued {
    /// A request line and its id.
    Request(Vec<u8>, Value),
    /// A notification line (no response will come back).
    Notification(Vec<u8>),
}

/// Flush queued items while the in-flight slot is free: a queued
/// request takes the slot; notifications pass through in order.
async fn flush_queue(
    queue: &mut VecDeque<Queued>,
    ordering: &Ordering,
    to_service: &mut WriteHalf<DuplexStream>,
) -> Result<(), ()> {
    while ordering.slot_free() {
        let Some(item) = queue.pop_front() else {
            return Ok(());
        };
        match item {
            Queued::Request(line, id) => {
                ordering.on_forward(&id);
                write_line(to_service, &line).await?;
            }
            Queued::Notification(line) => write_line(to_service, &line).await?,
        }
    }
    Ok(())
}

/// Run the guard over real stdin: read client lines, forward
/// sequentially, answer the rest, and pump SDK responses to stdout.
/// Returns when the client stream ends and every accepted request
/// has been answered; `eof` fires at that same point, after the
/// SDK's write half is shut down.
pub async fn run(
    stdout: SharedStdout,
    to_service: WriteHalf<DuplexStream>,
    from_service: ReadHalf<DuplexStream>,
    eof: oneshot::Sender<()>,
) {
    let ordering = Arc::new(Ordering::new());
    let pump_task = tokio::spawn(pump(
        from_service,
        Arc::clone(&ordering),
        Arc::clone(&stdout),
    ));
    run_loop(tokio::io::stdin(), to_service, ordering, stdout, eof).await;
    pump_task.abort();
}

/// The guard loop proper, over an arbitrary client stream.
async fn run_loop(
    client: impl AsyncRead + Unpin,
    mut to_service: WriteHalf<DuplexStream>,
    ordering: Arc<Ordering>,
    stdout: SharedStdout,
    eof: oneshot::Sender<()>,
) {
    let mut lines = BufReader::new(client);
    let mut queue: VecDeque<Queued> = VecDeque::new();
    let mut input_open = true;
    let mut eof = Some(eof);
    loop {
        if flush_queue(&mut queue, &ordering, &mut to_service)
            .await
            .is_err()
        {
            // The SDK side is gone; the session is over either way,
            // so report end of input rather than leaving main waiting.
            if let Some(eof) = eof.take() {
                let _ = eof.send(());
            }
            return;
        }
        if ordering.drained_after_eof() {
            let _ = to_service.shutdown().await;
            if let Some(eof) = eof.take() {
                let _ = eof.send(());
            }
            return;
        }
        tokio::select! {
            line = read_bounded(&mut lines), if input_open => {
                match line {
                    ReadOutcome::Eof => {
                        input_open = false;
                        ordering.mark_eof();
                    }
                    ReadOutcome::Line(bytes) => {
                        handle_line(bytes, &mut queue, &ordering, &stdout, &mut to_service).await;
                    }
                }
            }
            () = ordering.wake.notified() => {}
        }
    }
}

/// Classify and act on one client line read inside the guard loop.
async fn handle_line(
    bytes: Vec<u8>,
    queue: &mut VecDeque<Queued>,
    ordering: &Arc<Ordering>,
    stdout: &Arc<Mutex<std::io::Stdout>>,
    to_service: &mut WriteHalf<DuplexStream>,
) {
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return; // blank keep-alive lines are skipped, as in the baseline
    }
    if bytes.len() > REQUEST_BYTES_MAX {
        // The remainder of the line was already drained by the
        // bounded read; answer and move on.
        wait_for_drain(ordering).await;
        write_stdout(
            stdout,
            &error_frame(Value::Null, PARSE_ERROR, "request line exceeds size bound"),
        );
        return;
    }
    match classify(&bytes) {
        Verdict::Drop => {}
        Verdict::Reply(frame) => {
            wait_for_drain(ordering).await;
            write_stdout(stdout, &frame);
        }
        Verdict::Forward => match request_id(&bytes) {
            Some(id) => {
                ordering.on_accept(&id);
                queue.push_back(Queued::Request(bytes, id));
            }
            // No id: a notification. `notifications/cancelled` for
            // the in-flight request keeps its meaning only in wire
            // order, so notifications queue behind it like the
            // baseline's inline processing did.
            None => queue.push_back(Queued::Notification(bytes)),
        },
    }
    let _ = flush_queue(queue, ordering, to_service).await;
}

/// Copy SDK responses to stdout, retiring ids as they pass. Ends
/// when the SDK side closes.
async fn pump(
    service: ReadHalf<DuplexStream>,
    ordering: Arc<Ordering>,
    stdout: Arc<Mutex<std::io::Stdout>>,
) {
    let mut lines = BufReader::new(service);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match lines.read_until(b'\n', &mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                ordering.on_response(&buffer);
                let mut out = stdout.lock().unwrap();
                let _ = out.write_all(&buffer);
                let _ = out.flush();
            }
        }
    }
}

/// Wait (bounded) until no earlier request is owed a response.
async fn wait_for_drain(ordering: &Ordering) {
    let start = Instant::now();
    while ordering.has_outstanding() && start.elapsed() < DRAIN_WAIT_MAX {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Serialize one guard-answered frame to stdout.
fn write_stdout(stdout: &Arc<Mutex<std::io::Stdout>>, frame: &Value) {
    let mut bytes = serde_json::to_vec(frame).unwrap_or_default();
    bytes.push(b'\n');
    let mut out = stdout.lock().unwrap();
    let _ = out.write_all(&bytes);
    let _ = out.flush();
}

/// Write one raw client line to the SDK side of the duplex.
async fn write_line(to_service: &mut WriteHalf<DuplexStream>, line: &[u8]) -> Result<(), ()> {
    to_service.write_all(line).await.map_err(|_| ())?;
    to_service.write_all(b"\n").await.map_err(|_| ())?;
    to_service.flush().await.map_err(|_| ())
}

/// Outcome of one bounded read from the client stream.
enum ReadOutcome {
    /// One line, newline stripped; an oversize line is drained and
    /// returned at its full held length so the caller can refuse it.
    Line(Vec<u8>),
    /// The stream ended.
    Eof,
}

/// Read one line with a hard memory bound: at most
/// `REQUEST_BYTES_MAX` plus one byte is ever held (the extra byte is
/// what proves the line is oversize); the rest is drained unread.
async fn read_bounded<R: AsyncRead + Unpin>(reader: &mut BufReader<R>) -> ReadOutcome {
    let mut line = Vec::new();
    loop {
        let available = match reader.fill_buf().await {
            Ok(buf) => buf,
            Err(_) => return ReadOutcome::Eof,
        };
        if available.is_empty() {
            return if line.is_empty() {
                ReadOutcome::Eof
            } else {
                ReadOutcome::Line(line)
            };
        }
        let room = REQUEST_BYTES_MAX + 1 - line.len().min(REQUEST_BYTES_MAX + 1);
        let take = available.len().min(room);
        if let Some(newline) = available[..take].iter().position(|&b| b == b'\n') {
            line.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            return ReadOutcome::Line(line);
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if take == 0 {
            // The held prefix is at the bound and no newline is in
            // reach: keep draining without holding more.
            let mut byte = [0u8; 1];
            loop {
                match reader.read(&mut byte).await {
                    Ok(0) | Err(_) => return ReadOutcome::Line(line),
                    Ok(_) if byte[0] == b'\n' => return ReadOutcome::Line(line),
                    Ok(_) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(line: &str) -> Value {
        match classify(line.as_bytes()) {
            Verdict::Reply(frame) => frame,
            other => panic!("expected a guard reply, got {other:?}"),
        }
    }

    #[test]
    fn legacy_initialize_gets_the_typed_era_refusal() {
        let frame = reply(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        );
        assert_eq!(frame["error"]["code"], ERA_MISMATCH);
        assert!(
            frame["error"]["message"]
                .as_str()
                .unwrap()
                .contains("2026-07-28")
        );
    }

    #[test]
    fn string_ids_are_preserved_in_replies() {
        let frame = reply(r#"{"jsonrpc":"2.0","id":"req-9","method":"initialize"}"#);
        assert_eq!(frame["id"], "req-9");
    }

    #[test]
    fn ping_is_answered_with_an_empty_result() {
        let frame = reply(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#);
        assert_eq!(frame["result"], json!({}));
        assert!(frame.get("error").is_none());
    }

    #[test]
    fn discover_list_and_valid_calls_forward() {
        for line in [
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"lovense_stop"}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"lovense_stop","arguments":null}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"lovense_stop","arguments":{}}}"#,
        ] {
            assert_eq!(classify(line.as_bytes()), Verdict::Forward, "{line}");
        }
    }

    #[test]
    fn notification_shaped_frames_drop_unexecuted() {
        for line in [
            r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"lovense_stop"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","method":"ping"}"#,
        ] {
            assert_eq!(classify(line.as_bytes()), Verdict::Drop, "{line}");
        }
    }

    #[test]
    fn unknown_methods_get_the_baseline_wording() {
        let frame = reply(r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#);
        assert_eq!(frame["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(frame["error"]["message"], "unknown method `resources/list`");
    }

    #[test]
    fn unknown_tools_are_refused_before_any_dispatch() {
        // Names that must never reach a handler: plausible-but-wrong
        // tools a client could guess from the domain.
        for name in [
            "lovense_heartbeat",
            "lovense_patternv2",
            "lovense_vibrate",
            "LOVENSE_TOYS",
            "",
        ] {
            let line = format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{name}"}}}}"#
            );
            let frame = reply(&line);
            assert_eq!(frame["error"]["code"], INVALID_PARAMS, "{name}");
            assert_eq!(
                frame["error"]["message"],
                format!("unknown tool `{name}`"),
                "{name}"
            );
        }
    }

    #[test]
    fn call_shape_refusals_match_the_baseline() {
        let cases: &[(&str, &str)] = &[
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#,
                "params must be an object",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":[]}"#,
                "params must be an object",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#,
                "params.name must be a string",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":3}}"#,
                "params.name must be a string",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"lovense_stop","arguments":[]}}"#,
                "arguments must be an object",
            ),
        ];
        for (line, message) in cases {
            let frame = reply(line);
            assert_eq!(frame["error"]["code"], INVALID_PARAMS, "{line}");
            assert_eq!(frame["error"]["message"], *message, "{line}");
        }
    }

    #[test]
    fn malformed_envelopes_match_the_baseline() {
        // The baseline's framing battery, re-homed: each line maps to
        // the same code and message the hand-rolled server produced.
        let cases: &[(&str, i64, &str)] = &[
            (
                "{not json",
                PARSE_ERROR,
                "key must be a string at line 1 column 2",
            ),
            ("[]", INVALID_REQUEST, "message must be an object"),
            (
                r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#,
                INVALID_REQUEST,
                "id must be string or number",
            ),
            (
                r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
                INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            ),
            (
                r#"{"id":1,"method":"ping"}"#,
                INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1}"#,
                INVALID_REQUEST,
                "method must be a string",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":4}"#,
                INVALID_REQUEST,
                "method must be a string",
            ),
        ];
        for (line, code, message) in cases {
            let frame = reply(line);
            assert_eq!(frame["error"]["code"], *code, "{line}");
            assert_eq!(frame["error"]["message"], *message, "{line}");
        }
    }

    #[test]
    fn non_object_id_is_null_in_error_frames() {
        let frame = reply(r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#);
        assert_eq!(frame["id"], Value::Null);
    }

    #[test]
    fn ordering_tracks_the_inflight_slot_and_drain() {
        let ordering = Ordering::new();
        ordering.on_accept(&json!(1));
        ordering.on_accept(&json!(2));
        ordering.on_forward(&json!(1));
        assert!(ordering.has_outstanding());
        assert!(!ordering.slot_free());
        // An unrelated frame does not release the slot.
        ordering.on_response(br#"{"jsonrpc":"2.0","method":"notifications/progress"}"#);
        assert!(!ordering.slot_free());
        ordering.on_response(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
        assert!(ordering.slot_free());
        assert!(ordering.has_outstanding(), "id 2 is still owed");
        ordering.mark_eof();
        assert!(!ordering.drained_after_eof());
        ordering.on_response(br#"{"jsonrpc":"2.0","id":2,"result":{}}"#);
        assert!(ordering.drained_after_eof());
    }
}
