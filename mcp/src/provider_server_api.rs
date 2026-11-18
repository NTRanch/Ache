//! The Lovense Server API provider (D-1451): the third provider
//! behind the one `decide()` gate, and the only one whose transport
//! leaves the local network — commands relay through Lovense's cloud
//! with a developer token, giving the server true internet remote
//! play. The gate's authority is unchanged: this provider executes
//! [`AllowedOutput`] plans and nothing else, exactly like the local
//! providers; only the transport differs.
//!
//! Posture (D-1451, binding):
//! - Opt-in and OFF by default. The provider exists in the surface
//!   only when `[server_api] enabled = true`; local routes remain
//!   the default, and a disabled provider is inert and says so.
//! - The developer token (and the paired user id) are referenced by
//!   password-store path only — config names `lovense/ntr/token` and
//!   `lovense/ntr/uid`, never a value. Values are loaded at runtime
//!   through [`SecretSource`], are held only in memory, and never
//!   appear in logs, errors, or tool results. An unloadable secret
//!   degrades to a structured `secret_unavailable`, never a panic,
//!   a retry loop, or a fake success.
//! - QR pairing is the Owner's explicit act. The pairing surface
//!   ([`PairingProvider`]) can report pairing state and, on an
//!   explicit operator invocation, request a QR code from Lovense
//!   for the Owner to scan in the Remote app. It performs no pairing
//!   itself and can never cause device output; pairing completes in
//!   the app, and the Owner places the resulting user id in the
//!   password store as the final step.
//! - Lovense Live Control and Control Link have no third-party API
//!   and are out of scope, here as everywhere in this server.
//!
//! Wire shape (estate records, from the official Standard API docs):
//! the server route is `POST https://api.lovense-api.com/api/lan/v2/command`
//! carrying the same command vocabulary the gate already builds
//! (Function / Pattern / Preset / GetToys, `code` 200 ack envelope),
//! with the developer `token` and the paired `uid` merged into the
//! body as auth fields. QR codes come from
//! `POST https://api.lovense-api.com/api/lan/getQrCode` with the
//! token. Evidence: local-lovense-api skill (distilled from
//! developer.lovense.com via the S1 research report, 2026-10-18);
//! COVERAGE-MATRIX.md [L] (Lovense Standard Solutions).

#[cfg(test)]
use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::config::Config;
use crate::decide::{AllowedOutput, Decision, GateSnapshot, OutputIntent, decide, lovense_wire};
use crate::error::{ErrorCode, Result, err};
use crate::provider::{Capability, Device, DeviceProvider, ProviderId, ProviderProbe};
use crate::transport::{
    PLATFORM_NAME, RESPONSE_BYTES_MAX, TOY_FIELD_BYTES_MAX, Toy, parse_command_ack, parse_get_toys,
};

/// The Lovense cloud base the server route posts to.
pub const SERVER_API_BASE: &str = "https://api.lovense-api.com";
/// The server-route command endpoint (token + uid auth in the body).
pub const SERVER_API_COMMAND_PATH: &str = "/api/lan/v2/command";
/// The QR-code endpoint for pairing (token auth in the body).
pub const SERVER_API_QR_PATH: &str = "/api/lan/getQrCode";
/// Whole-request deadline for one cloud call. The cloud route adds
/// an internet round trip to the local route's 2 s figure.
pub const SERVER_API_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest one password-store lookup may run before the secret is
/// declared unavailable: a locked store must degrade, never hang
/// the server on a pinentry prompt.
pub const SECRET_LOAD_TIMEOUT: Duration = Duration::from_secs(5);

// ------------------------------------------------------------ secrets

/// Where a credential value comes from, named by password-store
/// path. Implementations return the value or a structured
/// `secret_unavailable`; the value itself is never logged or
/// surfaced by any caller.
pub trait SecretSource: Send + Sync {
    /// Load the value stored at `entry` (a path, not a value).
    fn load(&self, entry: &str) -> Result<String>;
}

fn secret_unavailable<T>(entry: &str) -> Result<T> {
    err(
        ErrorCode::SecretUnavailable,
        format!("password-store entry `{entry}` is unavailable (secret unavailable)"),
    )
}

/// The production secret source: the studio password store, read
/// through the `pass` CLI at runtime. The lookup is bounded
/// ([`SECRET_LOAD_TIMEOUT`]) with stdin closed, so a locked store
/// degrades to `secret_unavailable` instead of blocking on a
/// pinentry prompt; there is no retry and nothing is persisted.
pub struct PassSecretSource;

impl SecretSource for PassSecretSource {
    fn load(&self, entry: &str) -> Result<String> {
        match run_pass(entry) {
            Some(value) => Ok(value),
            None => secret_unavailable(entry),
        }
    }
}

