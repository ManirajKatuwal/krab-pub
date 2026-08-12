//! Browser tests for the client router.
//!
//! Run with:
//!
//! ```sh
//! CHROMEDRIVER=/path/to/chromedriver \
//!   cargo test -p krab_client --target wasm32-unknown-unknown --features web
//! ```
//!
//! # Why a browser is the only place these can run
//!
//! The router's pure decisions — [`should_intercept`], `extract_outlet_html`,
//! title extraction — are unit-tested in `router.rs` and run on the host. What is
//! *not* reachable there is everything the router exists for: a capturing click
//! listener beating the browser to a navigation, an outlet swap, history state,
//! scroll restoration, and the ordering between a slow response and a newer one.
//! Every defect this suite pins produced correct-looking code and a wrong page.
//!
//! # How the network is faked
//!
//! `window.fetch` is replaced with a JS shim consulting `window.__krabRoutes`,
//! keyed by `location.search`, with a per-route delay. Two properties matter:
//!
//! - It **never rejects**. A failed fetch makes the router fall back to
//!   `location.assign`, which reloads the page and takes the `wasm-bindgen-test`
//!   harness with it — the run then reports "Failed to detect test as having
//!   been run" with no hint of the cause. Unknown routes therefore resolve to a
//!   default document that has an outlet.
//! - It can stamp `response.url`, so a server redirect can be simulated on a
//!   synthetically constructed `Response` (whose `url` is otherwise empty).
//!
//! # Why clicks are dispatched with a guard listener
//!
//! A synthetic `click` on an `<a href>` performs the anchor's real activation
//! behaviour in Chrome, so a click the router correctly declines would navigate
//! the harness away. A bubble-ordered capture listener registered *after* the
//! router records `event.defaultPrevented` — the exact observable under test —
//! and then prevents the default itself, so the page stays put either way.

#![cfg(target_arch = "wasm32")]

use std::cell::Cell;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::Element;

#[path = "support/mod.rs"]
mod support;
use support::{document, settle};

wasm_bindgen_test_configure!(run_in_browser);

/// Container every test mounts into. Distinct from the other suites' ids.
const TEST_ROOT_ID: &str = "krab-router-test-root";
const OUTLET_ID: &str = "krab-router-test-outlet";

/// Tall enough that the test page always scrolls, whatever the headless window
/// size is. It lives *outside* the outlet so an outlet swap cannot change the
/// document height and invalidate a saved scroll offset.
const SPACER_HEIGHT_PX: u32 = 5000;

// ── Harness ────────────────────────────────────────────────────────────────

/// A full destination document with an outlet, as the server would send it.
fn page_document(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title></head><body>\
         <nav>chrome that is never swapped</nav>\
         <main data-krab-router-outlet tabindex=\"-1\">{body}</main>\
         </body></html>"
    )
}

fn eval(args: &str, source: &str, values: &[JsValue]) {
    let function = js_sys::Function::new_with_args(args, source);
    let array = js_sys::Array::new();
    for value in values {
        array.push(value);
    }
    function
        .apply(&JsValue::NULL, &array)
        .expect("test JS helper threw");
}

/// Install the `fetch` shim once per page.
fn install_fetch_mock() {
    eval(
        "",
        r#"
        if (window.__krabRoutesInstalled) { return; }
        window.__krabRoutesInstalled = true;
        window.__krabRoutes = {};
        window.__krabFetches = 0;
        window.fetch = function (input) {
            window.__krabFetches += 1;
            var raw = (typeof input === 'string') ? input : input.url;
            var parsed = new URL(raw, location.href);
            var entry = window.__krabRoutes[parsed.search] || window.__krabRoutes['__default'];
            if (!entry) {
                // Never reject and never throw: either would fall through to
                // location.assign and reload the harness out from under us.
                entry = { body: '<main data-krab-router-outlet></main>', delay: 0 };
            }
            return new Promise(function (resolve) {
                setTimeout(function () {
                    var response = new Response(entry.body, {
                        status: 200,
                        headers: { 'content-type': 'text/html' }
                    });
                    if (entry.url) {
                        Object.defineProperty(response, 'url', { value: entry.url });
                    }
                    resolve(response);
                }, entry.delay || 0);
            });
        };
        "#,
        &[],
    );
}

/// Register a mock response. `search` is the query string including `?`, or
/// `__default` for everything unregistered.
fn route(search: &str, body: &str, delay_ms: u32) {
    eval(
        "search, body, delay",
        "window.__krabRoutes[search] = { body: body, delay: delay };",
        &[
            JsValue::from_str(search),
            JsValue::from_str(body),
            JsValue::from_f64(delay_ms as f64),
        ],
    );
}

