//! Client-side routing: same-origin navigation without a full page load.
//!
//! Krab renders on the server and hydrates islands. Without a client router
//! every in-app link is a full document request, which discards the hydrated
//! island state, the scroll position, and the WASM module's warm state.
//!
//! # How it works
//!
//! `start` — exported to JavaScript as `start_router`, and available on
//! `wasm32` with the `web` feature — installs two listeners:
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
//!   <main data-krab-router-outlet tabindex="-1">
//!     <!-- swapped on navigation; everything outside is left alone -->
//!   </main>
//! </body>
//! ```
//!
//! # The export matters
//!
//! `start` carries `#[wasm_bindgen(js_name = start_router)]`. Without it the
//! whole module is dead code from the linker's point of view — nothing in the
//! crate calls it, and the production load path is JavaScript
//! (`import('/pkg/krab_client.js')`), which can only reach `#[wasm_bindgen]`
//! exports. A bundle built before that attribute existed contained no trace of
//! `data-krab-router-outlet` at all: the router shipped in the source tree and
//! nowhere else.
//!
//! # What a navigation does, in order
//!
//! 1. Claims a **navigation generation**. Every later step re-checks it, so a
//!    slow response for an abandoned navigation cannot overwrite the content of
//!    a newer one. Two fast clicks leave the second one's page on screen.
//! 2. Fetches the destination.
//! 3. Records the current scroll position into the **current** history entry
//!    (before the swap, while the position is still meaningful).
//! 4. Calls [`unmount`](crate::unmount) on the outlet, so the outgoing subtree
//!    releases its event closures and dynamic-region registrations rather than
//!    being dropped on the floor by `innerHTML`.
//! 5. Swaps in the new markup, sets the title, pushes history.
//! 6. Re-hydrates, moves focus to the outlet, and announces the new page in a
//!    polite live region.
//! 7. Scrolls to the top on a forward navigation; restores the saved position on
//!    Back or Forward.
//!
//! # Accessibility contract
//!
//! A content swap is invisible to a screen reader and leaves focus on a link
//! that no longer exists. After every swap the router therefore focuses the
//! outlet (adding `tabindex="-1"` if the markup did not) and writes the new page
//! title into a visually hidden `role="status"` region. Give the outlet
//! `tabindex="-1"` in your own markup if you would rather not have the attribute
//! appear after the first navigation.
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
//! - **No scroll capture on the way out of a Back.** The position of an entry is
//!   written when a *forward* navigation leaves it. `popstate` fires after the
//!   browser has already moved off the previous entry, so there is no entry left
//!   to write it to. Going Back and then Forward returns to the top of the page
//!   rather than to where you were. Fixing it needs a throttled `scroll`
//!   listener rewriting history state continuously, which is a real cost on
//!   every page for a narrow case.
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

/// Id of the live region the router announces navigations through.
pub const ANNOUNCER_ID: &str = "krab-router-announcer";

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

// ── Markup scanning ─────────────────────────────────────────────────────────
//
// A deliberately small tag scanner rather than a DOM parse: this runs on the
// navigation hot path, and building a whole document to read one subtree is
// wasteful. It is *not* a parser — it knows only enough to tell a real tag from
// text that looks like one.
//
// The naive version of this (`html.find(OUTLET_ATTR)`) matched the marker
// wherever it appeared, including inside a comment, inside a `<script>`, and
// inside another element's attribute *value* — so a page that merely mentioned
// the attribute in prose swapped the wrong subtree, silently. The scanner below
// skips comments, doctypes, processing instructions, and the contents of
// raw-text elements, and only ever matches the marker as an attribute *name*.

/// A start or end tag located by [`next_tag`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tag {
    /// Lowercased tag name.
    name: String,
    /// Byte offset of the opening `<`.
    start: usize,
    /// Byte offset just past the closing `>`.
    end: usize,
    kind: TagKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagKind {
    Start {
        self_closing: bool,
        /// Whether [`OUTLET_ATTR`] appears as an attribute name on this tag.
        outlet: bool,
    },
    End,
}