/// Run `pass show <entry>` with a hard deadline. `None` covers every
/// failure — spawn failure, non-zero exit, timeout, non-UTF-8 or
/// empty output — because the caller distinguishes nothing between
/// them: the secret is unavailable.
fn run_pass(entry: &str) -> Option<String> {
    let mut child = Command::new("pass")
        .arg("show")
        .arg(entry)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + SECRET_LOAD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                // The output is one short line; the pipe cannot fill.
                let mut out = Vec::new();
                child.stdout.take()?.read_to_end(&mut out).ok()?;
                let text = String::from_utf8(out).ok()?;
                let value = text.trim_end().to_string();
                return if value.is_empty() { None } else { Some(value) };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

/// A fixture secret source for tests: a fixed path → value map, or
/// total failure. Values here are fixtures, never real credentials.
#[cfg(test)]
pub struct FixtureSecretSource {
    entries: HashMap<String, String>,
    fail: bool,
}

#[cfg(test)]
impl FixtureSecretSource {
    pub fn with(entries: &[(&str, &str)]) -> FixtureSecretSource {
        FixtureSecretSource {
            entries: entries
                .iter()
                .map(|(path, value)| (path.to_string(), value.to_string()))
                .collect(),
            fail: false,
        }
    }

    pub fn failing() -> FixtureSecretSource {
        FixtureSecretSource {
            entries: HashMap::new(),
            fail: true,
        }
    }
}

#[cfg(test)]
impl SecretSource for FixtureSecretSource {
    fn load(&self, entry: &str) -> Result<String> {
        if self.fail {
            return secret_unavailable(entry);
        }
        match self.entries.get(entry) {
            Some(value) => Ok(value.clone()),
            None => secret_unavailable(entry),
        }
    }
}

// ------------------------------------------------------------ pairing

/// A QR code issued by Lovense for the Owner to scan in the Remote
/// app. An invitation, not a credential: it may be shown to the
/// operator, and it pairs nothing by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrCode {
    pub qr_code: String,
    /// The short numeric code the docs carry alongside, when present.
    pub code: Option<String>,
}

impl QrCode {
    pub fn to_json(&self) -> Value {
        json!({ "qr_code": self.qr_code, "code": self.code })
    }
}

/// Pairing state as presence only: whether credentials exist in the
/// store, never their values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingState {
    pub enabled: bool,
    pub token_present: bool,
    pub uid_present: bool,
    pub paired: bool,
    pub qr_issued: bool,
    pub note: String,
}

impl PairingState {
    pub fn to_json(&self) -> Value {
        json!({
            "provider": ProviderId::ServerApi.as_str(),
            "enabled": self.enabled,
            "token_present": self.token_present,
            "uid_present": self.uid_present,
            "paired": self.paired,
            "qr_issued": self.qr_issued,
            "note": self.note,
        })
    }
}

/// The pairing surface. Deliberately separate from
/// [`DeviceProvider`]: pairing is an operator/Owner flow, not a
/// device operation, and nothing on this surface can cause output.
pub trait PairingProvider: Send + Sync {
    /// Report pairing state. Presence checks only; never a value.
    fn pairing_state(&self) -> PairingState;
    /// Ask Lovense for a QR code for the Owner to scan. An explicit
    /// operator act: this issues an invitation and pairs nothing.
    fn request_qr(&self) -> Result<QrCode>;
}

/// The pairing surface when the provider is disabled: state reports
/// disabled, and a QR request is refused, never faked.
pub struct NoPairing {
    note: String,
}

impl NoPairing {
    pub fn disabled(note: impl Into<String>) -> NoPairing {
        NoPairing { note: note.into() }
    }
}

impl PairingProvider for NoPairing {
    fn pairing_state(&self) -> PairingState {
        PairingState {
            enabled: false,
            token_present: false,
            uid_present: false,
            paired: false,
            qr_issued: false,
            note: self.note.clone(),
        }
    }

    fn request_qr(&self) -> Result<QrCode> {
        err(ErrorCode::ProviderUnavailable, self.note.clone())
    }
}

// ----------------------------------------------------------- transport

/// The Server API transport boundary, mockable like the local one:
/// request bodies arrive fully formed — the gate's command body with
/// credentials merged by [`server_api_wire`] — and implementations
/// send them exactly. No implementation reshapes a command body;
/// the only body a transport constructs is the QR request, which is
/// an auth request (`{"token": ...}`), not a device command.
pub trait ServerApiTransport: Send + Sync {
    /// POST one fully-formed body to the command endpoint and
    /// validate the ack envelope.
    fn send_command(&self, body: &Value) -> Result<()>;
    /// POST a GetToys body to the command endpoint and parse the
    /// reply into toys.
    fn get_toys(&self, body: &Value) -> Result<Vec<Toy>>;
    /// POST the token to the QR endpoint and parse the issued code.
    fn get_qr_code(&self, token: &str) -> Result<QrCode>;
}

