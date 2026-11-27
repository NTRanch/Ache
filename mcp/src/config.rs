//! Fail-closed configuration. Unknown keys, out-of-range values, and
//! malformed hosts are refused with a structured error; nothing is
//! silently defaulted past validation.
//!
//! S1 note: `intensity_ceiling` and `watchdog_secs` are parsed, validated,
//! and reported only. They bind output from S2, when an output path exists.

use std::path::Path;

use serde::Deserialize;

use crate::error::{ErrorCode, Result, err};
use crate::rumble::{self, Mapping, Motor, RumbleConfig, SourceRegistration};

/// Environment variable naming the config file. Unset means defaults.
/// (Renamed from `LOVENSE_MCP_CONFIG` in the ache unification; the old
/// name is no longer read — see the merge report's breaking changes.)
pub const CONFIG_ENV: &str = "ACHE_MCP_CONFIG";
/// Default device-scan window for the Buttplug provider, milliseconds.
pub const BUTTPLUG_SCAN_MS_DEFAULT: u32 = 1500;
pub const BUTTPLUG_SCAN_MS_MIN: u32 = 100;
pub const BUTTPLUG_SCAN_MS_MAX: u32 = 10_000;
/// Largest config file accepted, in bytes.
pub const CONFIG_BYTES_MAX: u64 = 64 * 1024;
/// Default password-store path of the Lovense developer token. The
/// config names a path, never a value: credentials live in the
/// password store and are loaded at runtime (D-1451).
pub const SERVER_API_TOKEN_PASS_ENTRY_DEFAULT: &str = "lovense/ntr/token";
/// Default password-store path of the paired user id, placed there
/// by the Owner as the final step of QR pairing (D-1451).
pub const SERVER_API_UID_PASS_ENTRY_DEFAULT: &str = "lovense/ntr/uid";
/// Longest password-store path accepted, in bytes.
pub const PASS_ENTRY_BYTES_MAX: usize = 128;
/// Highest intensity the Lovense API accepts; also the default ceiling.
pub const INTENSITY_CEILING_MAX: u8 = 20;
pub const WATCHDOG_SECS_MIN: u8 = 1;
pub const WATCHDOG_SECS_MAX: u8 = 60;
/// Longest DNS name, in bytes.
pub const HOST_BYTES_MAX: usize = 253;
pub const PORT_DEFAULT: u16 = 20010;
pub const WATCHDOG_SECS_DEFAULT: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    Mock,
    Real,
}

impl TransportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TransportKind::Mock => "mock",
            TransportKind::Real => "real",
        }
    }
}

/// Validated configuration. Only constructed through `from_toml_str`,
/// `load`, or `Default`, so every instance satisfies the field ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub transport: TransportKind,
    /// Empty means no target: no network call is ever made.
    pub host: String,
    pub port: u16,
    pub use_https: bool,
    pub intensity_ceiling: u8,
    pub watchdog_secs: u8,
    /// Whether the embedded Buttplug provider runs at all. Default on:
    /// the unified server serves both providers unless told otherwise.
    pub buttplug_enabled: bool,
    /// How long one Buttplug discovery scan listens, in milliseconds.
    pub buttplug_scan_ms: u32,
    /// Whether the Lovense Server API provider (the cloud route,
    /// D-1451) runs at all. Default OFF: local routes are the default
    /// and this provider is an explicit operator opt-in.
    pub server_api_enabled: bool,
    /// Password-store path of the developer token. A path, never a
    /// value; the credential itself is loaded at runtime.
    pub server_api_token_pass_entry: String,
    /// Password-store path of the paired user id.
    pub server_api_uid_pass_entry: String,
    /// The `ache.rumble/v1` event-source input. Default OFF.
    pub rumble: RumbleConfig,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            transport: TransportKind::Mock,
            host: String::new(),
            port: PORT_DEFAULT,
            use_https: false,
            intensity_ceiling: INTENSITY_CEILING_MAX,
            watchdog_secs: WATCHDOG_SECS_DEFAULT,
            buttplug_enabled: true,
            buttplug_scan_ms: BUTTPLUG_SCAN_MS_DEFAULT,
            server_api_enabled: false,
            server_api_token_pass_entry: SERVER_API_TOKEN_PASS_ENTRY_DEFAULT.to_string(),
            server_api_uid_pass_entry: SERVER_API_UID_PASS_ENTRY_DEFAULT.to_string(),
            rumble: RumbleConfig::default(),
        }
    }
}

