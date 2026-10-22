//! The transport boundary. S1 exposes READ operations only: a status
//! probe (no I/O) and toy discovery. There is deliberately no output
//! method on the trait; S2 adds output behind the `decide()` gate.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::config::{Config, TransportKind};
use crate::error::{ErrorCode, LovenseError, Result, err};

/// Whole-request deadline for one discovery call (the bridge's figure).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// Largest app response read, in bytes; anything longer is refused.
pub const RESPONSE_BYTES_MAX: u64 = 256 * 1024;
/// Most toys accepted from one discovery answer.
pub const TOYS_MAX: usize = 64;
/// Longest toy string field kept, in bytes.
pub const TOY_FIELD_BYTES_MAX: usize = 128;
/// Value of the required `X-platform` header.
pub const PLATFORM_NAME: &str = "lovense-mcp";

/// One toy as reported by discovery. Absent or unparseable fields stay
/// `None` rather than being invented.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Toy {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub toy_type: Option<String>,
    /// Percent, 0..=100.
    pub battery: Option<u8>,
}

/// A configured network target. Only constructible with a non-empty host,
/// so a `RealTransport` without a target cannot exist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Target {
    pub scheme: &'static str,
    pub host: String,
    pub port: u16,
}

impl Target {
    pub fn from_config(config: &Config) -> Option<Target> {
        if config.host.is_empty() {
            return None;
        }
        let scheme = if config.use_https { "https" } else { "http" };
        Some(Target {
            scheme,
            host: config.host.clone(),
            port: config.port,
        })
    }

    pub fn command_url(&self) -> String {
        format!("{}://{}:{}/command", self.scheme, self.host, self.port)
    }
}

/// What a probe reports. Pure: no network I/O.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportProbe {
    pub kind: TransportKind,
    pub target: Option<Target>,
}

/// The read-only transport contract.
pub trait ReadTransport {
    /// Describe this transport. Never performs I/O.
    fn probe(&self) -> TransportProbe;
    /// Ask for the connected toys. Real transports make exactly one request.
    fn discover(&self) -> Result<Vec<Toy>>;
}

/// The production transport for a config, or `None` when the config
/// names the real transport but no host: then nothing can reach the wire.
pub fn from_config(config: &Config) -> Option<Box<dyn ReadTransport>> {
    match config.transport {
        TransportKind::Mock => Some(Box::new(MockTransport::new())),
        TransportKind::Real => {
            Target::from_config(config).map(|target| Box::new(RealTransport::new(target)) as _)
        }
    }
}

// ---------------------------------------------------------------- mock

/// Every request the mock receives, in order, for test assertions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadRequest {
    Probe,
    Discover,
}

pub struct MockTransport {
    requests: Arc<Mutex<Vec<ReadRequest>>>,
}