/// Merge the auth fields into a gate-constructed body. Pure: the
/// gate's fields pass through untouched (same keys, same values);
/// only `token` and `uid` are added. This is credential attachment,
/// not command assembly — the command was the gate's and stays the
/// gate's.
pub fn server_api_wire(body: &Value, token: &str, uid: &str) -> Value {
    let mut wire = body.clone();
    if let Some(map) = wire.as_object_mut() {
        map.insert("token".to_string(), Value::String(token.to_string()));
        map.insert("uid".to_string(), Value::String(uid.to_string()));
    }
    wire
}

/// The production transport: HTTPS posts to Lovense's cloud, no
/// redirects, bounded reads, the local transport's ack discipline.
pub struct RealServerApiTransport {
    base: String,
    agent: ureq::Agent,
}

impl RealServerApiTransport {
    pub fn new() -> RealServerApiTransport {
        RealServerApiTransport::with_base(SERVER_API_BASE.to_string())
    }

    fn with_base(base: String) -> RealServerApiTransport {
        // No redirects: the cloud endpoint has no reason to send us
        // elsewhere, and a redirect would carry the token with it.
        let agent = ureq::AgentBuilder::new()
            .timeout(SERVER_API_TIMEOUT)
            .redirects(0)
            .build();
        RealServerApiTransport { base, agent }
    }

    fn post(&self, path: &str, body: &Value) -> Result<Vec<u8>> {
        let url = format!("{}{}", self.base, path);
        let response = self
            .agent
            .post(&url)
            .set("X-platform", PLATFORM_NAME)
            .set("Content-Type", "application/json")
            .send_string(&body.to_string());
        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::Status(status, _)) => {
                return err(ErrorCode::AppError, format!("{url} answered HTTP {status}"));
            }
            Err(ureq::Error::Transport(transport)) => {
                return err(ErrorCode::TargetUnreachable, format!("{url}: {transport}"));
            }
        };
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(RESPONSE_BYTES_MAX + 1)
            .read_to_end(&mut bytes)
            .map_err(|cause| {
                crate::error::AcheError::new(
                    ErrorCode::TargetUnreachable,
                    format!("{url}: {cause}"),
                )
            })?;
        if bytes.len() as u64 > RESPONSE_BYTES_MAX {
            return err(
                ErrorCode::ResponseInvalid,
                format!("response exceeds {RESPONSE_BYTES_MAX} bytes"),
            );
        }
        Ok(bytes)
    }
}

impl Default for RealServerApiTransport {
    fn default() -> RealServerApiTransport {
        RealServerApiTransport::new()
    }
}

impl ServerApiTransport for RealServerApiTransport {
    fn send_command(&self, body: &Value) -> Result<()> {
        let bytes = self.post(SERVER_API_COMMAND_PATH, body)?;
        parse_command_ack(&bytes)
    }

    fn get_toys(&self, body: &Value) -> Result<Vec<Toy>> {
        let bytes = self.post(SERVER_API_COMMAND_PATH, body)?;
        parse_get_toys(&bytes)
    }

    fn get_qr_code(&self, token: &str) -> Result<QrCode> {
        let bytes = self.post(SERVER_API_QR_PATH, &json!({ "token": token }))?;
        parse_qr_code(&bytes)
    }
}

/// Parse a getQrCode answer: the `code` 200 envelope with
/// `data.qrCode` carrying the code to scan. Anything else is
/// `response_invalid` — a pairing invitation is never invented.
pub fn parse_qr_code(body: &[u8]) -> Result<QrCode> {
    let root: Value = serde_json::from_slice(body).map_err(|cause| {
        crate::error::AcheError::new(ErrorCode::ResponseInvalid, format!("not JSON: {cause}"))
    })?;
    let Some(code) = root.get("code").and_then(Value::as_i64) else {
        return err(ErrorCode::ResponseInvalid, "no numeric `code` field");
    };
    if code != 200 {
        return err(ErrorCode::AppError, format!("Lovense API code {code}"));
    }
    let Some(qr_code) = root
        .get("data")
        .and_then(|data| data.get("qrCode"))
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    else {
        return err(ErrorCode::ResponseInvalid, "no `data.qrCode` field");
    };
    let code = root
        .get("data")
        .and_then(|data| data.get("code"))
        .and_then(Value::as_str)
        .map(bounded);
    Ok(QrCode {
        qr_code: bounded(qr_code),
        code,
    })
}

fn bounded(text: &str) -> String {
    let mut cut = text.len().min(TOY_FIELD_BYTES_MAX);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].to_string()
}

// ------------------------------------------------------------ provider

/// The loaded credential pair. In memory only; deliberately no
/// `Debug`, so a stray format of the provider can never print one.
#[derive(Clone)]
struct Credentials {
    token: String,
    uid: String,
}