// Integers are read as i64 so out-of-range values reach our validation
// (and its error message) instead of a generic deserializer overflow.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    transport: Option<String>,
    host: Option<String>,
    port: Option<i64>,
    use_https: Option<bool>,
    intensity_ceiling: Option<i64>,
    watchdog_secs: Option<i64>,
    buttplug: Option<RawButtplug>,
    server_api: Option<RawServerApi>,
    rumble: Option<RawRumble>,
}

/// The `[rumble]` section: the `ache.rumble/v1` input. Sources and
/// mappings are declared here and nowhere else; unknown keys refuse.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRumble {
    enabled: Option<bool>,
    tick_ms: Option<i64>,
    socket: Option<String>,
    #[serde(default)]
    source: Vec<RawRumbleSource>,
    #[serde(default)]
    map: Vec<RawRumbleMap>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRumbleSource {
    id: String,
    gain_percent: Option<i64>,
    ceiling_percent: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRumbleMap {
    source: String,
    motor: String,
    device: String,
    function: String,
    max_strength: Option<i64>,
}

/// The `[buttplug]` section: the embedded Buttplug provider's knobs.
/// Same fail-closed discipline as the top level — unknown keys refuse.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawButtplug {
    enabled: Option<bool>,
    scan_ms: Option<i64>,
}

/// The `[server_api]` section: the Server API provider's knobs
/// (D-1451). It can name password-store paths only — a credential
/// value has no field to land in, and an unknown key (a stray
/// `token = "..."` above all) refuses the whole config.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServerApi {
    enabled: Option<bool>,
    token_pass_entry: Option<String>,
    uid_pass_entry: Option<String>,
}

