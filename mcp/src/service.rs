//! The rmcp service layer (fleet standardization WS2, program D-113).
//!
//! The protocol era is pinned to 2026-07-28 in
//! `supported_protocol_versions` (which `server/discover` serves);
//! the era
//! guard (`guard.rs`) owns the stdio framing, so this layer only ever
//! sees shape-valid modern frames: `server/discover`, `tools/list`,
//! and `tools/call`. Tool definitions are served RAW from the same
//! hand-written JSON the pre-migration server served — schema delta
//! zero by construction — and tool dispatch goes through
//! [`Server::dispatch_call`], the unchanged handler path, so every
//! refusal keeps its shape, code, and message class.
//!
//! Dispatch is serialized behind one lock and run on a blocking
//! thread: the pre-migration serve loop was strictly sequential, and
//! the providers behind the seam were built for that contract. This
//! also keeps response order deterministic per connection.
//!
//! The watchdog poller preserves the baseline's fire-during-silence
//! behavior: the old loop tripped the watchdog while waiting for
//! input; here a timer task trips it at a bounded cadence until
//! shutdown.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode,
    Implementation, InitializeResult, ListToolsResult, ProtocolVersion, ServerCapabilities, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};
use serde_json::Value;

use crate::server::{self, SERVER_NAME, SERVER_VERSION, Server};

/// The handshake instructions text, carried over byte-for-byte from
/// the pre-migration server's `initialize` result.
const INSTRUCTIONS: &str = "ache — unified haptics: Lovense local Game Mode and an \
    embedded Buttplug server behind one surface. Tools: lovense_status, \
    lovense_toys, lovense_arm, lovense_disarm, lovense_function, \
    lovense_pattern, lovense_preset, lovense_stop, ache_devices, \
    ache_device. Output is off by default: arm explicitly, every output \
    on either provider passes the one safety gate that clamps intensity \
    to the configured ceiling, and stop always works.";

/// How often the watchdog poller checks for an expired window. The
/// baseline tripped on its serve-loop cadence; a trip lands at most
/// this late past the deadline.
const WATCHDOG_POLL: Duration = Duration::from_millis(200);

#[derive(Clone)]
pub struct AcheService {
    server: Arc<Server>,
    /// Serializes dispatch: the handler path (and the providers
    /// behind it) keeps the baseline's one-call-at-a-time contract.
    dispatch_lock: Arc<Mutex<()>>,
}

impl AcheService {
    pub fn new(server: Arc<Server>) -> AcheService {
        AcheService {
            server,
            dispatch_lock: Arc::new(Mutex::new(())),
        }
    }
}

/// Trip the watchdog on a timer until the caller aborts the task at
/// shutdown. Output active in silence must die on schedule even when
/// no request ever comes.
pub fn spawn_watchdog(server: Arc<Server>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(WATCHDOG_POLL).await;
            server.trip_watchdog();
        }
    })
}

impl ServerHandler for AcheService {
    /// The era pin: `server/discover` advertises exactly this list,
    /// and it is the only era the guard lets a session run in.
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Borrowed(&[ProtocolVersion::V_2026_07_28])
    }

    fn get_info(&self) -> InitializeResult {
        let mut info = InitializeResult::default();
        info.protocol_version = ProtocolVersion::V_2026_07_28;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new(SERVER_NAME, SERVER_VERSION);
        info.instructions = Some(INSTRUCTIONS.into());
        info
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(build_tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.into_owned();
        let arguments = request.arguments.map(Value::Object);
        let server = Arc::clone(&self.server);
        let lock = Arc::clone(&self.dispatch_lock);
        let outcome = tokio::task::spawn_blocking(move || {
            let _held = lock.lock().unwrap_or_else(|poison| poison.into_inner());
            server.dispatch_call(&name, arguments)
        })
        .await
        .map_err(|cause| {
            McpError::internal_error(format!("tool dispatch task failed: {cause}"), None)
        })?;
        match outcome {
            Ok(result) => Ok(CallToolResponse::from(call_tool_result(&result))),
            Err((code, message)) => {
                let code = i32::try_from(code).map_err(|_| {
                    McpError::internal_error("protocol refusal code out of range", None)
                })?;
                Err(McpError::new(ErrorCode(code), message, None))
            }
        }
    }
}

/// Convert the handler path's tool-result JSON (the exact value the
/// pre-migration server put on the wire) into rmcp's result type.
/// The text payload is the pretty-printed JSON, unchanged; `isError`
/// rides along. rmcp stamps its own envelope fields on top, which
/// the parity table records as the accepted envelope class.
fn call_tool_result(result: &Value) -> CallToolResult {
    let text = result
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let content = vec![ContentBlock::text(text)];
    if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

/// Build rmcp tool values from the served JSON definitions without
/// re-shaping them: name, description, and the raw input schema pass
/// through; annotations are carried field for field.
fn build_tools() -> Vec<Tool> {
    let Value::Array(definitions) = server::tool_list() else {
        return Vec::new();
    };
    definitions
        .iter()
        .filter_map(|definition| {
            let name = definition.get("name")?.as_str()?.to_string();
            let description = definition
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let schema = definition.get("inputSchema")?.as_object()?.clone();
            let mut tool = Tool::new(name, description, schema);
            if let Some(annotations) = definition.get("annotations") {
                let text = |key: &str| {
                    annotations
                        .get(key)
                        .and_then(Value::as_str)
                        .map(str::to_string)
                };
                let flag = |key: &str| annotations.get(key).and_then(Value::as_bool);
                tool.annotations = Some(ToolAnnotations::from_raw(
                    text("title"),
                    flag("readOnlyHint"),
                    flag("destructiveHint"),
                    flag("idempotentHint"),
                    flag("openWorldHint"),
                ));
            }
            Some(tool)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_carry_the_served_names_and_annotations() {
        let tools = build_tools();
        assert_eq!(tools.len(), 10);
        let status = &tools[0];
        assert_eq!(status.name, "lovense_status");
        let annotations = status.annotations.as_ref().expect("annotations");
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));
        let stop = &tools[7];
        assert_eq!(stop.name, "lovense_stop");
        assert_eq!(
            stop.annotations.as_ref().expect("annotations").title,
            Some("Lovense stop".into())
        );
    }

    #[test]
    fn served_tools_serialize_back_to_the_baseline_definitions() {
        // The tools/list parity gate in miniature: rmcp's Tool
        // serialization of the raw definitions must be value-equal
        // to the definitions themselves (canonicalized by the session
        // harness over the wire; here over the values directly).
        let served = serde_json::to_value(build_tools()).expect("serialize");
        assert_eq!(served, server::tool_list());
    }

    #[test]
    fn tool_result_conversion_keeps_text_and_error_flag() {
        let ok = call_tool_result(&serde_json::json!({
            "content": [{ "type": "text", "text": "{}" }], "isError": false
        }));
        assert_eq!(ok.is_error, Some(false));
        let err = call_tool_result(&serde_json::json!({
            "content": [{ "type": "text", "text": "{}" }], "isError": true
        }));
        assert_eq!(err.is_error, Some(true));
    }
}