/// Elements whose content is text, not markup. A `</main>` inside one of these
/// is characters, not a close tag, and must not terminate the outlet scan.
fn is_raw_text(name: &str) -> bool {
    matches!(name, "script" | "style" | "textarea" | "title")
}

/// The next real tag at or after `from`, skipping comments, doctypes, and bare
/// `<` characters that do not begin one.
fn next_tag(html: &str, from: usize) -> Option<Tag> {
    let mut pos = from;

    loop {
        let at = pos + html.get(pos..)?.find('<')?;
        let rest = &html[at..];

        if let Some(body) = rest.strip_prefix("<!--") {
            // An unterminated comment swallows the rest of the document, which
            // is what a browser does too.
            pos = at + 4 + body.find("-->")? + 3;
            continue;
        }

        if rest.starts_with("<!") || rest.starts_with("<?") {
            pos = at + rest.find('>')? + 1;
            continue;
        }

        if let Some(tag) = parse_tag(html, at) {
            return Some(tag);
        }

        // `a < b` in text: not a tag, keep looking.
        pos = at + 1;
    }
}

/// Parse the tag whose `<` is at `at`, or `None` if it is not one.
fn parse_tag(html: &str, at: usize) -> Option<Tag> {
    let after = html.get(at + 1..)?;
    let is_end = after.starts_with('/');
    let name_start = at + 1 + usize::from(is_end);

    let name: String = html
        .get(name_start..)?
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();

    // A tag name starts with a letter; anything else is text.
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }

    let (end, self_closing, outlet) = scan_attributes(html, name_start + name.len())?;

    Some(Tag {
        name: name.to_ascii_lowercase(),
        start: at,
        end,
        kind: if is_end {
            TagKind::End
        } else {
            TagKind::Start {
                self_closing,
                outlet,
            }
        },
    })
}

/// Walk a start tag's attributes to its `>`.
///
/// Returns the offset just past the `>`, whether the tag was self-closing, and
/// whether [`OUTLET_ATTR`] appeared as an attribute *name*. Quoted values are
/// consumed whole, so a `data-krab-router-outlet` mentioned inside one is text.
fn scan_attributes(html: &str, from: usize) -> Option<(usize, bool, bool)> {
    let bytes = html.as_bytes();
    let len = bytes.len();
    let mut pos = from;
    let mut outlet = false;

    loop {
        while pos < len && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= len {
            // Unterminated tag.
            return None;
        }

        match bytes[pos] {
            b'>' => return Some((pos + 1, false, outlet)),
            b'/' if html[pos..].starts_with("/>") => return Some((pos + 2, true, outlet)),
            b'/' => pos += 1,
            _ => {
                let name_start = pos;
                while pos < len
                    && !bytes[pos].is_ascii_whitespace()
                    && !matches!(bytes[pos], b'=' | b'>' | b'/')
                {
                    pos += 1;
                }

                // A stray `=` with no name in front of it: step over it rather
                // than spin.
                if pos == name_start {
                    pos += 1;
                    continue;
                }

                if html[name_start..pos].eq_ignore_ascii_case(OUTLET_ATTR) {
                    outlet = true;
                }

                pos = skip_attribute_value(html, pos);
            }
        }
    }
}

/// If an `=` follows at `from`, consume it and the value; otherwise leave `from`
/// where it is (a valueless attribute such as the outlet marker itself).
fn skip_attribute_value(html: &str, from: usize) -> usize {
    let bytes = html.as_bytes();
    let len = bytes.len();
    let mut probe = from;

    while probe < len && bytes[probe].is_ascii_whitespace() {
        probe += 1;
    }
    if probe >= len || bytes[probe] != b'=' {
        return from;
    }

    probe += 1;
    while probe < len && bytes[probe].is_ascii_whitespace() {
        probe += 1;
    }
    if probe >= len {
        return probe;
    }

    match bytes[probe] {
        quote @ (b'"' | b'\'') => {
            probe += 1;
            while probe < len && bytes[probe] != quote {
                probe += 1;
            }
            // Past the closing quote, clamped for an unterminated value.
            (probe + 1).min(len)
        }
        _ => {
            while probe < len && !bytes[probe].is_ascii_whitespace() && bytes[probe] != b'>' {
                probe += 1;
            }
            probe
        }
    }
}