impl Config {
    pub fn from_toml_str(text: &str) -> Result<Config> {
        let raw: RawConfig = toml::from_str(text).map_err(|cause| {
            crate::error::AcheError::new(ErrorCode::ConfigInvalid, cause.to_string())
        })?;
        let defaults = Config::default();
        let transport = match raw.transport.as_deref() {
            None | Some("mock") => TransportKind::Mock,
            Some("real") => TransportKind::Real,
            Some(other) => return invalid("transport", format!("`{other}` is not mock or real")),
        };
        let host = match raw.host {
            None => defaults.host,
            Some(host) => validate_host(host)?,
        };
        let port = match raw.port {
            None => defaults.port,
            Some(port) => match u16::try_from(port) {
                Ok(port) if port >= 1 => port,
                _ => return invalid("port", format!("{port} is outside 1..=65535")),
            },
        };
        let intensity_ceiling = match raw.intensity_ceiling {
            None => defaults.intensity_ceiling,
            Some(ceiling) => match u8::try_from(ceiling) {
                Ok(ceiling) if ceiling <= INTENSITY_CEILING_MAX => ceiling,
                _ => return invalid("intensity_ceiling", format!("{ceiling} is outside 0..=20")),
            },
        };
        let watchdog_secs = match raw.watchdog_secs {
            None => defaults.watchdog_secs,
            Some(secs) => match u8::try_from(secs) {
                Ok(secs) if (WATCHDOG_SECS_MIN..=WATCHDOG_SECS_MAX).contains(&secs) => secs,
                _ => return invalid("watchdog_secs", format!("{secs} is outside 1..=60")),
            },
        };
        let (buttplug_enabled, buttplug_scan_ms) = match raw.buttplug {
            None => (defaults.buttplug_enabled, defaults.buttplug_scan_ms),
            Some(section) => {
                let scan_ms = match section.scan_ms {
                    None => defaults.buttplug_scan_ms,
                    Some(ms) => match u32::try_from(ms) {
                        Ok(ms) if (BUTTPLUG_SCAN_MS_MIN..=BUTTPLUG_SCAN_MS_MAX).contains(&ms) => ms,
                        _ => {
                            return invalid(
                                "buttplug.scan_ms",
                                format!(
                                    "{ms} is outside {BUTTPLUG_SCAN_MS_MIN}..={BUTTPLUG_SCAN_MS_MAX}"
                                ),
                            );
                        }
                    },
                };
                (
                    section.enabled.unwrap_or(defaults.buttplug_enabled),
                    scan_ms,
                )
            }
        };
        let (server_api_enabled, server_api_token_pass_entry, server_api_uid_pass_entry) =
            match raw.server_api {
                None => (
                    defaults.server_api_enabled,
                    defaults.server_api_token_pass_entry.clone(),
                    defaults.server_api_uid_pass_entry.clone(),
                ),
                Some(section) => {
                    let token_entry = match section.token_pass_entry {
                        None => defaults.server_api_token_pass_entry.clone(),
                        Some(entry) => validate_pass_entry("server_api.token_pass_entry", entry)?,
                    };
                    let uid_entry = match section.uid_pass_entry {
                        None => defaults.server_api_uid_pass_entry.clone(),
                        Some(entry) => validate_pass_entry("server_api.uid_pass_entry", entry)?,
                    };
                    (
                        section.enabled.unwrap_or(defaults.server_api_enabled),
                        token_entry,
                        uid_entry,
                    )
                }
            };
        let rumble = match raw.rumble {
            None => RumbleConfig::default(),
            Some(section) => validate_rumble(section)?,
        };
        Ok(Config {
            transport,
            host,
            port,
            use_https: raw.use_https.unwrap_or(defaults.use_https),
            intensity_ceiling,
            watchdog_secs,
            buttplug_enabled,
            buttplug_scan_ms,
            server_api_enabled,
            server_api_token_pass_entry,
            server_api_uid_pass_entry,
            rumble,
        })
    }

    pub fn load(path: &Path) -> Result<Config> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
                return err(
                    ErrorCode::ConfigMissing,
                    format!("config {} does not exist", path.display()),
                );
            }
            Err(cause) => {
                return err(
                    ErrorCode::ConfigUnreadable,
                    format!("config {}: {cause}", path.display()),
                );
            }
        };
        if !metadata.is_file() {
            return err(
                ErrorCode::ConfigUnreadable,
                format!("config {} is not a file", path.display()),
            );
        }
        if metadata.len() > CONFIG_BYTES_MAX {
            return err(
                ErrorCode::ConfigInvalid,
                format!("config {} exceeds {CONFIG_BYTES_MAX} bytes", path.display()),
            );
        }
        let text = std::fs::read_to_string(path).map_err(|cause| {
            crate::error::AcheError::new(
                ErrorCode::ConfigUnreadable,
                format!("config {}: {cause}", path.display()),
            )
        })?;
        Config::from_toml_str(&text)
    }
}

/// A host is a bare DNS name or IPv4 address: ASCII letters, digits, `.`
/// and `-` only. That excludes schemes, paths, userinfo, ports, and
/// whitespace, so a host value can never rewrite the request URL.
fn validate_host(host: String) -> Result<String> {
    if host.is_empty() {
        return Ok(host);
    }
    if host.len() > HOST_BYTES_MAX {
        return invalid("host", format!("longer than {HOST_BYTES_MAX} bytes"));
    }
    let allowed = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-';
    if !host.bytes().all(allowed) || host.starts_with(['.', '-']) {
        return invalid(
            "host",
            format!("`{host}` must be a bare hostname or IPv4 address"),
        );
    }
    Ok(host)
}