/// The Server API provider: discovery and gate-plan execution over
/// Lovense's cloud. Constructed only when the operator enabled it;
/// the disabled case is an [`crate::provider::InertProvider`] (and a
/// [`NoPairing`]), never a half-wired instance of this type.
pub struct ServerApiProvider {
    transport: Box<dyn ServerApiTransport>,
    secrets: Box<dyn SecretSource>,
    token_entry: String,
    uid_entry: String,
    credentials: Mutex<Option<Credentials>>,
    qr_issued: Mutex<bool>,
}

impl ServerApiProvider {
    pub fn new(
        config: &Config,
        transport: Box<dyn ServerApiTransport>,
        secrets: Box<dyn SecretSource>,
    ) -> ServerApiProvider {
        ServerApiProvider {
            transport,
            secrets,
            token_entry: config.server_api_token_pass_entry.clone(),
            uid_entry: config.server_api_uid_pass_entry.clone(),
            credentials: Mutex::new(None),
            qr_issued: Mutex::new(false),
        }
    }

    /// Load the credential pair, once, and hold it in memory. A
    /// failed load is NOT cached: the store may be unlocked later,
    /// and the next call may succeed — each call attempts at most
    /// one load, so there is no retry loop.
    fn credentials(&self) -> Result<Credentials> {
        let mut cached = self
            .credentials
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(credentials) = cached.as_ref() {
            return Ok(credentials.clone());
        }
        let credentials = Credentials {
            token: self.secrets.load(&self.token_entry)?,
            uid: self.secrets.load(&self.uid_entry)?,
        };
        *cached = Some(credentials.clone());
        Ok(credentials)
    }

    /// The gate's stop body, built exactly as the local providers
    /// build theirs: stop is the intent the gate allows in every
    /// state, so the snapshot is deliberately the emptiest one.
    fn stop_body(&self, credentials: &Credentials) -> Value {
        let snapshot = GateSnapshot {
            armed: false,
            target_configured: false,
            ceiling: 0,
        };
        let Decision::Allow { body, .. } = decide(&snapshot, &OutputIntent::Stop) else {
            unreachable!("the gate always allows stop");
        };
        server_api_wire(&body, &credentials.token, &credentials.uid)
    }
}

/// Map one cloud-reported toy into the unified model, mirroring the
/// local Lovense mapping with this provider's id namespace.
fn serverapi_toy_to_device(toy: &Toy) -> Device {
    let (capabilities, capability_source) = match &toy.functions {
        Some(names) => {
            let capabilities = names
                .iter()
                .map(|name| Capability {
                    feature: name.clone(),
                    max: crate::decide::Feature::from_action(name).map(|feature| feature.api_max),
                    native: format!("lovense:{name}"),
                    steps: None,
                })
                .collect();
            (capabilities, "reported")
        }
        None => (Vec::new(), "unreported"),
    };
    Device {
        id: format!("serverapi:{}", toy.id),
        provider: ProviderId::ServerApi,
        name: toy.name.clone(),
        kind: toy.toy_type.clone(),
        battery: toy.battery,
        capabilities,
        capability_source,
    }
}

impl DeviceProvider for ServerApiProvider {
    fn probe(&self) -> ProviderProbe {
        ProviderProbe {
            provider: ProviderId::ServerApi,
            state: "ready",
            available: true,
            note: "server api enabled: commands relay through Lovense's cloud with \
                   credentials loaded from the password store at first use; \
                   discovery contacts the cloud"
                .to_string(),
        }
    }

    fn discover(&self) -> Result<Vec<Device>> {
        let credentials = self.credentials()?;
        let body = server_api_wire(
            &json!({ "command": "GetToys" }),
            &credentials.token,
            &credentials.uid,
        );
        Ok(self
            .transport
            .get_toys(&body)?
            .iter()
            .map(serverapi_toy_to_device)
            .collect())
    }

    fn execute(&self, plan: &AllowedOutput) -> Result<()> {
        if matches!(plan, AllowedOutput::Stop) {
            self.zero()?;
            return Ok(());
        }
        let credentials = self.credentials()?;
        let (body, _) = lovense_wire(plan);
        let wire = server_api_wire(&body, &credentials.token, &credentials.uid);
        self.transport.send_command(&wire)
    }

    fn zero(&self) -> Result<bool> {
        // Without credentials nothing could ever have been started
        // through this provider, so there is no target to zero: the
        // local zero stands. That is a clean `Ok(false)`, never an
        // error on the panic path.
        let Ok(credentials) = self.credentials() else {
            return Ok(false);
        };
        let body = self.stop_body(&credentials);
        self.transport.send_command(&body)?;
        Ok(true)
    }
}