/// Offset of the `</name` that ends a raw-text element, or the end of the
/// document.
fn skip_raw_text(html: &str, name: &str, from: usize) -> usize {
    find_ignore_ascii_case(html, &format!("</{name}"), from).unwrap_or(html.len())
}

/// Case-insensitive substring search from `from`.
fn find_ignore_ascii_case(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let hay = haystack.as_bytes();
    let ned = needle.as_bytes();

    if ned.is_empty() || hay.len() < ned.len() || from > hay.len() - ned.len() {
        return None;
    }

    (from..=hay.len() - ned.len()).find(|&i| hay[i..i + ned.len()].eq_ignore_ascii_case(ned))
}

/// The first tag `accept` approves, skipping the contents of raw-text elements
/// along the way.
fn scan_for<F>(html: &str, mut accept: F) -> Option<Tag>
where
    F: FnMut(&Tag) -> bool,
{
    let mut pos = 0usize;

    loop {
        let tag = next_tag(html, pos)?;
        pos = tag.end;

        if accept(&tag) {
            return Some(tag);
        }

        if let TagKind::Start {
            self_closing: false,
            ..
        } = tag.kind
        {
            if is_raw_text(&tag.name) {
                pos = skip_raw_text(html, &tag.name, pos);
            }
        }
    }
}

/// Extract the outlet's inner HTML from a full document string.
///
/// Returns `None` when the response has no outlet, which is the signal to fall
/// back to a full page load — the target page is not built for client routing,
/// and swapping a guess would produce a broken page.
///
/// The marker is only recognised as an attribute name on a real start tag:
/// mentions inside comments, `<script>`/`<style>` bodies, and quoted attribute
/// values are ignored, as are close tags appearing in those places while the
/// outlet's own extent is being measured.
pub fn extract_outlet_html(document_html: &str) -> Option<String> {
    let opening = scan_for(document_html, |tag| {
        matches!(tag.kind, TagKind::Start { outlet: true, .. })
    })?;

    if matches!(
        opening.kind,
        TagKind::Start {
            self_closing: true,
            ..
        }
    ) {
        // Self-closing outlet has nothing to swap in.
        return Some(String::new());
    }

    let content_start = opening.end;
    let mut depth = 1usize;
    let mut pos = content_start;

    // Nesting matters: `<main …><main>…</main></main>` must match the outer
    // close tag, not the first one encountered.
    loop {
        let tag = next_tag(document_html, pos)?;
        pos = tag.end;

        match tag.kind {
            TagKind::Start {
                self_closing: false,
                ..
            } => {
                if tag.name == opening.name {
                    depth += 1;
                } else if is_raw_text(&tag.name) {
                    pos = skip_raw_text(document_html, &tag.name, pos);
                }
            }
            TagKind::Start { .. } => {}
            TagKind::End if tag.name == opening.name => {
                depth -= 1;
                if depth == 0 {
                    return Some(document_html[content_start..tag.start].to_string());
                }
            }
            TagKind::End => {}
        }
    }
}

/// The document's `<title>` text, entity-decoded.
///
/// Attributes on the tag are tolerated, and a `<title>` mentioned inside a
/// comment or a script body is not mistaken for the real one.
pub fn extract_title(document_html: &str) -> Option<String> {
    let opening = scan_for(document_html, |tag| {
        tag.name == "title" && matches!(tag.kind, TagKind::Start { .. })
    })?;

    let end = find_ignore_ascii_case(document_html, "</title", opening.end)?;
    Some(decode_html_entities(document_html[opening.end..end].trim()))
}