/// A password-store path is a relative entry name: ASCII letters,
/// digits, `/`, `-`, `_`, and `.` only, never absolute, never with a
/// `..` component, never empty. That excludes traversal out of the
/// store and anything that could be mistaken for a credential value
/// with whitespace or punctuation in it.
fn validate_pass_entry(field: &str, entry: String) -> Result<String> {
    if entry.is_empty() || entry.len() > PASS_ENTRY_BYTES_MAX {
        return invalid(field, format!("must be 1..={PASS_ENTRY_BYTES_MAX} bytes"));
    }
    let allowed =
        |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.');
    if !entry.bytes().all(allowed)
        || entry.starts_with('/')
        || entry.split('/').any(|part| part.is_empty() || part == "..")
    {
        return invalid(
            field,
            format!("`{entry}` must be a relative password-store path"),
        );
    }
    Ok(entry)
}

/// A config identifier (source id, device id): printable, bounded.
fn validate_ident(field: &str, value: &str, max: usize) -> Result<()> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return invalid(
            field,
            format!("must be 1..={max} bytes with no control characters"),
        );
    }
    Ok(())
}

fn validate_rumble(section: RawRumble) -> Result<RumbleConfig> {
    let tick_ms = match section.tick_ms {
        None => rumble::TICK_MS_DEFAULT,
        Some(ms) => match u64::try_from(ms) {
            Ok(ms) if (rumble::TICK_MS_MIN..=rumble::TICK_MS_MAX).contains(&ms) => ms,
            _ => {
                return invalid(
                    "rumble.tick_ms",
                    format!(
                        "{ms} is outside {}..={}",
                        rumble::TICK_MS_MIN,
                        rumble::TICK_MS_MAX
                    ),
                );
            }
        },
    };
    let socket = match section.socket {
        None => None,
        Some(path) => {
            if !path.starts_with('/')
                || path.len() > rumble::SOCKET_PATH_BYTES_MAX
                || path.chars().any(char::is_control)
                || path.split('/').any(|part| part == "..")
            {
                return invalid(
                    "rumble.socket",
                    format!(
                        "must be an absolute path of at most {} bytes with no `..`",
                        rumble::SOCKET_PATH_BYTES_MAX
                    ),
                );
            }
            Some(path)
        }
    };
    if section.source.len() > rumble::SOURCES_MAX {
        return invalid(
            "rumble.source",
            format!("at most {} sources", rumble::SOURCES_MAX),
        );
    }
    if section.map.len() > rumble::MAPPINGS_MAX {
        return invalid(
            "rumble.map",
            format!("at most {} mappings", rumble::MAPPINGS_MAX),
        );
    }
    let mut sources: Vec<SourceRegistration> = Vec::new();
    for raw in section.source {
        validate_ident("rumble.source.id", &raw.id, rumble::SOURCE_ID_BYTES_MAX)?;
        if sources.iter().any(|source| source.id == raw.id) {
            return invalid(
                "rumble.source.id",
                format!("`{}` is registered twice", raw.id),
            );
        }
        let gain_percent = match raw.gain_percent {
            None => 100,
            Some(gain) => match u16::try_from(gain) {
                Ok(gain)
                    if (rumble::GAIN_PERCENT_MIN..=rumble::GAIN_PERCENT_MAX).contains(&gain) =>
                {
                    gain
                }
                _ => {
                    return invalid(
                        "rumble.source.gain_percent",
                        format!(
                            "{gain} is outside {}..={}",
                            rumble::GAIN_PERCENT_MIN,
                            rumble::GAIN_PERCENT_MAX
                        ),
                    );
                }
            },
        };
        let ceiling_percent = match raw.ceiling_percent {
            None => rumble::CEILING_PERCENT_MAX,
            Some(ceiling) => match u8::try_from(ceiling) {
                Ok(ceiling) if ceiling <= rumble::CEILING_PERCENT_MAX => ceiling,
                _ => {
                    return invalid(
                        "rumble.source.ceiling_percent",
                        format!("{ceiling} is outside 0..={}", rumble::CEILING_PERCENT_MAX),
                    );
                }
            },
        };
        sources.push(SourceRegistration {
            id: raw.id,
            gain_percent,
            ceiling_percent,
        });
    }
    let mut mappings: Vec<Mapping> = Vec::new();
    for raw in section.map {
        if !sources.iter().any(|source| source.id == raw.source) {
            return invalid(
                "rumble.map.source",
                format!("`{}` is not a registered source", raw.source),
            );
        }
        let Some(motor) = Motor::parse(&raw.motor) else {
            return invalid(
                "rumble.map.motor",
                format!("`{}` is not heavy or light", raw.motor),
            );
        };
        validate_ident(
            "rumble.map.device",
            &raw.device,
            rumble::DEVICE_ID_BYTES_MAX,
        )?;
        if !rumble::FUNCTIONS.contains(&raw.function.as_str()) {
            return invalid(
                "rumble.map.function",
                format!(
                    "`{}` is not one of {}",
                    raw.function,
                    rumble::FUNCTIONS.join(", ")
                ),
            );
        }
        let max_strength = match raw.max_strength {
            None => rumble::MAX_STRENGTH_MAX,
            Some(max) => match u8::try_from(max) {
                Ok(max) if max <= rumble::MAX_STRENGTH_MAX => max,
                _ => {
                    return invalid(
                        "rumble.map.max_strength",
                        format!("{max} is outside 0..={}", rumble::MAX_STRENGTH_MAX),
                    );
                }
            },
        };
        let mapping = Mapping {
            source: raw.source,
            motor,
            device: raw.device,
            function: raw.function,
            max_strength,
        };
        if mappings.iter().any(|m| {
            (&m.source, m.motor, &m.device, &m.function)
                == (
                    &mapping.source,
                    mapping.motor,
                    &mapping.device,
                    &mapping.function,
                )
        }) {
            return invalid("rumble.map", "the same mapping is declared twice");
        }
        mappings.push(mapping);
    }
    let enabled = section.enabled.unwrap_or(false);
    if enabled && mappings.is_empty() {
        return invalid("rumble.enabled", "enabled with no [[rumble.map]] entries");
    }
    Ok(RumbleConfig {
        enabled,
        tick_ms,
        socket,
        sources,
        mappings,
    })
}