impl PairingProvider for ServerApiProvider {
    fn pairing_state(&self) -> PairingState {
        // Presence only: each check loads (and discards) at most the
        // fact that a value exists; nothing is cached or reported.
        let token_present = self.secrets.load(&self.token_entry).is_ok();
        let uid_present = self.secrets.load(&self.uid_entry).is_ok();
        let qr_issued = *self
            .qr_issued
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        PairingState {
            enabled: true,
            token_present,
            uid_present,
            paired: token_present && uid_present,
            qr_issued,
            note: if token_present && uid_present {
                "paired: token and user id are both in the password store"
            } else if token_present {
                "developer token present; no user id yet — pair via request_qr and \
                 place the resulting user id in the password store"
            } else {
                "no developer token in the password store; the provider cannot \
                 contact the cloud"
            }
            .to_string(),
        }
    }

    fn request_qr(&self) -> Result<QrCode> {
        // The token alone is loaded here (pairing precedes any uid);
        // an unavailable token is the structured custody error.
        let token = self.secrets.load(&self.token_entry)?;
        let qr = self.transport.get_qr_code(&token)?;
        *self
            .qr_issued
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = true;
        Ok(qr)
    }
}

/// The two seams are wired from one instance in `main` (the device
/// provider and the pairing surface), so an `Arc` of the provider
/// implements both traits by delegation: the credential cache and
/// the QR-issued flag stay single-instance state.
impl DeviceProvider for Arc<ServerApiProvider> {
    fn probe(&self) -> ProviderProbe {
        (**self).probe()
    }

    fn discover(&self) -> Result<Vec<Device>> {
        (**self).discover()
    }

    fn execute(&self, plan: &AllowedOutput) -> Result<()> {
        (**self).execute(plan)
    }

    fn zero(&self) -> Result<bool> {
        (**self).zero()
    }
}

impl PairingProvider for Arc<ServerApiProvider> {
    fn pairing_state(&self) -> PairingState {
        (**self).pairing_state()
    }

    fn request_qr(&self) -> Result<QrCode> {
        (**self).request_qr()
    }
}

// ---------------------------------------------------- fixture (mock)

/// Fixture credentials for the mock provider. Plainly labelled
/// fixture values, used only inside this crate's mock mode and
/// tests; they are not, and could never be mistaken for, real ones.
pub const FIXTURE_TOKEN: &str = "fixture-token";
pub const FIXTURE_UID: &str = "fixture-uid";

/// The Server API fixture provider: the mock mode counterpart.
/// Serves fixture devices, records every wire body it is handed
/// (with fixture credentials merged, exactly as the real provider
/// would post), and can inject send failures for the session-drop
/// path. Labelled `fixture` everywhere its data appears.
#[derive(Clone)]
pub struct FakeServerApi {
    sent: Arc<Mutex<Vec<Value>>>,
    qr_calls: Arc<Mutex<u32>>,
    qr_issued: Arc<Mutex<bool>>,
    fail_sends: Arc<AtomicBool>,
}

impl Default for FakeServerApi {
    fn default() -> FakeServerApi {
        FakeServerApi::new()
    }
}

impl FakeServerApi {
    pub fn new() -> FakeServerApi {
        FakeServerApi {
            sent: Arc::new(Mutex::new(Vec::new())),
            qr_calls: Arc::new(Mutex::new(0)),
            qr_issued: Arc::new(Mutex::new(false)),
            fail_sends: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The fixture devices: a vibrate-only unit and a vibrate+rotate
    /// unit, mirroring the other providers' fixtures.
    pub fn fixture_devices() -> Vec<Device> {
        let capability = |feature: &str| Capability {
            feature: feature.to_string(),
            max: Some(20),
            native: format!("lovense:{feature}"),
            steps: None,
        };
        vec![
            Device {
                id: "serverapi:fixture-lush".to_string(),
                provider: ProviderId::ServerApi,
                name: "Fixture Lush (cloud)".to_string(),
                kind: Some("lush".to_string()),
                battery: Some(87),
                capabilities: vec![capability("Vibrate")],
                capability_source: "fixture",
            },
            Device {
                id: "serverapi:fixture-duo".to_string(),
                provider: ProviderId::ServerApi,
                name: "Fixture Duo (cloud)".to_string(),
                kind: Some("edge".to_string()),
                battery: Some(42),
                capabilities: vec![capability("Vibrate"), capability("Rotate")],
                capability_source: "fixture",
            },
        ]
    }

    /// A shared handle on the wire bodies recorded, in order.
    #[cfg(test)]
    pub fn sent_log(&self) -> Arc<Mutex<Vec<Value>>> {
        Arc::clone(&self.sent)
    }

    /// A shared handle on the QR-request count.
    #[cfg(test)]
    pub fn qr_log(&self) -> Arc<Mutex<u32>> {
        Arc::clone(&self.qr_calls)
    }

    /// A shared handle on the failure flag, so a test can inject
    /// send failures (the session-drop path) after the provider has
    /// been handed to the server.
    #[cfg(test)]
    pub fn fail_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.fail_sends)
    }

