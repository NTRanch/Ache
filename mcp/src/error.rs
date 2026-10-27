//! Structured errors. Every expected failure carries a stable snake_case
//! code, a class, and a bounded message; the wire form mirrors the estate
//! servers: `{"error": {"code", "class", "message"}}`.

use std::fmt;

/// Longest message carried in an error, in bytes. Longer messages are
/// truncated on a char boundary so a hostile peer cannot inflate output.
pub const ERROR_MESSAGE_BYTES_MAX: usize = 2048;

/// Stable error codes. The string form is part of the tool contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    ConfigMissing,
    ConfigUnreadable,
    ConfigInvalid,
    TargetUnconfigured,
    TargetUnreachable,
    ResponseInvalid,
    AppError,
    NotArmed,
    RequestInvalid,
    /// A named provider (Lovense or Buttplug) cannot serve at all in
    /// this process: disabled, not wired, or its embedded stack failed
    /// to start. Distinct from a configured target that merely has no
    /// device attached right now.
    ProviderUnavailable,
    /// The target device exists but does not offer the requested
    /// capability (a feature it lacks, or a Lovense-only preset aimed
    /// at a Buttplug device). The gate refuses; nothing is sent.
    CapabilityUnsupported,
    /// A unified device id names no device any provider can see.
    DeviceNotFound,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::ConfigMissing => "config_missing",
            ErrorCode::ConfigUnreadable => "config_unreadable",
            ErrorCode::ConfigInvalid => "config_invalid",
            ErrorCode::TargetUnconfigured => "target_unconfigured",
            ErrorCode::TargetUnreachable => "target_unreachable",
            ErrorCode::ResponseInvalid => "response_invalid",
            ErrorCode::AppError => "app_error",
            ErrorCode::NotArmed => "not_armed",
            ErrorCode::RequestInvalid => "request_invalid",
            ErrorCode::ProviderUnavailable => "provider_unavailable",
            ErrorCode::CapabilityUnsupported => "capability_unsupported",
            ErrorCode::DeviceNotFound => "device_not_found",
        }
    }

    pub fn class(self) -> &'static str {
        match self {
            ErrorCode::ConfigMissing | ErrorCode::ConfigUnreadable | ErrorCode::ConfigInvalid => {
                "configuration"
            }
            ErrorCode::TargetUnconfigured
            | ErrorCode::CapabilityUnsupported
            | ErrorCode::DeviceNotFound => "target",
            ErrorCode::TargetUnreachable
            | ErrorCode::ResponseInvalid
            | ErrorCode::AppError
            | ErrorCode::ProviderUnavailable => "transport",
            ErrorCode::NotArmed => "safety",
            ErrorCode::RequestInvalid => "request",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcheError {
    pub code: ErrorCode,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, AcheError>;

impl AcheError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> AcheError {
        AcheError {
            code,
            message: truncate(message.into()),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "error": {
                "code": self.code.as_str(),
                "class": self.code.class(),
                "message": self.message,
            }
        })
    }
}

impl fmt::Display for AcheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for AcheError {}

pub fn err<T>(code: ErrorCode, message: impl Into<String>) -> Result<T> {
    Err(AcheError::new(code, message))
}

/// Bound a message to `ERROR_MESSAGE_BYTES_MAX` bytes on a char boundary.
pub fn truncate(mut message: String) -> String {
    if message.len() <= ERROR_MESSAGE_BYTES_MAX {
        return message;
    }
    let mut cut = ERROR_MESSAGE_BYTES_MAX;
    while !message.is_char_boundary(cut) {
        cut -= 1;
    }
    message.truncate(cut);
    message.push('…');
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_messages_are_truncated_on_a_char_boundary() {
        let error = AcheError::new(ErrorCode::ResponseInvalid, "é".repeat(4096));
        assert!(error.message.len() <= ERROR_MESSAGE_BYTES_MAX + '…'.len_utf8());
        assert!(error.message.ends_with('…'));
    }
}
