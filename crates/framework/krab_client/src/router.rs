//! Client-side routing: same-origin navigation without a full page load.
//!
//! Krab renders on the server and hydrates islands. Without a client router
//! every in-app link is a full document request, which discards the hydrated
//! island state, the scroll position, and the WASM module's warm state.
//!
//! # How it works
//!
//! [`start`] installs two listeners:
//!
//! - a capturing `click` listener on `document`, which intercepts anchors that
//!   [`should_intercept`] approves and navigates in place instead;
//! - a `popstate` listener, so Back and Forward route through the same path.
//!
//! A navigation fetches the target URL, takes the contents of the **router
//! outlet** — the element marked `data-krab-router-outlet` — from the response,
//! swaps it into the live document, and re-runs [`hydrate`](crate::hydrate) so
//! islands in the new markup come alive.
//!
//! ```html
//! <body>
//!   <nav><a href="/about">About</a></nav>
//!   <main data-krab-router-outlet>
//!     <!-- swapped on navigation; everything outside is left alone -->
//!   </main>
//! </body>
//! ```
//!
//! # What it deliberately does not do
//!
//! - **No nested layouts.** One outlet per document. Nested outlets need a
//!   route tree the framework does not have yet.
//! - **No prefetch.** Deliberate: prefetch-on-hover is easy to add and easy to
//!   get wrong (it turns a hover into a server request), so it is a separate
//!   decision.
//! - **No client-side route table.** The server remains the router; this only
//!   avoids the document reload. That keeps SSR, ISR, and render policy
//!   authoritative rather than duplicating routing rules in two places.
//!
//! Every failure falls back to a normal browser navigation, so a broken
//! response, an offline network, or a missing outlet degrades to what would
//! have happened without the router rather than to a blank page.

/// Attribute marking the element whose contents are swapped on navigation.
pub const OUTLET_ATTR: &str = "data-krab-router-outlet";

/// Attribute that opts a single anchor out of client-side routing.
///
/// `<a href="/admin" data-krab-router-ignore>` always does a full page load.
/// Useful for links that cross into a differently-built part of the site.
pub const IGNORE_ATTR: &str = "data-krab-router-ignore";

/// Everything about a click needed to decide whether to intercept it.
///
/// Split out from the DOM so the decision is a pure function: the rules below
/// are where routers get subtly wrong (swallowing a middle-click, hijacking a
/// download link), and those bugs are invisible in a browser test that only
/// checks the happy path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkClick {
    /// Resolved absolute href of the anchor.
    pub href: String,
    /// Origin of the anchor's URL, e.g. `https://example.com`.
    pub link_origin: String,
    /// Origin of the current document.
    pub document_origin: String,
    /// `target` attribute, if any.
    pub target: Option<String>,
    /// Whether the anchor carries `download`.
    pub has_download: bool,
    /// Whether the anchor carries [`IGNORE_ATTR`].
    pub opted_out: bool,
    /// `event.button`: 0 is the primary button.
    pub button: i16,
    /// Whether any of Ctrl / Shift / Alt / Meta was held.
    pub modifier_held: bool,
    /// Whether `preventDefault()` has already been called by other code.
    pub default_prevented: bool,
}

/// Whether a click should be handled in-page rather than by the browser.
///
/// Returns `false` — meaning "let the browser do it" — for every case where
/// intercepting would break an expectation the user already has:
///
/// - a modifier key or a non-primary button (open in new tab, new window)
/// - `target` other than `_self`
/// - `download`
/// - a cross-origin URL
/// - an explicit [`IGNORE_ATTR`] opt-out
/// - an event something else already handled
pub fn should_intercept(click: &LinkClick) -> bool {
    if click.default_prevented || click.opted_out || click.has_download {
        return false;
    }

    // Middle-click and ctrl/cmd-click mean "open elsewhere". Swallowing them is
    // the most common client-router bug and users notice immediately.
    if click.button != 0 || click.modifier_held {
        return false;
    }

    if let Some(target) = &click.target {
        if !target.is_empty() && target != "_self" {
            return false;
        }
    }

    // Cross-origin navigation is the browser's job. Comparing origins rather
    // than hosts keeps http/https and port differences out.
    if click.link_origin != click.document_origin {
        return false;
    }

    // A bare fragment is in-page scrolling, not navigation. `/docs#usage` from
    // `/docs` is also just a scroll, but that is handled after the fetch by
    // comparing paths — here we only skip the unambiguous case.
    if click.href.starts_with('#') {
        return false;
    }

    true
}