/// Decode the HTML entities an SSR title can realistically contain.
///
/// Titles arrive escaped — `krab_core`'s renderer escapes `&`, `<`, and `>` —
/// so `document.title = raw` published `Krab &amp; friends` into the tab.
/// Numeric references are handled too because hand-written templates use them.
fn decode_html_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];

        // A reference is short; anything longer is a stray ampersand followed
        // by a semicolon somewhere else entirely.
        match after.find(';').filter(|&end| end <= 8) {
            Some(end) => match decode_entity(&after[..end]) {
                Some(decoded) => {
                    out.push(decoded);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('&');
                    rest = after;
                }
            },
            None => {
                out.push('&');
                rest = after;
            }
        }
    }

    out.push_str(rest);
    out
}

fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse::<u32>().ok()?,
            };
            char::from_u32(code)
        }
    }
}

// ── Browser wiring ──────────────────────────────────────────────────────────

#[cfg(all(feature = "web", target_arch = "wasm32"))]
mod browser {
    use super::*;
    use crate::hydrate;
    use std::cell::{Cell, RefCell};
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::spawn_local;
    use web_sys::{Document, Element, HtmlAnchorElement, HtmlElement, MouseEvent, Window};

    /// Keys the router writes its scroll position under in `history.state`.
    const SCROLL_X_KEY: &str = "krabScrollX";
    const SCROLL_Y_KEY: &str = "krabScrollY";

