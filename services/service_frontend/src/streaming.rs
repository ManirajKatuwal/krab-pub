//! `/streaming`: the progressive streaming SSR demo (ADR 0017), and the swap
//! runtime it loads from `/_krab/stream.js`.
//!
//! The page has one `<Suspense>` boundary around a resource with a slow server
//! loader. The shell — heading, intro, and the boundary's fallback — is
//! flushed immediately; the report streams in as a `<template>` once the
//! loader finishes, and the swap runtime moves it into place.

use axum::body::Body;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};
use krab_core::render_stream::{render_to_stream, STREAM_SWAP_SCRIPT};
use krab_core::resource::create_resource;
use krab_core::Node;
use krab_macros::view;
use std::time::Duration;

/// How long the demo's "slow" loader takes. Long enough to see the fallback,
/// short enough not to annoy.
const DEMO_LOADER_DELAY: Duration = Duration::from_millis(400);

pub(crate) async fn streaming_handler() -> Response {
    streaming_response(DEMO_LOADER_DELAY)
}

/// The streamed response for a loader taking `delay`.
pub(crate) fn streaming_response(delay: Duration) -> Response {
    let body = Body::from_stream(render_to_stream(move || streaming_page(delay)));
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    // The route has no render policy, so the cache middleware passes it
    // straight through; this also keeps intermediaries from buffering a
    // response whose point is arriving in pieces.
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Serve the swap runtime a streamed page loads. An external script because
/// the security headers send `script-src 'self'`, which blocks inline ones.
pub(crate) async fn stream_script_handler() -> impl IntoResponse {
    (
        [
            (
                CONTENT_TYPE,
                HeaderValue::from_static("application/javascript; charset=utf-8"),
            ),
            (
                CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=3600"),
            ),
        ],
        STREAM_SWAP_SCRIPT,
    )
}

/// The demo page. Runs on the render thread `render_to_stream` owns.
fn streaming_page(delay: Duration) -> Node {
    view! {
        <html lang="en">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <title>"Streaming SSR · Krab"</title>
            </head>
            <body>
                <main>
                    <h1>"Progressive streaming"</h1>
                    <p>"This paragraph arrived in the first chunk. The report below is loaded on the server and streamed in when it is ready."</p>
                    <Suspense fallback={|| view! { <p class="loading">"Loading the slow report…"</p> }}>
                        {{
                            // The browser fetcher is never used — the page has
                            // no island — but a resource needs one.
                            let report = create_resource(
                                || (),
                                |_| async move { Ok::<String, String>(String::new()) },
                            )
                            .with_server_loader(move || async move {
                                tokio::time::sleep(delay).await;
                                Ok::<_, String>(format!(
                                    "Report ready: loaded on the server in {} ms.",
                                    delay.as_millis()
                                ))
                            });
                            move || view! {
                                <p class="report">{report.value().get().unwrap_or_default()}</p>
                            }
                        }}
                    </Suspense>
                </main>
            </body>
        </html>
    }
}