/// Register a mock whose `response.url` differs from the requested href, the
/// way a server redirect reports itself.
fn redirecting_route(search: &str, body: &str, final_url: &str) {
    eval(
        "search, body, url",
        "window.__krabRoutes[search] = { body: body, delay: 0, url: url };",
        &[
            JsValue::from_str(search),
            JsValue::from_str(body),
            JsValue::from_str(final_url),
        ],
    );
}

thread_local! {
    /// `event.defaultPrevented` as the recorder saw it — that is, after the
    /// router's capturing listener had its say and before anything else did.
    static ROUTER_PREVENTED: Cell<bool> = const { Cell::new(false) };
    /// Whether the router and the recorder are already installed on this page.
    static STARTED: Cell<bool> = const { Cell::new(false) };
}

/// Capture-phase recorder, registered after the router's own listener so it runs
/// second. Also swallows the default so a declined click cannot navigate the
/// harness away.
fn install_click_recorder() {
    let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        ROUTER_PREVENTED.with(|cell| cell.set(event.default_prevented()));
        event.prevent_default();
    });

    document()
        .add_event_listener_with_callback_and_bool("click", handler.as_ref().unchecked_ref(), true)
        .expect("failed to install click recorder");
    handler.forget();
}

/// Everything a test needs in place: mock network, router, click recorder, and a
/// freshly mounted outlet. Idempotent — the router and the listeners install
/// once per page, the markup is rebuilt per test.
fn setup(home_body: &str) -> Element {
    install_fetch_mock();
    route("__default", &page_document("Home", home_body), 0);

    let root = support::mount_container(TEST_ROOT_ID);
    root.set_inner_html(&format!(
        "<nav id=\"krab-router-test-nav\"></nav>\
         <main id=\"{OUTLET_ID}\" data-krab-router-outlet tabindex=\"-1\">{home_body}</main>\
         <div style=\"height:{SPACER_HEIGHT_PX}px\"></div>"
    ));

    // The router installs global listeners and is itself idempotent, but the
    // recorder is not — a second copy would fire twice per click.
    if !STARTED.with(|started| started.replace(true)) {
        krab_client::router::start();
        install_click_recorder();
    }

    outlet()
}

fn outlet() -> Element {
    document()
        .query_selector(&format!("#{OUTLET_ID}"))
        .expect("query failed")
        .expect("outlet not mounted")
}

fn outlet_text() -> String {
    outlet().text_content().unwrap_or_default()
}

/// The document URL the harness was loaded at, restored after any test that
/// pushes history.
fn location_href() -> String {
    web_sys::window()
        .expect("no window")
        .location()
        .href()
        .expect("no href")
}

fn restore_location(href: &str) {
    eval(
        "href",
        "history.replaceState(null, '', href);",
        &[JsValue::from_str(href)],
    );
}

/// Sleep on the browser's task queue. [`support::settle`] only drains
/// microtasks, which is not enough for a `setTimeout`-delayed response or for
/// the `popstate` a `history.back()` produces.
async fn sleep(ms: i32) {
    let window = web_sys::window().expect("no window");
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        window
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
            .expect("setTimeout failed");
    });
    let _ = JsFuture::from(promise).await;
}

/// Poll until `ready` holds, up to about a second and a half.
///
/// Fixed sleeps make a suite that passes on a fast machine and flakes on a busy
/// one; a bounded poll fails only when the thing genuinely never happens.
async fn wait_until<F: Fn() -> bool>(ready: F) -> bool {
    for _ in 0..60 {
        if ready() {
            return true;
        }
        sleep(25).await;
    }
    false
}

/// Dispatch a `click` the way a browser would, with the button and modifier
/// state under test. Returns whether the router prevented the default.
fn click(target: &Element, button: i16, ctrl_key: bool) -> bool {
    ROUTER_PREVENTED.with(|cell| cell.set(false));
    eval(
        "target, button, ctrl",
        "target.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, \
         button: button, ctrlKey: ctrl }));",
        &[
            target.clone().into(),
            JsValue::from_f64(button as f64),
            JsValue::from_bool(ctrl_key),
        ],
    );
    ROUTER_PREVENTED.with(|cell| cell.get())
}