    thread_local! {
        /// Monotonic navigation counter.
        ///
        /// A navigation claims the next value and re-checks it after every
        /// `await`. Without this, two clicks in quick succession raced: whichever
        /// response arrived last won, so a slow request for a page the user had
        /// already navigated away from overwrote the newer content — and left the
        /// address bar pointing at the newer page while showing the older one.
        static NAVIGATION: Cell<u64> = const { Cell::new(0) };

        /// The URL whose content is currently in the outlet.
        ///
        /// Not the same thing as `location.href`, and the difference is
        /// load-bearing on `popstate`: the browser has *already* moved the
        /// address bar by the time the event fires, so asking "did only the
        /// fragment change?" by comparing `location.href` with itself answers
        /// yes whenever the URL has a fragment at all. Back and Forward were
        /// dead on any page served under a `#…` URL — no refetch, no swap, no
        /// symptom other than nothing happening.
        static RENDERED_URL: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    /// The URL the outlet's current content came from, falling back to the
    /// address bar before the first client navigation.
    fn rendered_url() -> Option<String> {
        RENDERED_URL
            .with(|url| url.borrow().clone())
            .or_else(current_href)
    }

    fn set_rendered_url(url: &str) {
        RENDERED_URL.with(|rendered| *rendered.borrow_mut() = Some(url.to_string()));
    }

    fn begin_navigation() -> u64 {
        NAVIGATION.with(|generation| {
            let next = generation.get().wrapping_add(1);
            generation.set(next);
            next
        })
    }

    /// Whether `generation` is still the newest navigation.
    fn is_current(generation: u64) -> bool {
        NAVIGATION.with(|current| current.get() == generation)
    }

    /// Install the click and popstate listeners.
    ///
    /// Idempotent: calling it twice does not double-handle clicks.
    ///
    /// Exported to JavaScript as `start_router`. The export is what keeps this
    /// module in the shipped bundle at all — see the module docs.
    #[wasm_bindgen(js_name = start_router)]
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

        // The router owns scroll from here on. Left on `auto`, the browser
        // restores a position against the *old* document height during a Back,
        // then the router restores again — two competing answers, and the
        // browser's is measured before the outlet has been swapped.
        //
        // `History::set_scroll_restoration` needs a `web-sys` feature this crate
        // does not enable; the property is a plain assignment, so set it as one.
        if let Ok(history) = window.history() {
            let _ = js_sys::Reflect::set(
                history.as_ref(),
                &JsValue::from_str("scrollRestoration"),
                &JsValue::from_str("manual"),
            );
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

    fn find_outlet(document: &Document) -> Option<Element> {
        document
            .query_selector(&format!("[{OUTLET_ATTR}]"))
            .ok()
            .flatten()
    }

    /// A fetched destination document and the URL it actually came from.
    struct FetchedDocument {
        html: String,
        /// `response.url`, which differs from the requested href after a server
        /// redirect. `None` when the browser reports none — a synthetic
        /// `Response` has an empty `url`, and pushing an empty string would
        /// rewrite the address bar to the current directory.
        final_url: Option<String>,
    }

    /// Fetch `href`, swap the outlet, re-hydrate.
    ///
    /// Any failure falls back to a full browser navigation — unless a newer
    /// navigation has since started, in which case this one simply gives up:
    /// hard-navigating to an abandoned destination would be worse than the
    /// stale-content bug it is guarding against.
    pub async fn navigate_to(href: &str, push: bool) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(document) = window.document() else {
            return;
        };

        // Compared against the URL the outlet's content came from, not against
        // the address bar — on `popstate` the address bar is already the
        // destination, and comparing it with itself declares every traversal a
        // fragment change.
        if let Some(rendered) = rendered_url() {
            if is_same_document_fragment(&rendered, href) {
                set_rendered_url(href);
                if push {
                    remember_scroll(&window);
                    push_history(&window, href, href);
                }
                return;
            }
        }

        if find_outlet(&document).is_none() {
            // No outlet in the *current* page: this document is not set up for
            // client routing.
            hard_navigate(href);
            return;
        }

        let generation = begin_navigation();

        let Some(fetched) = fetch_document(href).await else {
            if is_current(generation) {
                hard_navigate(href);
            }
            return;
        };

        if !is_current(generation) {
            return;
        }

        let Some(next_html) = extract_outlet_html(&fetched.html) else {
            // The destination has no outlet — it is not a client-routable page.
            hard_navigate(href);
            return;
        };

        // Re-queried rather than captured before the `await`: the document may
        // have been swapped by anything else in the meantime.
        let Some(outlet) = find_outlet(&document) else {
            hard_navigate(href);
            return;
        };

        // Before the swap, while the position still describes the page the user
        // is leaving. Reading it afterwards measures a document whose height has
        // already changed, and the browser may have clamped the offset.
        if push {
            remember_scroll(&window);
        }

        // `set_inner_html` alone drops the subtree without releasing anything it
        // holds: event closures stay in the crate's registry keyed by a
        // `__krab_id` on a node that no longer exists, and dynamic regions stay
        // registered and subscribed. Every navigation leaked a page's worth.
        crate::unmount(&outlet);
        outlet.set_inner_html(&next_html);
        set_rendered_url(fetched.final_url.as_deref().unwrap_or(href));

        let title = extract_title(&fetched.html);
        if let Some(title) = &title {
            document.set_title(title);
        }

        if push {
            let final_url = fetched.final_url.as_deref().unwrap_or(href);
            push_history(&window, final_url, href);
        }

        // New markup means new islands.
        hydrate();

        focus_outlet(&outlet);
        announce(&document, title.as_deref().unwrap_or(href));

        if push {
            window.scroll_to_with_x_and_y(0.0, 0.0);
        } else if let Some((x, y)) = saved_scroll(&window) {
            window.scroll_to_with_x_and_y(x, y);
        }
    }

    async fn fetch_document(href: &str) -> Option<FetchedDocument> {
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

        let final_url = response.url();
        let html = JsFuture::from(response.text().ok()?)
            .await
            .ok()?
            .as_string()?;

        Some(FetchedDocument {
            html,
            final_url: (!final_url.is_empty()).then_some(final_url),
        })
    }

    /// Push `url`, falling back to `requested` if the browser refuses it.
    ///
    /// `url` is `response.url` — the URL the content actually came from, which
    /// is what the address bar must show after a server redirect. Pushing the
    /// requested href instead left `/login` in the bar while `/dashboard` was on
    /// screen, and a reload then bounced the user back through the redirect.
    /// A cross-origin final URL makes `pushState` throw, hence the fallback.
    fn push_history(window: &Window, url: &str, requested: &str) {
        let Ok(history) = window.history() else {
            return;
        };

        if history
            .push_state_with_url(&JsValue::NULL, "", Some(url))
            .is_err()
        {
            let _ = history.push_state_with_url(&JsValue::NULL, "", Some(requested));
        }
    }

    /// Write the current scroll position into the current history entry.
    fn remember_scroll(window: &Window) {
        let Ok(history) = window.history() else {
            return;
        };

        let x = window.scroll_x().unwrap_or(0.0);
        let y = window.scroll_y().unwrap_or(0.0);

        let state = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            state.as_ref(),
            &JsValue::from_str(SCROLL_X_KEY),
            &JsValue::from_f64(x),
        );
        let _ = js_sys::Reflect::set(
            state.as_ref(),
            &JsValue::from_str(SCROLL_Y_KEY),
            &JsValue::from_f64(y),
        );

        let _ = history.replace_state(state.as_ref(), "");
    }