    fn record(&self, body: &Value) {
        let mut sent = self
            .sent
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        sent.push(body.clone());
    }

    fn stop_wire(&self) -> Value {
        let snapshot = GateSnapshot {
            armed: false,
            target_configured: false,
            ceiling: 0,
        };
        let Decision::Allow { body, .. } = decide(&snapshot, &OutputIntent::Stop) else {
            unreachable!("the gate always allows stop");
        };
        server_api_wire(&body, FIXTURE_TOKEN, FIXTURE_UID)
    }

    fn send(&self, body: &Value) -> Result<()> {
        // Record before the failure check: an attempted send is a
        // fact the battery asserts on, even when the drop swallows it.
        self.record(body);
        if self.fail_sends.load(Ordering::SeqCst) {
            return err(
                ErrorCode::TargetUnreachable,
                "fixture server api: send failed (injected session drop)",
            );
        }
        Ok(())
    }
}

impl DeviceProvider for FakeServerApi {
    fn probe(&self) -> ProviderProbe {
        ProviderProbe {
            provider: ProviderId::ServerApi,
            state: "mock",
            available: true,
            note: "fixture provider: fixture devices only; no cloud, device, or app \
                   is contacted"
                .to_string(),
        }
    }

    fn discover(&self) -> Result<Vec<Device>> {
        Ok(FakeServerApi::fixture_devices())
    }

    fn execute(&self, plan: &AllowedOutput) -> Result<()> {
        if matches!(plan, AllowedOutput::Stop) {
            self.zero()?;
            return Ok(());
        }
        let (body, _) = lovense_wire(plan);
        let wire = server_api_wire(&body, FIXTURE_TOKEN, FIXTURE_UID);
        self.send(&wire)
    }

    fn zero(&self) -> Result<bool> {
        let body = self.stop_wire();
        self.send(&body)?;
        Ok(true)
    }
}

impl PairingProvider for FakeServerApi {
    fn pairing_state(&self) -> PairingState {
        PairingState {
            enabled: true,
            token_present: true,
            uid_present: true,
            paired: true,
            qr_issued: *self
                .qr_issued
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
            note: "fixture pairing: no cloud is contacted".to_string(),
        }
    }