/// Whether two same-origin URLs differ only by fragment.
///
/// `/docs#a` -> `/docs#b` must scroll, not refetch.
pub fn is_same_document_fragment(current: &str, next: &str) -> bool {
    strip_fragment(current) == strip_fragment(next) && next.contains('#')
}

fn strip_fragment(url: &str) -> &str {
    match url.split_once('#') {
        Some((base, _)) => base,
        None => url,
    }
}

/// Extract the outlet's inner HTML from a full document string.
///
/// Returns `None` when the response has no outlet, which is the signal to fall
/// back to a full page load — the target page is not built for client routing,
/// and swapping a guess would produce a broken page.
///
/// A deliberately small scanner rather than a DOM parse: this runs on the
/// navigation hot path, and parsing the entire document to read one subtree is
/// wasteful when the marker is unambiguous.
pub fn extract_outlet_html(document_html: &str) -> Option<String> {
    let marker = document_html.find(OUTLET_ATTR)?;

    // Find the '>' closing the outlet's opening tag, and the '<' opening it.
    let open_tag_end = document_html[marker..].find('>')? + marker + 1;
    let open_tag_start = document_html[..marker].rfind('<')?;
    let tag_name = tag_name_at(document_html, open_tag_start)?;

    // Self-closing outlet has nothing to swap in.
    if document_html[..open_tag_end].ends_with("/>") {
        return Some(String::new());
    }

    let close = format!("</{tag_name}");
    let mut depth = 1usize;
    let mut cursor = open_tag_end;

    // Nesting matters: `<main …><main>…</main></main>` must match the outer
    // close tag, not the first one encountered.
    while depth > 0 {
        let rest = &document_html[cursor..];
        let next_open = find_tag(rest, &format!("<{tag_name}"));
        let next_close = find_tag(rest, &close)?;

        match next_open {
            Some(open_at) if open_at < next_close => {
                depth += 1;
                cursor += open_at + 1;
            }
            _ => {
                depth -= 1;
                if depth == 0 {
                    return Some(document_html[open_tag_end..cursor + next_close].to_string());
                }
                cursor += next_close + 1;
            }
        }
    }

    None
}

/// Position of `needle` in `haystack`, only where it is a real tag boundary.
///
/// Prevents `<mains>` from matching a search for `<main`.
fn find_tag(haystack: &str, needle: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(found) = haystack[from..].find(needle) {
        let at = from + found;
        let after = haystack[at + needle.len()..].chars().next();
        match after {
            // `<main>`, `<main `, `<main/`, `</main>`
            None | Some('>') | Some(' ') | Some('/') | Some('\n') | Some('\r') | Some('\t') => {
                return Some(at)
            }
            _ => from = at + needle.len(),
        }
    }
    None
}

