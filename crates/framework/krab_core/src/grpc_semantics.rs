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
//! [ADR 0007](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/adr/0007-grpc-feature-disposition.md).
//!
//! A real transport is not foreclosed; it is simply a separate piece of work
//! with its own ADR.

use std::time::Duration;

/// gRPC status codes as defined by the wire protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum GrpcStatusCode {
    /// Success.
    Ok = 0,
    /// The operation was cancelled, typically by the caller.
    Cancelled = 1,
    /// An unknown error; also what [`GrpcStatusCode::from_u16`] returns for an
    /// out-of-range value.
    Unknown = 2,
    /// The client supplied an invalid argument, whatever the system state.
    InvalidArgument = 3,
    /// The deadline expired before the operation completed.
    DeadlineExceeded = 4,
    /// A requested entity was not found.
    NotFound = 5,
    /// The entity the client tried to create already exists.
    AlreadyExists = 6,
    /// The caller is identified but not permitted to do this.
    PermissionDenied = 7,
    /// A resource, such as a quota or rate limit, is exhausted.
    ResourceExhausted = 8,
    /// The system is not in the state the operation requires.
    FailedPrecondition = 9,
    /// The operation was aborted, typically by a concurrency conflict.
    Aborted = 10,
    /// The operation was attempted past a valid range.
    OutOfRange = 11,
    /// The operation is not implemented or not supported.
    Unimplemented = 12,
    /// An internal invariant was broken.
    Internal = 13,
    /// The service is currently unavailable; usually transient and retryable.
    Unavailable = 14,
    /// Unrecoverable data loss or corruption.
    DataLoss = 15,
    /// The request lacks valid authentication credentials.
    Unauthenticated = 16,
}

impl GrpcStatusCode {
    /// The code with numeric value `value`; any value above 16 maps to
    /// [`GrpcStatusCode::Unknown`].
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

/// A gRPC call outcome: the status code and its message (the `grpc-status`
/// and `grpc-message` trailers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcStatus {
    /// The status code.
    pub code: GrpcStatusCode,
    /// Developer-facing description; empty on success.
    pub message: String,
}

impl GrpcStatus {
    /// [`GrpcStatusCode::Ok`] with an empty message.
    pub fn ok() -> Self {
        Self {
            code: GrpcStatusCode::Ok,
            message: String::new(),
        }
    }

    /// A status with `code` and `message`. Does not check that `code` is an
    /// error code.
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