    fn request_qr(&self) -> Result<QrCode> {
        *self
            .qr_calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) += 1;
        *self
            .qr_issued
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = true;
        Ok(QrCode {
            qr_code: "fixture-qr-code".to_string(),
            code: Some("000000".to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SERVER_API_TOKEN_PASS_ENTRY_DEFAULT, SERVER_API_UID_PASS_ENTRY_DEFAULT};
    use crate::decide::{FunctionIntent, decide_plan};
    use crate::test_stub::AppStub;

    const TOKEN_ENTRY: &str = SERVER_API_TOKEN_PASS_ENTRY_DEFAULT;
    const UID_ENTRY: &str = SERVER_API_UID_PASS_ENTRY_DEFAULT;

    fn snapshot() -> GateSnapshot {
        GateSnapshot {
            armed: true,
            target_configured: true,
            ceiling: 20,
        }
    }

    fn intent(action: &str, strength: i64, time_sec: i64, toy: &str) -> OutputIntent {
        OutputIntent::Function(FunctionIntent {
            feature: action.into(),
            strength,
            time_sec,
            toy: Some(toy.into()),
        })
    }

    /// A canned cloud transport: records bodies, serves fixed toys,
    /// and (when `refuse_functions` is set) refuses every non-Stop
    /// command the way a cloud refusal would surface.
    struct CannedTransport {
        sent: Mutex<Vec<Value>>,
        toys: Vec<Toy>,
        refuse_functions: bool,
    }

    impl CannedTransport {
        fn new(toys: Vec<Toy>) -> CannedTransport {
            CannedTransport {
                sent: Mutex::new(Vec::new()),
                toys,
                refuse_functions: false,
            }
        }
    }

    impl ServerApiTransport for CannedTransport {
        fn send_command(&self, body: &Value) -> Result<()> {
            self.sent
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(body.clone());
            if self.refuse_functions && body["action"] != "Stop" {
                return err(ErrorCode::AppError, "canned cloud refusal");
            }
            Ok(())
        }

        fn get_toys(&self, _body: &Value) -> Result<Vec<Toy>> {
            Ok(self.toys.clone())
        }

        fn get_qr_code(&self, _token: &str) -> Result<QrCode> {
            Ok(QrCode {
                qr_code: "canned-qr".to_string(),
                code: None,
            })
        }
    }

    fn provider_with(
        transport: CannedTransport,
        secrets: FixtureSecretSource,
    ) -> ServerApiProvider {
        ServerApiProvider::new(&Config::default(), Box::new(transport), Box::new(secrets))
    }

    fn good_secrets() -> FixtureSecretSource {
        FixtureSecretSource::with(&[(TOKEN_ENTRY, "tok-value"), (UID_ENTRY, "uid-value")])
    }

    // --- validation ---------------------------------------------------

    #[test]
    fn wire_merge_adds_auth_and_touches_nothing_else() {
        let body = json!({"command": "Function", "action": "Vibrate:7", "timeSec": 3, "toy": "a1", "apiVer": 1});
        let wire = server_api_wire(&body, "tok", "uid");
        assert_eq!(wire["command"], "Function");
        assert_eq!(wire["action"], "Vibrate:7");
        assert_eq!(wire["timeSec"], 3);
        assert_eq!(wire["toy"], "a1");
        assert_eq!(wire["apiVer"], 1);
        assert_eq!(wire["token"], "tok");
        assert_eq!(wire["uid"], "uid");
        // The gate's body itself is unchanged (merge works on a copy).
        assert!(body.get("token").is_none());
    }

    #[test]
    fn fixture_devices_carry_serverapi_ids_and_preset_support() {
        let devices = FakeServerApi::fixture_devices();
        assert_eq!(devices[0].id, "serverapi:fixture-lush");
        assert_eq!(devices[0].provider, ProviderId::ServerApi);
        assert_eq!(devices[0].capability_source, "fixture");
        assert_eq!(devices[0].caps().max_for("Vibrate"), Some(20));
        assert_eq!(devices[0].caps().max_for("Rotate"), None);
        assert_eq!(devices[1].caps().max_for("Rotate"), Some(20));
        // The cloud route carries the documented Preset vocabulary.
        assert!(devices[0].caps().supports_presets());
    }

    #[test]
    fn fixture_execute_posts_the_gate_body_with_credentials_merged() {
        let provider = FakeServerApi::new();
        let sent = provider.sent_log();
        let caps = FakeServerApi::fixture_devices()[0].caps();
        let plan =
            decide_plan(&snapshot(), &intent("Vibrate", 7, 3, "fixture-lush"), &caps).unwrap();
        provider.execute(&plan).unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["command"], "Function");
        assert_eq!(sent[0]["action"], "Vibrate:7");
        assert_eq!(sent[0]["toy"], "fixture-lush");
        assert_eq!(sent[0]["token"], FIXTURE_TOKEN);
        assert_eq!(sent[0]["uid"], FIXTURE_UID);
    }

    #[test]
    fn fixture_zero_posts_the_gate_stop() {
        let provider = FakeServerApi::new();
        let sent = provider.sent_log();
        assert!(provider.zero().unwrap());
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["command"], "Function");
        assert_eq!(sent[0]["action"], "Stop");
        assert_eq!(sent[0]["token"], FIXTURE_TOKEN);
    }

    #[test]
    fn fixture_pairing_reports_paired_and_issues_fixture_qr() {
        let provider = FakeServerApi::new();
        let state = provider.pairing_state();
        assert!(state.enabled && state.paired && !state.qr_issued);
        let qr = provider.request_qr().unwrap();
        assert_eq!(qr.qr_code, "fixture-qr-code");
        assert!(provider.pairing_state().qr_issued);
        // A QR request is not a device command: nothing was "sent".
        assert!(provider.sent_log().lock().unwrap().is_empty());
        assert_eq!(*provider.qr_log().lock().unwrap(), 1);
    }

    #[test]
    fn qr_answer_parses_the_documented_shape() {
        let body = br#"{"code":200,"data":{"qrCode":"https://example.invalid/qr/abc","code":"123456"},"type":"OK"}"#;
        let qr = parse_qr_code(body).unwrap();
        assert_eq!(qr.qr_code, "https://example.invalid/qr/abc");
        assert_eq!(qr.code.as_deref(), Some("123456"));
    }

    #[test]
    fn real_transport_get_toys_against_loopback_stub() {
        let documented = r#"{"code":200,"data":{"toys":"{\"a1\":{\"id\":\"a1\",\"name\":\"lush\",\"nickName\":\"\",\"toyType\":\"lush\",\"battery\":64,\"status\":\"1\"}}"},"type":"OK"}"#;
        let mut stub = AppStub::start(documented, 1, 200);
        let transport =
            RealServerApiTransport::with_base(format!("http://127.0.0.1:{}", stub.port));
        let body = server_api_wire(&json!({ "command": "GetToys" }), "tok", "uid");
        let toys = transport.get_toys(&body).unwrap();
        assert_eq!(toys.len(), 1);
        assert_eq!(toys[0].id, "a1");
        let received = stub.received();
        assert_eq!(received["path"], SERVER_API_COMMAND_PATH);
        let posted: Value = serde_json::from_str(received["body"].as_str().unwrap()).unwrap();
        assert_eq!(posted["command"], "GetToys");
        assert_eq!(posted["token"], "tok");
        assert_eq!(posted["uid"], "uid");
    }