/// Read the tag name from a `<` at `start`.
fn tag_name_at(html: &str, start: usize) -> Option<String> {
    let rest = html.get(start + 1..)?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();

    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

// ── Browser wiring ──────────────────────────────────────────────────────────

#[cfg(all(feature = "web", target_arch = "wasm32"))]
mod browser {
    use super::*;
    use crate::hydrate;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::spawn_local;
    use web_sys::{Document, Element, HtmlAnchorElement, MouseEvent, Window};

    /// Install the click and popstate listeners.
    ///
    /// Idempotent: calling it twice does not double-handle clicks.
    pub fn start() {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(document) = window.document() else {
            return;
        };

        if document
            .document_element()
            .and_then(|el| el.get_attribute("data-krab-router-active"))
            .is_some()
        {
            return;
        }
        if let Some(root) = document.document_element() {
            let _ = root.set_attribute("data-krab-router-active", "1");
        }

        install_click_listener(&document);
        install_popstate_listener(&window);
    }

    fn install_click_listener(document: &Document) {
        let handler = Closure::<dyn FnMut(MouseEvent)>::new(move |event: MouseEvent| {
            let Some(anchor) = closest_anchor(&event) else {
                return;
            };
            let Some(click) = describe_click(&event, &anchor) else {
                return;
            };

            if !should_intercept(&click) {
                return;
            }

            event.prevent_default();
            let href = click.href.clone();
            spawn_local(async move {
                navigate_to(&href, true).await;
            });
        });

        // Capturing, so the router sees the click before handlers that might
        // stop propagation on a wrapper element.
        let _ = document.add_event_listener_with_callback_and_bool(
            "click",
            handler.as_ref().unchecked_ref(),
            true,
        );
        handler.forget();
    }

    fn install_popstate_listener(window: &Window) {
        let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let Some(href) = current_href() else {
                return;
            };
            spawn_local(async move {
                // Back/Forward: the browser already moved history, so do not
                // push another entry.
                navigate_to(&href, false).await;
            });
        });

        let _ =
            window.add_event_listener_with_callback("popstate", handler.as_ref().unchecked_ref());
        handler.forget();
    }

    fn closest_anchor(event: &MouseEvent) -> Option<HtmlAnchorElement> {
        let target = event.target()?.dyn_into::<Element>().ok()?;
        target
            .closest("a")
            .ok()
            .flatten()
            .and_then(|el| el.dyn_into::<HtmlAnchorElement>().ok())
    }

    fn describe_click(event: &MouseEvent, anchor: &HtmlAnchorElement) -> Option<LinkClick> {
        let document_origin = web_sys::window()?.location().origin().ok()?;

        Some(LinkClick {
            href: anchor.href(),
            link_origin: anchor.origin(),
            document_origin,
            target: Some(anchor.target()),
            has_download: anchor.has_attribute("download"),
            opted_out: anchor.has_attribute(IGNORE_ATTR),
            button: event.button(),
            modifier_held: event.ctrl_key()
                || event.shift_key()
                || event.alt_key()
                || event.meta_key(),
            default_prevented: event.default_prevented(),
        })
    }

    fn current_href() -> Option<String> {
        web_sys::window()?.location().href().ok()
    }

    /// Fetch `href`, swap the outlet, re-hydrate.
    ///
    /// Any failure falls back to a full browser navigation.
    pub async fn navigate_to(href: &str, push: bool) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(document) = window.document() else {
            return;
        };

        if let Some(current) = current_href() {
            if is_same_document_fragment(&current, href) {
                if push {
                    push_history(href);
                }
                return;
            }
        }

        let Some(outlet) = document
            .query_selector(&format!("[{OUTLET_ATTR}]"))
            .ok()
            .flatten()
        else {
            // No outlet in the *current* page: this document is not set up for
            // client routing.
            hard_navigate(href);
            return;
        };

        let html = match fetch_document(href).await {
            Some(html) => html,
            None => {
                hard_navigate(href);
                return;
            }
        };

        let Some(next_html) = extract_outlet_html(&html) else {
            // The destination has no outlet — it is not a client-routable page.
            hard_navigate(href);
            return;
        };

        outlet.set_inner_html(&next_html);

        if let Some(title) = extract_title(&html) {
            document.set_title(&title);
        }

        if push {
            push_history(href);
        }

        // New markup means new islands.
        hydrate();

        window.scroll_to_with_x_and_y(0.0, 0.0);
    }

    async fn fetch_document(href: &str) -> Option<String> {
        use wasm_bindgen_futures::JsFuture;

        let window = web_sys::window()?;
        let opts = web_sys::RequestInit::new();
        opts.set_method("GET");

        let request = web_sys::Request::new_with_str_and_init(href, &opts).ok()?;
        // Lets the server distinguish a router fetch from a document request.
        request.headers().set("x-krab-router", "1").ok()?;
        request.headers().set("accept", "text/html").ok()?;

        let response: web_sys::Response = JsFuture::from(window.fetch_with_request(&request))
            .await
            .ok()?
            .dyn_into()
            .ok()?;

        if !response.ok() {
            return None;
        }

        JsFuture::from(response.text().ok()?)
            .await
            .ok()?
            .as_string()
    }

    fn push_history(href: &str) {
        if let Some(window) = web_sys::window() {
            let _ = window
                .history()
                .map(|h| h.push_state_with_url(&JsValue::NULL, "", Some(href)));
        }
    }

    fn hard_navigate(href: &str) {
        if let Some(window) = web_sys::window() {
            let _ = window.location().assign(href);
        }
    }

    fn extract_title(html: &str) -> Option<String> {
        let start = html.find("<title>")? + "<title>".len();
        let end = html[start..].find("</title>")? + start;
        Some(html[start..end].to_string())
    }
}

#[cfg(all(feature = "web", target_arch = "wasm32"))]
pub use browser::{navigate_to, start};

#[cfg(test)]
mod tests {
    use super::*;

    fn base_click() -> LinkClick {
        LinkClick {
            href: "https://example.com/about".to_string(),
            link_origin: "https://example.com".to_string(),
            document_origin: "https://example.com".to_string(),
            target: None,
            has_download: false,
            opted_out: false,
            button: 0,
            modifier_held: false,
            default_prevented: false,
        }
    }

    #[test]
    fn a_plain_same_origin_click_is_intercepted() {
        assert!(should_intercept(&base_click()));
    }