fn invalid<T>(field: &str, reason: impl AsRef<str>) -> Result<T> {
    err(
        ErrorCode::ConfigInvalid,
        format!("config field `{field}`: {}", reason.as_ref()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(text: &str) -> crate::error::AcheError {
        match Config::from_toml_str(text) {
            Ok(config) => panic!("config was accepted: {config:?}"),
            Err(error) => error,
        }
    }

    // --- validation ---

    #[test]
    fn empty_file_yields_documented_defaults() {
        let config = Config::from_toml_str("").unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.transport, TransportKind::Mock);
        assert_eq!(config.host, "");
        assert_eq!(config.port, 20010);
        assert!(!config.use_https);
        assert_eq!(config.intensity_ceiling, 20);
        assert_eq!(config.watchdog_secs, 5);
    }

    #[test]
    fn full_real_config_is_accepted() {
        let text = "transport = \"real\"\nhost = \"192.168.1.5\"\nport = 30010\n\
                    use_https = true\nintensity_ceiling = 0\nwatchdog_secs = 60\n";
        let config = Config::from_toml_str(text).unwrap();
        assert_eq!(config.transport, TransportKind::Real);
        assert_eq!(config.host, "192.168.1.5");
        assert_eq!(config.port, 30010);
        assert!(config.use_https);
        assert_eq!(config.intensity_ceiling, 0);
        assert_eq!(config.watchdog_secs, 60);
    }

    #[test]
    fn range_edges_are_accepted() {
        let text = "port = 1\nintensity_ceiling = 20\nwatchdog_secs = 1\n";
        let config = Config::from_toml_str(text).unwrap();
        assert_eq!(
            (config.port, config.intensity_ceiling, config.watchdog_secs),
            (1, 20, 1)
        );
        assert_eq!(Config::from_toml_str("port = 65535").unwrap().port, 65535);
    }

    #[test]
    fn dns_hostname_is_accepted() {
        let config = Config::from_toml_str("host = \"192-168-1-5.lovense.club\"").unwrap();
        assert_eq!(config.host, "192-168-1-5.lovense.club");
    }

    #[test]
    fn buttplug_section_parses_and_defaults() {
        let config = Config::from_toml_str("[buttplug]\nenabled = false\nscan_ms = 250\n").unwrap();
        assert!(!config.buttplug_enabled);
        assert_eq!(config.buttplug_scan_ms, 250);
        let config = Config::from_toml_str("[buttplug]\n").unwrap();
        assert!(config.buttplug_enabled);
        assert_eq!(config.buttplug_scan_ms, BUTTPLUG_SCAN_MS_DEFAULT);
        let config = Config::from_toml_str("").unwrap();
        assert!(config.buttplug_enabled, "the provider is on by default");
    }

    #[test]
    fn server_api_section_defaults_off_with_store_paths() {
        // The D-1451 posture: off by default, local routes default;
        // the section names password-store paths, never values.
        let config = Config::from_toml_str("").unwrap();
        assert!(!config.server_api_enabled);
        assert_eq!(
            config.server_api_token_pass_entry,
            SERVER_API_TOKEN_PASS_ENTRY_DEFAULT
        );
        assert_eq!(
            config.server_api_uid_pass_entry,
            SERVER_API_UID_PASS_ENTRY_DEFAULT
        );
        let config = Config::from_toml_str("[server_api]\n").unwrap();
        assert!(!config.server_api_enabled, "an empty section stays off");
        let config = Config::from_toml_str(
            "[server_api]\nenabled = true\ntoken_pass_entry = \"lovense/dev/token\"\n",
        )
        .unwrap();
        assert!(config.server_api_enabled);
        assert_eq!(config.server_api_token_pass_entry, "lovense/dev/token");
        assert_eq!(
            config.server_api_uid_pass_entry,
            SERVER_API_UID_PASS_ENTRY_DEFAULT
        );
    }

    #[test]
    fn missing_file_is_config_missing() {
        let error = Config::load(Path::new("/nonexistent/lovense-mcp/none.toml")).unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigMissing);
    }

    // --- adversarial ---

    #[test]
    fn unknown_key_is_refused() {
        let error = refused("transport = \"mock\"\ntoken = \"x\"\n");
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(error.message.contains("token"), "{}", error.message);
    }

    #[test]
    fn ceiling_21_is_refused() {
        let error = refused("intensity_ceiling = 21");
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(error.message.contains("intensity_ceiling"));
        refused("intensity_ceiling = -1");
    }

    #[test]
    fn port_0_and_overflow_are_refused() {
        assert!(refused("port = 0").message.contains("port"));
        assert!(refused("port = 65536").message.contains("port"));
        assert!(refused("port = -1").message.contains("port"));
    }

    #[test]
    fn watchdog_0_and_61_are_refused() {
        assert!(
            refused("watchdog_secs = 0")
                .message
                .contains("watchdog_secs")
        );
        assert!(
            refused("watchdog_secs = 61")
                .message
                .contains("watchdog_secs")
        );
    }

    #[test]
    fn bad_transport_value_is_refused() {
        assert!(
            refused("transport = \"cloud\"")
                .message
                .contains("transport")
        );
        assert!(
            refused("transport = \"MOCK\"")
                .message
                .contains("transport")
        );
        refused("transport = 1");
    }

    #[test]
    fn hosts_that_could_rewrite_the_url_are_refused() {
        for host in [
            "http://evil",
            "evil/path",
            "user@host",
            "host:1234",
            "host name",
            "host?x=1",
            "host#frag",
            "-leading",
            "[::1]",
            "host\n",
        ] {
            let text = format!("host = {}", toml_string(host));
            assert!(refused(&text).message.contains("host"), "host {host:?}");
        }
        refused(&format!("host = \"{}\"", "a".repeat(254)));
    }

    #[test]
    fn buttplug_section_is_fail_closed_too() {
        // Unknown keys inside the section refuse, exactly like the top
        // level — a stray credential-shaped key has nowhere to hide.
        assert!(
            refused("[buttplug]\ntoken = \"x\"")
                .message
                .contains("token")
        );
        assert!(
            refused("[buttplug]\nscan_ms = 99")
                .message
                .contains("buttplug.scan_ms")
        );
        assert!(
            refused("[buttplug]\nscan_ms = 10001")
                .message
                .contains("buttplug.scan_ms")
        );
        refused("[buttplug]\nenabled = \"yes\"");
    }

    #[test]
    fn server_api_section_is_fail_closed_too() {
        // A credential value has nowhere to land: `token` is an
        // unknown key here and refuses the config outright (D-1451:
        // the token lives in the password store, never in config).
        assert!(
            refused("[server_api]\ntoken = \"x\"")
                .message
                .contains("token")
        );
        assert!(refused("[server_api]\nuid = \"x\"").message.contains("uid"));
        refused("[server_api]\nenabled = \"yes\"");
        refused("[server_api]\nenabled = 1");
    }

    #[test]
    fn server_api_pass_entries_cannot_escape_the_store() {
        for entry in [
            "/etc/passwd",
            "../secret",
            "lovense/../secret",
            "lovense//token",
            "lovense/token ",
            "lovense\\token",
            "lovense/token\n",
            "",
        ] {
            let text = format!("[server_api]\ntoken_pass_entry = {}", toml_string(entry));
            assert!(
                refused(&text)
                    .message
                    .contains("server_api.token_pass_entry"),
                "entry {entry:?}"
            );
        }
        refused(&format!(
            "[server_api]\nuid_pass_entry = \"{}\"",
            "a/".repeat(70)
        ));
    }

    #[test]
    fn wrong_types_and_malformed_toml_are_refused() {
        refused("use_https = \"yes\"");
        refused("port = \"20010\"");
        refused("transport = ");
        refused("[section]\nhost = \"x\"");
    }

    fn toml_string(value: &str) -> String {
        format!(
            "\"{}\"",
            value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        )
    }
}