    #[test]
    fn real_transport_send_command_against_loopback_stub() {
        let mut stub = AppStub::start(r#"{"code":200,"type":"OK"}"#, 1, 200);
        let transport =
            RealServerApiTransport::with_base(format!("http://127.0.0.1:{}", stub.port));
        let body = server_api_wire(
            &json!({"command": "Function", "action": "Stop", "timeSec": 0, "toy": "", "apiVer": 1}),
            "tok",
            "uid",
        );
        transport.send_command(&body).unwrap();
        let received = stub.received();
        assert_eq!(received["path"], SERVER_API_COMMAND_PATH);
        assert_eq!(received["platform"], PLATFORM_NAME);
    }

    // --- adversarial --------------------------------------------------

    #[test]
    fn unavailable_secret_degrades_structurally_never_panics() {
        let provider = provider_with(
            CannedTransport::new(Vec::new()),
            FixtureSecretSource::failing(),
        );
        // The provider is configured and could serve with custody;
        // the failure is the secret, and it is named as such.
        assert!(provider.probe().available);
        let error = provider.discover().unwrap_err();
        assert_eq!(error.code, ErrorCode::SecretUnavailable);
        assert!(error.message.contains(TOKEN_ENTRY), "{}", error.message);
        // The error names the path (configuration), never a value.
        assert!(!error.message.contains("tok-value"));
        let caps = FakeServerApi::fixture_devices()[0].caps();
        let plan =
            decide_plan(&snapshot(), &intent("Vibrate", 5, 3, "fixture-lush"), &caps).unwrap();
        assert_eq!(
            provider.execute(&plan).unwrap_err().code,
            ErrorCode::SecretUnavailable
        );
        // Zero with no custody: nothing could have started, so the
        // local zero stands and the panic path stays clean.
        assert!(!provider.zero().unwrap());
        // Pairing state reports absence, and a QR request refuses
        // with the same structured error.
        let state = provider.pairing_state();
        assert!(state.enabled && !state.token_present && !state.paired);
        assert_eq!(
            provider.request_qr().unwrap_err().code,
            ErrorCode::SecretUnavailable
        );
    }

    #[test]
    fn clamped_strength_is_what_reaches_the_wire() {
        let provider = FakeServerApi::new();
        let sent = provider.sent_log();
        let caps = FakeServerApi::fixture_devices()[0].caps();
        // Strength 99 against a ceiling of 20: the GATE clamps, and
        // the credential merge must not un-clamp or reshape anything.
        let plan = decide_plan(
            &snapshot(),
            &intent("Vibrate", 99, 3, "fixture-lush"),
            &caps,
        )
        .unwrap();
        assert!(plan.clamped());
        provider.execute(&plan).unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(sent[0]["action"], "Vibrate:20");
        assert_eq!(sent[0]["timeSec"], 3);
    }

    #[test]
    fn malformed_qr_answers_are_refused_not_invented() {
        for (body, code) in [
            ("", ErrorCode::ResponseInvalid),
            ("{oops", ErrorCode::ResponseInvalid),
            (r#"{"code":"200"}"#, ErrorCode::ResponseInvalid),
            (r#"{"code":200,"data":{}}"#, ErrorCode::ResponseInvalid),
            (
                r#"{"code":200,"data":{"qrCode":""}}"#,
                ErrorCode::ResponseInvalid,
            ),
            (r#"{"code":403,"type":"error"}"#, ErrorCode::AppError),
        ] {
            assert_eq!(
                parse_qr_code(body.as_bytes()).unwrap_err().code,
                code,
                "body {body}"
            );
        }
    }

    #[test]
    fn disabled_pairing_surface_refuses_qr_and_reports_disabled() {
        let pairing = NoPairing::disabled("server api provider is disabled by configuration");
        let state = pairing.pairing_state();
        assert!(!state.enabled && !state.paired);
        assert_eq!(
            pairing.request_qr().unwrap_err().code,
            ErrorCode::ProviderUnavailable
        );
    }

    #[test]
    fn stop_still_zeroes_after_a_refused_execute() {
        let mut transport = CannedTransport::new(Vec::new());
        transport.refuse_functions = true;
        let provider = provider_with(transport, good_secrets());
        let caps = FakeServerApi::fixture_devices()[0].caps();
        let plan =
            decide_plan(&snapshot(), &intent("Vibrate", 5, 3, "fixture-lush"), &caps).unwrap();
        // The cloud refuses the function; the error surfaces typed.
        assert_eq!(
            provider.execute(&plan).unwrap_err().code,
            ErrorCode::AppError
        );
        // Stop goes through the same transport afterwards and lands:
        // a refused command never wedges the panic path.
        assert!(provider.zero().unwrap());
    }
}