impl MockTransport {
    pub fn new() -> MockTransport {
        MockTransport {
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A shared handle on the request log.
    #[cfg(test)]
    pub fn request_log(&self) -> Arc<Mutex<Vec<ReadRequest>>> {
        Arc::clone(&self.requests)
    }

    pub fn fixture_toys() -> Vec<Toy> {
        vec![
            Toy {
                id: "mock-toy-1".into(),
                name: "Mock Lush".into(),
                toy_type: Some("lush".into()),
                battery: Some(87),
            },
            Toy {
                id: "mock-toy-2".into(),
                name: "Mock Edge".into(),
                toy_type: Some("edge".into()),
                battery: Some(42),
            },
        ]
    }

    fn record(&self, request: ReadRequest) {
        // A poisoned log only means a test thread panicked mid-push; the
        // Vec is still valid, so keep recording.
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        requests.push(request);
    }
}

impl ReadTransport for MockTransport {
    fn probe(&self) -> TransportProbe {
        self.record(ReadRequest::Probe);
        TransportProbe {
            kind: TransportKind::Mock,
            target: None,
        }
    }

    fn discover(&self) -> Result<Vec<Toy>> {
        self.record(ReadRequest::Discover);
        Ok(MockTransport::fixture_toys())
    }
}

// ---------------------------------------------------------------- real

pub struct RealTransport {
    target: Target,
    agent: ureq::Agent,
}

impl RealTransport {
    pub fn new(target: Target) -> RealTransport {
        assert!(!target.host.is_empty(), "Target is only built with a host");
        // No redirects: a local app has no reason to send us elsewhere.
        let agent = ureq::AgentBuilder::new()
            .timeout(REQUEST_TIMEOUT)
            .redirects(0)
            .build();
        RealTransport { target, agent }
    }
}

impl ReadTransport for RealTransport {
    fn probe(&self) -> TransportProbe {
        TransportProbe {
            kind: TransportKind::Real,
            target: Some(self.target.clone()),
        }
    }

    fn discover(&self) -> Result<Vec<Toy>> {
        let url = self.target.command_url();
        let response = self
            .agent
            .post(&url)
            .set("X-platform", PLATFORM_NAME)
            .set("Content-Type", "application/json")
            .send_string(r#"{"command":"GetToys"}"#);
        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::Status(status, _)) => {
                return err(ErrorCode::AppError, format!("{url} answered HTTP {status}"));
            }
            Err(ureq::Error::Transport(transport)) => {
                return err(ErrorCode::TargetUnreachable, format!("{url}: {transport}"));
            }
        };
        let mut body = Vec::new();
        response
            .into_reader()
            .take(RESPONSE_BYTES_MAX + 1)
            .read_to_end(&mut body)
            .map_err(|cause| {
                LovenseError::new(ErrorCode::TargetUnreachable, format!("{url}: {cause}"))
            })?;
        if body.len() as u64 > RESPONSE_BYTES_MAX {
            return err(
                ErrorCode::ResponseInvalid,
                format!("response exceeds {RESPONSE_BYTES_MAX} bytes"),
            );
        }
        parse_get_toys(&body)
    }
}

/// Parse a GetToys answer. Documented shape:
/// `{"code":200,"data":{"toys":"<JSON string of id -> toy>"}}`; `toys` may
/// also arrive as a plain object. Anything else is `response_invalid`.
pub fn parse_get_toys(body: &[u8]) -> Result<Vec<Toy>> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|cause| invalid_response(format!("not JSON: {cause}")))?;
    let Some(code) = root.get("code").and_then(Value::as_i64) else {
        return Err(invalid_response("no numeric `code` field"));
    };
    if code != 200 {
        return err(ErrorCode::AppError, format!("Lovense API code {code}"));
    }
    let Some(toys) = root.get("data").and_then(|data| data.get("toys")) else {
        return Err(invalid_response("no `data.toys` field"));
    };
    let inner;
    let map = match toys {
        Value::String(text) if text.trim().is_empty() => return Ok(Vec::new()),
        Value::String(text) => {
            inner = serde_json::from_str::<Value>(text)
                .map_err(|cause| invalid_response(format!("`data.toys` string: {cause}")))?;
            &inner
        }
        other => other,
    };
    let Some(map) = map.as_object() else {
        return Err(invalid_response("`data.toys` is not an object"));
    };
    if map.len() > TOYS_MAX {
        return Err(invalid_response(format!("more than {TOYS_MAX} toys")));
    }
    let mut result = Vec::with_capacity(map.len());
    for (key, value) in map {
        result.push(parse_toy(key, value)?);
    }
    Ok(result)
}

fn parse_toy(key: &str, value: &Value) -> Result<Toy> {
    if !value.is_object() {
        return Err(invalid_response(format!(
            "toy `{}` is not an object",
            bounded(key)
        )));
    }
    let id = string_field(value, "id").unwrap_or_else(|| bounded(key));
    let name = string_field(value, "nickName")
        .or_else(|| string_field(value, "name"))
        .unwrap_or_else(|| id.clone());
    let battery = match value.get("battery") {
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(text)) => text.trim().parse::<u64>().ok(),
        _ => None,
    };
    Ok(Toy {
        id,
        name,
        toy_type: string_field(value, "toyType"),
        // -1 or out-of-range means the app does not know; report unknown.
        battery: battery
            .filter(|percent| *percent <= 100)
            .map(|percent| percent as u8),
    })
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    match value.get(field) {
        Some(Value::String(text)) if !text.trim().is_empty() => Some(bounded(text)),
        _ => None,
    }
}

fn bounded(text: &str) -> String {
    let mut cut = text.len().min(TOY_FIELD_BYTES_MAX);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].to_string()
}