    /// The bug users notice fastest: ctrl-click and middle-click must keep
    /// meaning "open in a new tab".
    #[test]
    fn modified_and_non_primary_clicks_are_left_to_the_browser() {
        let modified = LinkClick {
            modifier_held: true,
            ..base_click()
        };
        assert!(!should_intercept(&modified));

        let middle = LinkClick {
            button: 1,
            ..base_click()
        };
        assert!(!should_intercept(&middle));
    }

    #[test]
    fn cross_origin_links_are_left_to_the_browser() {
        let external = LinkClick {
            href: "https://other.example/page".to_string(),
            link_origin: "https://other.example".to_string(),
            ..base_click()
        };
        assert!(!should_intercept(&external));
    }

    /// Same host, different scheme or port is still cross-origin.
    #[test]
    fn origin_comparison_is_not_a_host_comparison() {
        let other_port = LinkClick {
            link_origin: "https://example.com:8443".to_string(),
            ..base_click()
        };
        assert!(!should_intercept(&other_port));
    }

    #[test]
    fn targeted_and_download_links_are_left_to_the_browser() {
        let new_tab = LinkClick {
            target: Some("_blank".to_string()),
            ..base_click()
        };
        assert!(!should_intercept(&new_tab));

        let download = LinkClick {
            has_download: true,
            ..base_click()
        };
        assert!(!should_intercept(&download));
    }

    #[test]
    fn an_empty_or_self_target_is_still_intercepted() {
        for target in ["", "_self"] {
            let click = LinkClick {
                target: Some(target.to_string()),
                ..base_click()
            };
            assert!(should_intercept(&click), "target={target:?}");
        }
    }

    #[test]
    fn an_explicit_opt_out_is_honoured() {
        let opted_out = LinkClick {
            opted_out: true,
            ..base_click()
        };
        assert!(!should_intercept(&opted_out));
    }

    #[test]
    fn an_already_handled_event_is_not_hijacked() {
        let handled = LinkClick {
            default_prevented: true,
            ..base_click()
        };
        assert!(!should_intercept(&handled));
    }

    #[test]
    fn bare_fragments_are_left_to_the_browser() {
        let fragment = LinkClick {
            href: "#section".to_string(),
            ..base_click()
        };
        assert!(!should_intercept(&fragment));
    }

    #[test]
    fn same_page_fragment_navigation_is_detected() {
        assert!(is_same_document_fragment("/docs", "/docs#usage"));
        assert!(is_same_document_fragment("/docs#intro", "/docs#usage"));
        assert!(!is_same_document_fragment("/docs", "/guide#usage"));
        // No fragment at all is a real navigation.
        assert!(!is_same_document_fragment("/docs", "/docs"));
    }

    #[test]
    fn outlet_contents_are_extracted() {
        let html = r#"<html><body>
            <nav>keep me</nav>
            <main data-krab-router-outlet><h1>Page</h1></main>
        </body></html>"#;

        let extracted = extract_outlet_html(html).expect("outlet not found");
        assert_eq!(extracted.trim(), "<h1>Page</h1>");
        assert!(!extracted.contains("keep me"));
    }

    /// The subtle one: a nested element of the same tag must not terminate the
    /// scan early, or the swapped markup is silently truncated.
    #[test]
    fn nested_same_tag_elements_do_not_truncate_the_outlet() {
        let html = r#"<main data-krab-router-outlet>
            <main class="inner">nested</main><p>after</p>
        </main>"#;

        let extracted = extract_outlet_html(html).expect("outlet not found");
        assert!(extracted.contains("nested"), "got: {extracted}");
        assert!(
            extracted.contains("<p>after</p>"),
            "content after the nested element was truncated: {extracted}"
        );
    }

    #[test]
    fn a_tag_with_a_shared_prefix_does_not_close_the_outlet() {
        // `</mains>` must not be mistaken for `</main>`.
        let html = r#"<main data-krab-router-outlet><mains>x</mains><p>tail</p></main>"#;

        let extracted = extract_outlet_html(html).expect("outlet not found");
        assert!(extracted.contains("<p>tail</p>"), "got: {extracted}");
    }

    #[test]
    fn a_document_without_an_outlet_yields_none() {
        let html = "<html><body><main>no marker</main></body></html>";
        assert!(extract_outlet_html(html).is_none());
    }

    #[test]
    fn a_self_closing_outlet_yields_empty_content() {
        let html = r#"<div data-krab-router-outlet/>"#;
        assert_eq!(extract_outlet_html(html).as_deref(), Some(""));
    }

    #[test]
    fn outlet_works_on_tags_other_than_main() {
        let html = r#"<section id="x" data-krab-router-outlet><p>hi</p></section>"#;
        assert_eq!(extract_outlet_html(html).as_deref(), Some("<p>hi</p>"));
    }
}