#[cfg(test)]
mod rumble_config_tests {
    use super::*;

    const GOOD: &str = r#"[rumble]
enabled = true
tick_ms = 200
socket = "/run/user/1000/ache-rumble.sock"
[[rumble.source]]
id = "torogao"
gain_percent = 150
ceiling_percent = 80
[[rumble.map]]
source = "torogao"
motor = "heavy"
device = "buttplug:1"
function = "Vibrate"
max_strength = 12
[[rumble.map]]
source = "torogao"
motor = "light"
device = "buttplug:0"
function = "Suction"
"#;

    fn refused_field(text: &str) -> String {
        match Config::from_toml_str(text) {
            Ok(config) => panic!("config was accepted: {:?}", config.rumble),
            Err(error) => {
                assert_eq!(error.code, ErrorCode::ConfigInvalid);
                error.message
            }
        }
    }

    // --- validation ---

    #[test]
    fn rumble_is_off_by_default() {
        let config = Config::from_toml_str("").unwrap();
        assert!(!config.rumble.enabled);
        assert!(config.rumble.sources.is_empty() && config.rumble.mappings.is_empty());
        assert_eq!(config.rumble.socket, None);
        let config = Config::from_toml_str("[rumble]\n").unwrap();
        assert!(!config.rumble.enabled, "an empty section stays master-off");
    }

