//! gRPC **semantics** for a gateway — not a gRPC transport.
//!
//! This module provides the vocabulary needed to map between HTTP and gRPC at a
//! boundary: the canonical status codes, and `grpc-timeout` header parsing so a
//! deadline propagated by a gRPC client can be honoured.
//!
//! It does **not** provide a transport. There is no codegen, no `.proto`
//! handling, no service trait, no channel, and no client — `tonic` and `prost`
//! appear nowhere in the workspace. Nothing here can speak gRPC to anything.
//!
//! The module and its feature were called `grpc`, which implied otherwise at
//! the point of `cargo add` — before anyone reads a caveat. See
//! [ADR 0007](https://github.com/ManirajKatuwal/krab/blob/main/docs/adr/0007-grpc-feature-disposition.md).
//!
//! A real transport is not foreclosed; it is simply a separate piece of work
//! with its own ADR.

use std::time::Duration;

/// gRPC status codes as defined by the wire protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum GrpcStatusCode {
    Ok = 0,
    Cancelled = 1,
    Unknown = 2,
    InvalidArgument = 3,
    DeadlineExceeded = 4,
    NotFound = 5,
    AlreadyExists = 6,
    PermissionDenied = 7,
    ResourceExhausted = 8,
    FailedPrecondition = 9,
    Aborted = 10,
    OutOfRange = 11,
    Unimplemented = 12,
    Internal = 13,
    Unavailable = 14,
    DataLoss = 15,
    Unauthenticated = 16,
}

impl GrpcStatusCode {
    pub fn from_u16(value: u16) -> Self {
        match value {
            0 => Self::Ok,
            1 => Self::Cancelled,
            2 => Self::Unknown,
            3 => Self::InvalidArgument,
            4 => Self::DeadlineExceeded,
            5 => Self::NotFound,
            6 => Self::AlreadyExists,
            7 => Self::PermissionDenied,
            8 => Self::ResourceExhausted,
            9 => Self::FailedPrecondition,
            10 => Self::Aborted,
            11 => Self::OutOfRange,
            12 => Self::Unimplemented,
            13 => Self::Internal,
            14 => Self::Unavailable,
            15 => Self::DataLoss,
            16 => Self::Unauthenticated,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcStatus {
    pub code: GrpcStatusCode,
    pub message: String,
}

impl GrpcStatus {
    pub fn ok() -> Self {
        Self {
            code: GrpcStatusCode::Ok,
            message: String::new(),
        }
    }

    pub fn error(code: GrpcStatusCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Validate an HTTP content type for gRPC requests.
pub fn is_grpc_content_type(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value == "application/grpc" || value.starts_with("application/grpc+")
}

/// Parse the `grpc-timeout` header value.
///
/// Supported units:
/// - `H` hours
/// - `M` minutes
/// - `S` seconds
/// - `m` milliseconds
/// - `u` microseconds
/// - `n` nanoseconds
pub fn parse_grpc_timeout(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.len() < 2 {
        return None;
    }

    let (num_part, unit_part) = value.split_at(value.len() - 1);
    let amount = num_part.parse::<u64>().ok()?;
    let unit = unit_part.chars().next()?;

    match unit {
        'H' => Some(Duration::from_secs(amount.saturating_mul(60 * 60))),
        'M' => Some(Duration::from_secs(amount.saturating_mul(60))),
        'S' => Some(Duration::from_secs(amount)),
        'm' => Some(Duration::from_millis(amount)),
        'u' => Some(Duration::from_micros(amount)),
        'n' => Some(Duration::from_nanos(amount)),
        _ => None,
    }
}

/// Format a `grpc-timeout` header value from a duration.
///
/// Uses milliseconds by default to keep precision while remaining readable.
pub fn format_grpc_timeout(duration: Duration) -> String {
    format!("{}m", duration.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_content_type_validation_accepts_expected_values() {
        assert!(is_grpc_content_type("application/grpc"));
        assert!(is_grpc_content_type("application/grpc+proto"));
        assert!(is_grpc_content_type("application/grpc+json"));
        assert!(!is_grpc_content_type("application/json"));
    }

    #[test]
    fn grpc_timeout_parsing_supports_all_units() {
        assert_eq!(parse_grpc_timeout("1H"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_grpc_timeout("2M"), Some(Duration::from_secs(120)));
        assert_eq!(parse_grpc_timeout("3S"), Some(Duration::from_secs(3)));
        assert_eq!(parse_grpc_timeout("4m"), Some(Duration::from_millis(4)));
        assert_eq!(parse_grpc_timeout("5u"), Some(Duration::from_micros(5)));
        assert_eq!(parse_grpc_timeout("6n"), Some(Duration::from_nanos(6)));
    }

    #[test]
    fn grpc_timeout_parsing_rejects_invalid_values() {
        assert_eq!(parse_grpc_timeout(""), None);
        assert_eq!(parse_grpc_timeout("x"), None);
        assert_eq!(parse_grpc_timeout("10Q"), None);
    }

    #[test]
    fn grpc_timeout_formatter_uses_milliseconds() {
        assert_eq!(format_grpc_timeout(Duration::from_millis(250)), "250m");
    }

    #[test]
    fn status_code_round_trip_handles_unknown_values() {
        assert_eq!(GrpcStatusCode::from_u16(0), GrpcStatusCode::Ok);
        assert_eq!(
            GrpcStatusCode::from_u16(16),
            GrpcStatusCode::Unauthenticated
        );
        assert_eq!(GrpcStatusCode::from_u16(999), GrpcStatusCode::Unknown);
    }
}
