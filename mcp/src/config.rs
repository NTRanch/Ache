//! Fail-closed configuration. Unknown keys, out-of-range values, and
//! malformed hosts are refused with a structured error; nothing is
//! silently defaulted past validation.
//!
//! S1 note: `intensity_ceiling` and `watchdog_secs` are parsed, validated,
//! and reported only. They bind output from S2, when an output path exists.

use std::path::Path;

use serde::Deserialize;

use crate::error::{ErrorCode, Result, err};

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
}

/// The `[buttplug]` section: the embedded Buttplug provider's knobs.
/// Same fail-closed discipline as the top level — unknown keys refuse.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawButtplug {
    enabled: Option<bool>,
    scan_ms: Option<i64>,
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
        Ok(Config {
            transport,
            host,
            port,
            use_https: raw.use_https.unwrap_or(defaults.use_https),
            intensity_ceiling,
            watchdog_secs,
            buttplug_enabled,
            buttplug_scan_ms,
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