    #[test]
    fn a_full_rumble_section_parses_with_defaults_filled() {
        let rumble = Config::from_toml_str(GOOD).unwrap().rumble;
        assert!(rumble.enabled);
        assert_eq!(rumble.tick_ms, 200);
        assert_eq!(
            rumble.socket.as_deref(),
            Some("/run/user/1000/ache-rumble.sock")
        );
        assert_eq!(
            rumble.sources,
            vec![SourceRegistration {
                id: "torogao".into(),
                gain_percent: 150,
                ceiling_percent: 80
            }]
        );
        assert_eq!(rumble.mappings[0].motor, Motor::Heavy);
        assert_eq!(rumble.mappings[0].max_strength, 12);
        assert_eq!(rumble.mappings[1].function, "Suction");
        assert_eq!(rumble.mappings[1].max_strength, 20, "default is full scale");
    }

    // --- adversarial ---

    #[test]
    fn rumble_out_of_range_knobs_are_refused_by_field() {
        let socket = r#"socket = "/run/user/1000/ache-rumble.sock""#;
        for (from, to, field) in [
            ("tick_ms = 200", "tick_ms = 99", "rumble.tick_ms"),
            ("tick_ms = 200", "tick_ms = 1001", "rumble.tick_ms"),
            (
                "gain_percent = 150",
                "gain_percent = 0",
                "rumble.source.gain_percent",
            ),
            (
                "gain_percent = 150",
                "gain_percent = 501",
                "rumble.source.gain_percent",
            ),
            (
                "ceiling_percent = 80",
                "ceiling_percent = 101",
                "rumble.source.ceiling_percent",
            ),
            (
                "max_strength = 12",
                "max_strength = 21",
                "rumble.map.max_strength",
            ),
            (
                r#"motor = "heavy""#,
                r#"motor = "left""#,
                "rumble.map.motor",
            ),
            (
                r#"function = "Vibrate""#,
                r#"function = "Stroke""#,
                "rumble.map.function",
            ),
            (
                r#"function = "Vibrate""#,
                r#"function = "Thrusting""#,
                "rumble.map.function",
            ),
            (socket, r#"socket = "relative.sock""#, "rumble.socket"),
            (socket, r#"socket = "/run/../etc/x""#, "rumble.socket"),
        ] {
            let text = GOOD.replacen(from, to, 1);
            assert_ne!(text, GOOD, "{from} must occur in the fixture");
            let message = refused_field(&text);
            assert!(message.contains(field), "{to}: {message}");
        }
    }

    #[test]
    fn rumble_mapping_must_name_a_registered_source_and_be_unique() {
        let text = GOOD.replacen(
            "source = \"torogao\"\nmotor = \"heavy\"",
            "source = \"stranger\"\nmotor = \"heavy\"",
            1,
        );
        assert!(refused_field(&text).contains("rumble.map.source"));
        let text = format!("{GOOD}[[rumble.source]]\nid = \"torogao\"\n");
        assert!(refused_field(&text).contains("registered twice"));
        let text = format!(
            "{GOOD}[[rumble.map]]\nsource = \"torogao\"\nmotor = \"heavy\"\n\
             device = \"buttplug:1\"\nfunction = \"Vibrate\"\n"
        );
        assert!(refused_field(&text).contains("declared twice"));
    }

    #[test]
    fn rumble_enabled_without_mappings_and_unknown_keys_refuse() {
        assert!(refused_field("[rumble]\nenabled = true\n").contains("rumble.enabled"));
        let error = Config::from_toml_str("[rumble]\nstrength = 20\n").unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        let error =
            Config::from_toml_str("[rumble]\n[[rumble.source]]\nid = \"a\"\ntoken = \"x\"\n")
                .unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
    }
}
