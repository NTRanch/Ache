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
        }
    }

    pub fn class(self) -> &'static str {
        match self {
            ErrorCode::ConfigMissing | ErrorCode::ConfigUnreadable | ErrorCode::ConfigInvalid => {
                "configuration"
            }
            ErrorCode::TargetUnconfigured => "target",
            ErrorCode::TargetUnreachable | ErrorCode::ResponseInvalid | ErrorCode::AppError => {
                "transport"
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LovenseError {
    pub code: ErrorCode,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, LovenseError>;

impl LovenseError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> LovenseError {
        LovenseError {
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

impl fmt::Display for LovenseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for LovenseError {}

pub fn err<T>(code: ErrorCode, message: impl Into<String>) -> Result<T> {
    Err(LovenseError::new(code, message))
}

fn truncate(mut message: String) -> String {
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
        let error = LovenseError::new(ErrorCode::ResponseInvalid, "é".repeat(4096));
        assert!(error.message.len() <= ERROR_MESSAGE_BYTES_MAX + '…'.len_utf8());
        assert!(error.message.ends_with('…'));
    }
}