    /// The scroll position stored in the entry the browser has just restored.
    fn saved_scroll(window: &Window) -> Option<(f64, f64)> {
        let state = window.history().ok()?.state().ok()?;
        let x = js_sys::Reflect::get(&state, &JsValue::from_str(SCROLL_X_KEY))
            .ok()?
            .as_f64()?;
        let y = js_sys::Reflect::get(&state, &JsValue::from_str(SCROLL_Y_KEY))
            .ok()?
            .as_f64()?;
        Some((x, y))
    }

    /// Move focus into the freshly swapped content.
    ///
    /// Without this, focus stays on a link that the swap may have destroyed, and
    /// the next Tab starts from the top of the document. `tabindex="-1"` makes
    /// the outlet programmatically focusable without adding it to the tab order.
    fn focus_outlet(outlet: &Element) {
        if !outlet.has_attribute("tabindex") {
            let _ = outlet.set_attribute("tabindex", "-1");
        }
        if let Some(element) = outlet.dyn_ref::<HtmlElement>() {
            let _ = element.focus();
        }
    }

    /// Announce the new page in a polite live region.
    ///
    /// A screen reader is told nothing by an `innerHTML` swap: the document did
    /// not change, so no page-load announcement fires. The live region is the
    /// standard remedy, and it is visually hidden rather than `display:none`,
    /// which would take it out of the accessibility tree entirely.
    fn announce(document: &Document, message: &str) {
        let region = match document.get_element_by_id(ANNOUNCER_ID) {
            Some(region) => region,
            None => {
                let Ok(created) = document.create_element("div") else {
                    return;
                };
                created.set_id(ANNOUNCER_ID);
                let _ = created.set_attribute("role", "status");
                let _ = created.set_attribute("aria-live", "polite");
                let _ = created.set_attribute("aria-atomic", "true");
                let _ = created.set_attribute(
                    "style",
                    "position:absolute;width:1px;height:1px;margin:-1px;padding:0;\
                     overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap;border:0;",
                );

                let Some(body) = document.body() else {
                    return;
                };
                if body.append_child(created.as_ref()).is_err() {
                    return;
                }
                created
            }
        };

        region.set_text_content(Some(message));
    }

