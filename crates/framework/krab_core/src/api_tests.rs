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
        let error = ApiError::new(ErrorCategory::Authz, "UNAUTHORIZED", "Access denied");
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let error = ApiError::new(ErrorCategory::NotFound, "NOT_FOUND", "Resource missing");
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let error = ApiError::new(ErrorCategory::Internal, "UNKNOWN_CODE", "System error");
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
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