/// Put an anchor in the never-swapped nav so it survives an outlet swap.
fn nav_link(href: &str) -> Element {
    let nav = document()
        .query_selector("#krab-router-test-nav")
        .expect("query failed")
        .expect("nav not mounted");
    nav.set_inner_html(&format!(
        "<a id=\"krab-router-test-link\" href=\"{href}\">go</a>"
    ));
    nav.query_selector("a")
        .expect("query failed")
        .expect("link not mounted")
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// The headline behaviour, and the one that never reached a bundle before
/// `start` carried `#[wasm_bindgen(js_name = start_router)]`: a plain
/// same-origin click is handled in place instead of by the browser.
#[wasm_bindgen_test]
async fn a_same_origin_click_is_intercepted_and_swaps_the_outlet() {
    let original = location_href();
    let outlet = setup("HOME");
    route(
        "?krab-route=about",
        &page_document("About", "<p>ABOUT</p>"),
        0,
    );

    let link = nav_link("?krab-route=about");
    assert!(
        click(&link, 0, false),
        "the router must preventDefault a same-origin click, or the browser \
         performs a full page load"
    );

    settle().await;
    sleep(60).await;

    assert!(
        outlet_text().contains("ABOUT"),
        "the outlet must hold the destination's outlet content; was: {}",
        outlet.inner_html()
    );
    assert_eq!(
        document().title(),
        "About",
        "the destination's title must be published to the document"
    );

    restore_location(&original);
}

/// The bug users notice fastest. A synthetic ctrl-click or middle-click must
/// leave `defaultPrevented` false so the browser can open a new tab, and must
/// not swap anything.
#[wasm_bindgen_test]
async fn modified_and_middle_clicks_are_left_to_the_browser() {
    setup("HOME");
    route(
        "?krab-route=about",
        &page_document("About", "<p>ABOUT</p>"),
        0,
    );

    let link = nav_link("?krab-route=about");

    assert!(
        !click(&link, 0, true),
        "a ctrl-click must reach the browser unprevented"
    );
    assert!(
        !click(&link, 1, false),
        "a middle-click must reach the browser unprevented"
    );

    settle().await;
    sleep(60).await;

    assert!(
        outlet_text().contains("HOME"),
        "neither click may swap the outlet; was: {}",
        outlet_text()
    );
}

/// An anchor that opts out, and one that points off-origin, must both be left
/// alone — the same rules the pure unit tests pin, now through the real DOM
/// path that reads them off an `HtmlAnchorElement`.
#[wasm_bindgen_test]
async fn opted_out_and_cross_origin_links_are_left_to_the_browser() {
    setup("HOME");

    let nav = document()
        .query_selector("#krab-router-test-nav")
        .expect("query failed")
        .expect("nav not mounted");
    nav.set_inner_html(
        "<a id=\"opted\" href=\"?krab-route=about\" data-krab-router-ignore>a</a>\
         <a id=\"external\" href=\"https://example.invalid/page\">b</a>",
    );

    let opted = nav
        .query_selector("#opted")
        .expect("query failed")
        .expect("no opted-out link");
    let external = nav
        .query_selector("#external")
        .expect("query failed")
        .expect("no external link");

    assert!(
        !click(&opted, 0, false),
        "data-krab-router-ignore must disable interception"
    );
    assert!(
        !click(&external, 0, false),
        "a cross-origin link is the browser's job"
    );

    settle().await;
    sleep(60).await;
    assert!(outlet_text().contains("HOME"));
}

/// Back must restore both the previous view and where the user was in it.
///
/// Three defects meet here. `scroll_to(0, 0)` fired on `popstate` too, so Back
/// always jumped to the top; nothing wrote the offset anywhere it could be read
/// back from; and the fragment guard compared `location.href` against itself on
/// `popstate` — already the destination by the time the event fires — so a
/// traversal was mistaken for a same-page hash change and dropped entirely.
///
/// Both history entries are created by this test rather than inherited from the
/// harness, so what Back lands on does not depend on the page's initial URL or
/// on what any earlier test pushed.
#[wasm_bindgen_test]
async fn back_restores_the_previous_view_and_its_scroll_position() {
    let original = location_href();
    setup("HOME");
    route(
        "?krab-route=first",
        &page_document("First", "<p>FIRST</p>"),
        0,
    );
    route(
        "?krab-route=second",
        &page_document("Second", "<p>SECOND</p>"),
        0,
    );

    let window = web_sys::window().expect("no window");

    krab_client::router::navigate_to("?krab-route=first", true).await;
    assert!(
        wait_until(|| outlet_text().contains("FIRST")).await,
        "the first navigation never landed; outlet={}",
        outlet_text()
    );

    window.scroll_to_with_x_and_y(0.0, 400.0);
    sleep(50).await;
    let departed_from = window.scroll_y().unwrap_or(0.0);
    assert!(
        departed_from > 100.0,
        "the test page must actually scroll for this to mean anything; scrollY={departed_from}"
    );

    krab_client::router::navigate_to("?krab-route=second", true).await;
    assert!(
        wait_until(|| outlet_text().contains("SECOND")).await,
        "the second navigation never landed; outlet={}",
        outlet_text()
    );
    assert!(
        window.scroll_y().unwrap_or(-1.0) < 10.0,
        "a forward navigation must land at the top; scrollY={}",
        window.scroll_y().unwrap_or(-1.0)
    );

    eval("", "history.back();", &[]);
    assert!(
        wait_until(|| outlet_text().contains("FIRST")).await,
        "Back must restore the previous view; outlet={} url={}",
        outlet_text(),
        location_href()
    );

    // The scroll restore is the last thing the navigation does.
    assert!(
        wait_until(|| (window.scroll_y().unwrap_or(-1.0) - departed_from).abs() < 5.0).await,
        "Back must restore the scroll position it left at ({departed_from}), not \
         jump to the top; scrollY={}",
        window.scroll_y().unwrap_or(-1.0)
    );

    window.scroll_to_with_x_and_y(0.0, 0.0);
    restore_location(&original);
}

/// Two clicks in quick succession: the slower response belongs to a navigation
/// the user has already abandoned and must not be painted.
///
/// Without the generation counter this is a genuine race and the *first*,
/// slower page wins — the address bar says one thing and the content says
/// another, and the only symptom is an occasional wrong page.
#[wasm_bindgen_test]
async fn a_stale_response_does_not_overwrite_a_newer_navigation() {
    setup("HOME");
    route(
        "?krab-route=slow",
        &page_document("Slow", "<p>SLOW</p>"),
        300,
    );
    route("?krab-route=fast", &page_document("Fast", "<p>FAST</p>"), 0);

    // The slow navigation starts first and is deliberately not awaited.
    krab_client::spawn(async {
        krab_client::router::navigate_to("?krab-route=slow", false).await;
    });
    settle().await;

    krab_client::router::navigate_to("?krab-route=fast", false).await;
    assert!(
        outlet_text().contains("FAST"),
        "the newer navigation must land; was: {}",
        outlet_text()
    );

    // Past the slow response's arrival.
    sleep(500).await;
    settle().await;

    assert!(
        outlet_text().contains("FAST"),
        "the abandoned navigation's response overwrote newer content; was: {}",
        outlet_text()
    );
    assert!(
        !outlet_text().contains("SLOW"),
        "stale content reached the page"
    );
}

/// A server redirect must be reflected in the address bar. Pushing the
/// *requested* href leaves `/login` showing while `/dashboard` is on screen, and
/// a reload then bounces through the redirect again.
#[wasm_bindgen_test]
async fn history_records_the_url_the_response_came_from() {
    let original = location_href();
    setup("HOME");

    let redirected = {
        let base = original.split('?').next().unwrap_or(&original).to_string();
        format!("{base}?krab-route=final")
    };
    redirecting_route(
        "?krab-route=redirect",
        &page_document("Final", "<p>FINAL</p>"),
        &redirected,
    );
    route(
        "?krab-route=final",
        &page_document("Final", "<p>FINAL</p>"),
        0,
    );

    krab_client::router::navigate_to("?krab-route=redirect", true).await;
    sleep(60).await;

    assert!(
        location_href().contains("krab-route=final"),
        "history must record response.url, not the requested href; url was: {}",
        location_href()
    );

    restore_location(&original);
}

/// After a swap, focus must move into the new content and the navigation must
/// be announced — neither of which a screen reader gets for free, because the
/// document never "loaded".
#[wasm_bindgen_test]
async fn a_swap_moves_focus_to_the_outlet_and_announces_the_page() {
    setup("HOME");
    route(
        "?krab-route=about",
        &page_document("About", "<p>ABOUT</p>"),
        0,
    );

    krab_client::router::navigate_to("?krab-route=about", false).await;
    sleep(60).await;

    let focused = document().active_element().expect("nothing is focused");
    assert_eq!(
        focused.id(),
        OUTLET_ID,
        "focus must land in the swapped content, not stay on a destroyed link"
    );

    let announcer = document()
        .get_element_by_id(krab_client::router::ANNOUNCER_ID)
        .expect("no live region was created");
    assert_eq!(
        announcer.get_attribute("aria-live").as_deref(),
        Some("polite")
    );
    assert_eq!(
        announcer.text_content().unwrap_or_default(),
        "About",
        "the live region must carry the new page's title"
    );
}
