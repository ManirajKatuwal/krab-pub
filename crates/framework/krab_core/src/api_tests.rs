#[cfg(test)]
mod tests {
    use crate::http::{ApiError, ErrorCategory};
    use crate::protocol::RpcEnvelope;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use serde_json::json;

    #[test]
    fn test_error_envelope_structure() {
        let error = ApiError::new(
            ErrorCategory::Internal,
            "TEST_ERROR",
            "Something went wrong",
        )
        .with_details(json!({ "field": "value" }));

        assert_eq!(error.code, "TEST_ERROR");
        assert_eq!(error.message, "Something went wrong");
        assert!(error.details.is_some());
        assert_eq!(error.category, ErrorCategory::Internal);
    }

    #[test]
    fn test_error_status_mapping() {
        // Status now derives from the category alone: the old magic-string
        // overrides (`code == "UNAUTHORIZED"` => 401,
        // `code == "TOO_MANY_REQUESTS"` => 429) were replaced by the
        // dedicated `Unauthenticated` and `RateLimited` categories.
        let error = ApiError::new(
            ErrorCategory::Unauthenticated,
            "UNAUTHORIZED",
            "Access denied",
        );
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let error = ApiError::new(ErrorCategory::Authz, "FORBIDDEN", "Access denied");
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let error = ApiError::new(
            ErrorCategory::RateLimited,
            "TOO_MANY_REQUESTS",
            "Rate limit exceeded",
        );
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        let error = ApiError::new(ErrorCategory::NotFound, "NOT_FOUND", "Resource missing");
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let error = ApiError::new(ErrorCategory::Internal, "UNKNOWN_CODE", "System error");
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// Magic-string status overrides are gone: an `Authz` error whose code
    /// happens to be "UNAUTHORIZED" is still a 403 — only the category picks
    /// the status.
    #[test]
    fn test_authz_category_is_forbidden_regardless_of_code() {
        let error = ApiError::new(ErrorCategory::Authz, "UNAUTHORIZED", "Access denied");
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Wire-contract snapshot for the new categories: `rate_limited` and
    /// `unauthenticated` are the serde snake_case strings clients see.
    #[test]
    fn test_new_category_wire_snapshots() {
        let error = ApiError::new(
            ErrorCategory::RateLimited,
            "TOO_MANY_REQUESTS",
            "global per-ip rate limit exceeded",
        );
        let json_str = serde_json::to_string(&error).unwrap();
        assert_eq!(
            json_str,
            r#"{"category":"rate_limited","code":"TOO_MANY_REQUESTS","message":"global per-ip rate limit exceeded"}"#
        );

        let error = ApiError::new(
            ErrorCategory::Unauthenticated,
            "UNAUTHORIZED",
            "credentials required",
        );
        let json_str = serde_json::to_string(&error).unwrap();
        assert_eq!(
            json_str,
            r#"{"category":"unauthenticated","code":"UNAUTHORIZED","message":"credentials required"}"#
        );
    }

    #[test]
    fn test_rpc_envelope() {
        let data = json!({ "user": "test" });
        let envelope = RpcEnvelope::new(data, 1)
            .with_request_id("req-123".to_string())
            .with_compatibility_mode();

        assert_eq!(envelope.schema_version, 1);
        assert_eq!(envelope.request_id, Some("req-123".to_string()));
        assert!(envelope.compatibility_mode);
    }

    #[test]
    fn test_rpc_envelope_wire_snapshot() {
        let envelope =
            RpcEnvelope::new(json!({"id": "usr_123"}), 2).with_request_id("req-abc".to_string());

        let json_str = serde_json::to_string(&envelope).unwrap();

        // Exact wire payload snapshot test to catch breaking changes
        assert_eq!(
            json_str,
            r#"{"data":{"id":"usr_123"},"request_id":"req-abc","schema_version":2,"compatibility_mode":false}"#
        );
    }

    #[test]
    fn test_api_error_wire_snapshot() {
        let error = ApiError::new(
            ErrorCategory::Validation,
            "VALIDATION_FAILED",
            "Invalid payload",
        )
        .with_details(json!({"field": "email", "reason": "missing"}));

        let json_str = serde_json::to_string(&error).unwrap();

        assert_eq!(
            json_str,
            r#"{"category":"validation","code":"VALIDATION_FAILED","message":"Invalid payload","details":{"field":"email","reason":"missing"}}"#
        );
    }
}