    fn hard_navigate(href: &str) {
        if let Some(window) = web_sys::window() {
            let _ = window.location().assign(href);
        }
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

    // ── Marker recognition: the scanner's reason for existing ───────────────

    /// A documentation page that *mentions* the marker must not be mistaken for
    /// one that *has* it. The substring search this replaced swapped the comment
    /// into the live document.
    #[test]
    fn the_marker_inside_a_comment_is_not_an_outlet() {
        let html = r#"<html><body>
            <!-- put <main data-krab-router-outlet> here -->
            <main data-krab-router-outlet><p>real</p></main>
        </body></html>"#;

        assert_eq!(extract_outlet_html(html).as_deref(), Some("<p>real</p>"));
    }

    #[test]
    fn the_marker_inside_a_script_is_not_an_outlet() {
        let html = concat!(
            r#"<html><body>"#,
            r#"<script>var sel = "[data-krab-router-outlet]";</script>"#,
            r#"<main data-krab-router-outlet><p>real</p></main>"#,
            r#"</body></html>"#
        );

        assert_eq!(extract_outlet_html(html).as_deref(), Some("<p>real</p>"));
    }

    /// The marker as another attribute's *value* is data, not a marker.
    #[test]
    fn the_marker_inside_an_attribute_value_is_not_an_outlet() {
        let html = concat!(
            r#"<div data-doc="data-krab-router-outlet">docs</div>"#,
            r#"<main data-krab-router-outlet><p>real</p></main>"#
        );

        assert_eq!(extract_outlet_html(html).as_deref(), Some("<p>real</p>"));
    }

    /// A close tag inside the outlet's own script body must not end the scan.
    #[test]
    fn a_close_tag_inside_a_script_does_not_truncate_the_outlet() {
        let html = concat!(
            r#"<main data-krab-router-outlet>"#,
            r#"<script>var s = "</main>";</script>"#,
            r#"<p>tail</p>"#,
            r#"</main>"#
        );

        let extracted = extract_outlet_html(html).expect("outlet not found");
        assert!(
            extracted.contains("<p>tail</p>"),
            "a </main> inside a script body truncated the outlet: {extracted}"
        );
    }

    #[test]
    fn a_close_tag_inside_a_comment_does_not_truncate_the_outlet() {
        let html = concat!(
            r#"<main data-krab-router-outlet>"#,
            r#"<!-- </main> -->"#,
            r#"<p>tail</p>"#,
            r#"</main>"#
        );

        let extracted = extract_outlet_html(html).expect("outlet not found");
        assert!(extracted.contains("<p>tail</p>"), "got: {extracted}");
    }

    /// A doctype and a `<` in prose must not derail the scan.
    #[test]
    fn a_doctype_and_stray_angle_brackets_are_tolerated() {
        let html = concat!(
            "<!doctype html><html><body><p>a < b</p>",
            r#"<main data-krab-router-outlet><p>real</p></main>"#,
            "</body></html>"
        );

        assert_eq!(extract_outlet_html(html).as_deref(), Some("<p>real</p>"));
    }

    #[test]
    fn the_marker_is_matched_case_insensitively_on_any_attribute_order() {
        let html = r#"<MAIN id="x" DATA-KRAB-ROUTER-OUTLET class="y"><p>hi</p></MAIN>"#;
        assert_eq!(extract_outlet_html(html).as_deref(), Some("<p>hi</p>"));
    }

    // ── Title extraction ───────────────────────────────────────────────────

    #[test]
    fn the_title_is_extracted_and_entity_decoded() {
        let html = r#"<html><head><title>Krab &amp; friends &#8212; docs</title></head></html>"#;
        assert_eq!(
            extract_title(html).as_deref(),
            Some("Krab & friends — docs")
        );
    }

    #[test]
    fn a_title_with_attributes_is_still_found() {
        let html = r#"<head><title data-x="1">Page</title></head>"#;
        assert_eq!(extract_title(html).as_deref(), Some("Page"));
    }

    #[test]
    fn a_title_mentioned_in_a_comment_is_not_the_documents_title() {
        let html = r#"<head><!-- <title>wrong</title> --><title>right</title></head>"#;
        assert_eq!(extract_title(html).as_deref(), Some("right"));
    }

    #[test]
    fn a_document_without_a_title_yields_none() {
        assert!(extract_title("<html><body>none</body></html>").is_none());
    }

    #[test]
    fn entity_decoding_leaves_a_bare_ampersand_alone() {
        assert_eq!(decode_html_entities("Tom & Jerry"), "Tom & Jerry");
        assert_eq!(
            decode_html_entities("a &notanentity; b"),
            "a &notanentity; b"
        );
        assert_eq!(decode_html_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_html_entities("&#x41;&#66;"), "AB");
    }
}
