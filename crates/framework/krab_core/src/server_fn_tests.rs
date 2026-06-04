#[cfg(test)]
mod tests {
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::server_fn::{
        server_fn_dispatch_router, server_fn_router, BoxFuture, ServerFnError, ServerFnRegistration,
    };

    fn echo_handler(args: Value) -> BoxFuture<axum::response::Response> {
        Box::pin(async move { (StatusCode::OK, axum::Json(args)).into_response() })
    }

    fn fail_handler(_args: Value) -> BoxFuture<axum::response::Response> {
        Box::pin(async move { ServerFnError::bad_request("invalid payload").into_response() })
    }

    static DISPATCH_REGISTRATIONS: &[ServerFnRegistration] = &[
        ServerFnRegistration {
            name: "echo",
            url: "/api/rpc/echo",
            handler: echo_handler,
        },
        ServerFnRegistration {
            name: "fail",
            url: "/api/rpc/fail",
            handler: fail_handler,
        },
    ];

    static DIRECT_REGISTRATIONS: &[ServerFnRegistration] = &[ServerFnRegistration {
        name: "echo",
        url: "/api/rpc/echo",
        handler: echo_handler,
    }];

    #[tokio::test]
    async fn dispatch_router_happy_path_returns_json_payload() {
        let app = server_fn_dispatch_router(DISPATCH_REGISTRATIONS);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/rpc/echo")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "name": "krab", "n": 3 }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["name"], "krab");
        assert_eq!(parsed["n"], 3);
    }

    #[tokio::test]
    async fn dispatch_router_failure_path_returns_error_envelope() {
        let app = server_fn_dispatch_router(DISPATCH_REGISTRATIONS);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/rpc/fail")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "x": 1 }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["error"], "invalid payload");
        assert_eq!(parsed["message"], "invalid payload");
        assert_eq!(parsed["code"], "bad_request");
        assert_eq!(parsed["status_code"], 400);
    }

    #[tokio::test]
    async fn dispatch_router_unknown_function_returns_not_found() {
        let app = server_fn_dispatch_router(DISPATCH_REGISTRATIONS);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/rpc/missing_fn")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn direct_router_mounts_registered_url() {
        let app = server_fn_router(DIRECT_REGISTRATIONS);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/rpc/echo")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "ok": true }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