fn invalid_response(reason: impl Into<String>) -> LovenseError {
    LovenseError::new(ErrorCode::ResponseInvalid, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_stub::{AppStub, closed_port};

    fn loopback(port: u16) -> RealTransport {
        RealTransport::new(Target {
            scheme: "http",
            host: "127.0.0.1".into(),
            port,
        })
    }

    const DOCUMENTED: &str = r#"{"code":200,"data":{"toys":"{\"a1\":{\"id\":\"a1\",\"name\":\"lush\",\"nickName\":\"\",\"toyType\":\"lush\",\"battery\":64,\"status\":\"1\"}}"},"type":"OK"}"#;

    // --- validation ---

    #[test]
    fn mock_discovers_fixture_and_records_requests() {
        let mock = MockTransport::new();
        let log = mock.request_log();
        assert_eq!(mock.discover().unwrap(), MockTransport::fixture_toys());
        assert_eq!(mock.probe().kind, TransportKind::Mock);
        assert_eq!(
            *log.lock().unwrap(),
            vec![ReadRequest::Discover, ReadRequest::Probe]
        );
        assert_eq!(MockTransport::fixture_toys().len(), 2);
    }

    #[test]
    fn from_config_maps_transport_and_target() {
        let mock = from_config(&Config::default()).unwrap();
        assert_eq!(mock.probe().kind, TransportKind::Mock);
        let real = Config {
            transport: TransportKind::Real,
            host: "10.0.0.2".into(),
            use_https: true,
            port: 30010,
            ..Config::default()
        };
        let probe = from_config(&real).unwrap().probe();
        assert_eq!(probe.kind, TransportKind::Real);
        assert_eq!(
            probe.target.unwrap().command_url(),
            "https://10.0.0.2:30010/command"
        );
    }

    #[test]
    fn documented_string_shape_parses() {
        let toys = parse_get_toys(DOCUMENTED.as_bytes()).unwrap();
        assert_eq!(
            toys,
            vec![Toy {
                id: "a1".into(),
                name: "lush".into(),
                toy_type: Some("lush".into()),
                battery: Some(64),
            }]
        );
    }

    #[test]
    fn object_shape_and_string_battery_parse() {
        let body = r#"{"code":200,"data":{"toys":{"b2":{"nickName":"Mine","battery":"55"}}}}"#;
        let toys = parse_get_toys(body.as_bytes()).unwrap();
        assert_eq!(toys[0].id, "b2");
        assert_eq!(toys[0].name, "Mine");
        assert_eq!(toys[0].toy_type, None);
        assert_eq!(toys[0].battery, Some(55));
    }

    #[test]
    fn real_discovery_against_loopback_stub() {
        let mut stub = AppStub::start(DOCUMENTED, 1, 200);
        let toys = loopback(stub.port).discover().unwrap();
        assert_eq!(toys.len(), 1);
        let received = stub.received();
        assert_eq!(received["path"], "/command");
        assert_eq!(received["platform"], PLATFORM_NAME);
        assert_eq!(received["body"], r#"{"command":"GetToys"}"#);
    }

    // --- adversarial ---

    #[test]
    fn stub_returning_malformed_json_is_response_invalid() {
        let mut stub = AppStub::start("{not json", 1, 200);
        let error = loopback(stub.port).discover().unwrap_err();
        assert_eq!(error.code, ErrorCode::ResponseInvalid);
        stub.received();
    }

    #[test]
    fn unreachable_target_is_a_structured_error_not_an_empty_list() {
        let error = loopback(closed_port()).discover().unwrap_err();
        assert_eq!(error.code, ErrorCode::TargetUnreachable);
        assert!(error.message.contains("127.0.0.1"), "{}", error.message);
    }

    #[test]
    fn oversized_response_is_refused() {
        let mut stub = AppStub::start("x", RESPONSE_BYTES_MAX as usize + 1, 200);
        let error = loopback(stub.port).discover().unwrap_err();
        assert_eq!(error.code, ErrorCode::ResponseInvalid);
        stub.received();
    }

    #[test]
    fn http_error_status_is_app_error() {
        let mut stub = AppStub::start("{}", 1, 500);
        let error = loopback(stub.port).discover().unwrap_err();
        assert_eq!(error.code, ErrorCode::AppError);
        stub.received();
    }

    #[test]
    fn malformed_answers_are_refused_without_panic() {
        let cases: &[(&str, ErrorCode)] = &[
            ("", ErrorCode::ResponseInvalid),
            ("[]", ErrorCode::ResponseInvalid),
            (r#"{"code":"200"}"#, ErrorCode::ResponseInvalid),
            (r#"{"code":400,"type":"error"}"#, ErrorCode::AppError),
            (r#"{"code":200}"#, ErrorCode::ResponseInvalid),
            (
                r#"{"code":200,"data":{"toys":"{oops"}}"#,
                ErrorCode::ResponseInvalid,
            ),
            (
                r#"{"code":200,"data":{"toys":[1,2]}}"#,
                ErrorCode::ResponseInvalid,
            ),
            (
                r#"{"code":200,"data":{"toys":{"a":"str"}}}"#,
                ErrorCode::ResponseInvalid,
            ),
        ];
        for (body, code) in cases {
            let error = parse_get_toys(body.as_bytes()).unwrap_err();
            assert_eq!(error.code, *code, "body {body}");
        }
    }

    #[test]
    fn hostile_toy_fields_are_bounded_or_dropped() {
        let long = "n".repeat(10_000);
        let body = format!(
            r#"{{"code":200,"data":{{"toys":{{"k":{{"name":"{long}","battery":-1,"toyType":7}}}}}}}}"#
        );
        let toys = parse_get_toys(body.as_bytes()).unwrap();
        assert_eq!(toys[0].name.len(), TOY_FIELD_BYTES_MAX);
        assert_eq!(toys[0].battery, None);
        assert_eq!(toys[0].toy_type, None);
        let too_many: serde_json::Map<String, Value> = (0..=TOYS_MAX)
            .map(|index| (index.to_string(), serde_json::json!({})))
            .collect();
        let body = serde_json::json!({"code": 200, "data": {"toys": too_many}}).to_string();
        assert_eq!(
            parse_get_toys(body.as_bytes()).unwrap_err().code,
            ErrorCode::ResponseInvalid
        );
    }
}
