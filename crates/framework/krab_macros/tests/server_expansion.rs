//! Positive coverage for `#[server]`.
//!
//! The macro previously had only compile-fail tests plus a doctest, so nothing
//! exercised what it actually generates: the args struct, the Axum handler, the
//! dispatch shim, or the `ServerFn` marker. The handler and the shim used to be
//! two independent copies of the same logic; the shim now delegates, and these
//! tests pin the behaviour that delegation has to preserve.

use axum::body::to_bytes;
use axum::response::Response;
use krab_core::server_fn::{ServerFn, ServerFnError};
use krab_macros::server;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Greeting {
    pub message: String,
}

#[server]
pub async fn greet(name: String, excited: bool) -> Result<Greeting, ServerFnError> {
    let suffix = if excited { "!" } else { "." };
    Ok(Greeting {
        message: format!("Hello, {name}{suffix}"),
    })
}

#[server]
pub async fn heartbeat() -> Result<String, ServerFnError> {
    Ok("alive".to_string())
}

#[server]
pub async fn always_fails() -> Result<Greeting, ServerFnError> {
    Err(ServerFnError::new("deliberate failure"))
}

async fn body_of(response: Response) -> (u16, String) {
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should be readable");
    (
        status,
        String::from_utf8(bytes.to_vec()).expect("utf-8 body"),
    )
}

#[test]
fn registration_metadata_is_derived_from_the_function_name() {
    assert_eq!(<greet as ServerFn>::NAME, "greet");
    assert_eq!(<greet as ServerFn>::URL, "/api/rpc/greet");
    assert_eq!(<heartbeat as ServerFn>::NAME, "heartbeat");
    assert_eq!(<heartbeat as ServerFn>::URL, "/api/rpc/heartbeat");
}

#[tokio::test]
async fn the_function_itself_is_still_directly_callable() {
    let greeting = greet("Ada".to_string(), true)
        .await
        .expect("call should succeed");

    assert_eq!(greeting.message, "Hello, Ada!");
}

#[tokio::test]
async fn the_handler_decodes_named_arguments_and_serialises_the_result() {
    let response = greet_handler(axum::Json(serde_json::json!({
        "name": "Grace",
        "excited": false,
    })))
    .await;

    let (status, body) = body_of(response).await;
    assert_eq!(status, 200);
    assert_eq!(body, r#"{"message":"Hello, Grace."}"#);
}

#[tokio::test]
async fn the_dispatch_shim_matches_the_handler() {
    // Same input through both entry points. These were separate copies of the
    // same body until the shim was made to delegate; this is what keeps them
    // from drifting apart again.
    let args = serde_json::json!({ "name": "Grace", "excited": false });

    let via_handler = body_of(greet_handler(axum::Json(args.clone())).await).await;
    let via_dispatch = body_of(<greet as ServerFn>::dispatch(args).await).await;

    assert_eq!(via_handler, via_dispatch);
}

#[tokio::test]
async fn a_zero_argument_function_dispatches_on_an_empty_object() {
    let response = <heartbeat as ServerFn>::dispatch(serde_json::json!({})).await;

    let (status, body) = body_of(response).await;
    assert_eq!(status, 200);
    assert_eq!(body, r#""alive""#);
}

#[tokio::test]
async fn a_missing_argument_is_reported_as_a_validation_failure() {
    let response = <greet as ServerFn>::dispatch(serde_json::json!({ "name": "Ada" })).await;

    let (status, body) = body_of(response).await;
    assert_ne!(status, 200);
    assert!(
        body.contains("greet") && body.contains("excited"),
        "validation error should name the function and the missing field, got: {body}"
    );
}

#[tokio::test]
async fn an_error_returned_by_the_function_becomes_an_error_response() {
    let response = <always_fails as ServerFn>::dispatch(serde_json::json!({})).await;

    let (status, body) = body_of(response).await;
    assert_ne!(status, 200);
    assert!(
        body.contains("deliberate failure"),
        "error response should carry the message, got: {body}"
    );
}
