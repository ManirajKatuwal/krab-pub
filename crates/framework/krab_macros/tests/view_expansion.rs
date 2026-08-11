/// Integration tests for the `view!` macro.
///
/// These tests verify the *runtime behaviour* of the expansion (the rendered
/// HTML output), which implicitly validates that the macro emitted correct code.
/// Compile-error cases are covered by the `trybuild` suite in `tests/compile_fail/`.
use krab_core::Render;
use krab_macros::view;

#[test]
fn self_closing_element_renders_correctly() {
    let html = view! { <br/> }.render();
    assert_eq!(html, "<br/>");
}

#[test]
fn element_with_no_children_renders_open_close_tags() {
    let html = view! { <div></div> }.render();
    assert_eq!(html, "<div></div>");
}

#[test]
fn element_with_text_child() {
    let html = view! { <p>"Hello"</p> }.render();
    assert_eq!(html, "<p>Hello</p>");
}

#[test]
fn element_with_string_attribute() {
    let html = view! { <div id="main"></div> }.render();
    assert_eq!(html, "<div id=\"main\"></div>");
}

#[test]
fn element_with_expression_attribute() {
    let cls = "active";
    let html = view! { <span class={cls}></span> }.render();
    assert_eq!(html, "<span class=\"active\"></span>");
}

#[test]
fn nested_elements_render_correctly() {
    let html = view! {
        <div>
            <h1>"Title"</h1>
            <p>"Body"</p>
        </div>
    }
    .render();
    assert_eq!(html, "<div><h1>Title</h1><p>Body</p></div>");
}

#[test]
fn expression_child_with_integer() {
    let count = 42i32;
    let html = view! { <span>{count}</span> }.render();
    assert_eq!(html, "<span>42</span>");
}

#[test]
fn expression_child_with_string_variable() {
    let name = "Krab";
    let html = view! { <b>{name}</b> }.render();
    assert_eq!(html, "<b>Krab</b>");
}

#[test]
fn fragment_renders_children_without_wrapper() {
    let html = view! {
        <>
            <span>"A"</span>
            <span>"B"</span>
        </>
    }
    .render();
    assert_eq!(html, "<span>A</span><span>B</span>");
}

#[test]
fn html_text_content_is_escaped() {
    let user_input = "<script>alert('xss')</script>";
    let html = view! { <p>{user_input}</p> }.render();
    assert!(!html.contains("<script>"));
    assert!(html.contains("&lt;script&gt;"));
}

#[test]
fn attribute_value_is_escaped() {
    let val = r#""><img src=x onerror=alert(1)>"#;
    let html = view! { <div title={val}></div> }.render();
    assert!(!html.contains("<img"));
    assert!(html.contains("&quot;"));
}

#[test]
fn void_elements_do_not_double_close() {
    for tag in &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ] {
        // We can't call view! with a variable tag name, so test via Node directly
        let el = krab_core::Node::Element(krab_core::Element {
            tag: tag.to_string(),
            attributes: vec![],
            children: vec![],
            events: vec![],
        });
        let rendered = el.render();
        assert!(
            rendered.ends_with("/>"),
            "void element <{tag}> should self-close, got: {rendered}"
        );
    }
}

// ── Hyphenated, namespaced, and keyword names ───────────────────────────────
//
// Tag and attribute names were parsed as `syn::Ident`, which cannot contain
// `-` or `:` and rejects Rust keywords. That made `data-*`, `aria-*`,
// `xlink:*`, custom elements, `<input type=...>`, and `<label for=...>` all
// unrepresentable — so the reference frontend hand-wrote HTML strings for the
// island markup the macro could not produce.

#[test]
fn data_attributes_render() {
    let html = view! { <div data-testid="row"></div> }.render();
    assert_eq!(html, "<div data-testid=\"row\"></div>");
}

#[test]
fn aria_attributes_render() {
    let html = view! { <button aria-label="Close" aria-hidden="false"></button> }.render();
    assert_eq!(
        html,
        "<button aria-label=\"Close\" aria-hidden=\"false\"></button>"
    );
}

#[test]
fn multi_segment_attribute_names_render() {
    let html = view! { <div data-krab-boundary-id="root-0"></div> }.render();
    assert_eq!(html, "<div data-krab-boundary-id=\"root-0\"></div>");
}

#[test]
fn namespaced_attributes_render() {
    // `<use>` is an SVG element, not one of HTML's void elements, so the
    // renderer emits a closing tag for it even when the source self-closes.
    // What matters here is the `xlink:href` name surviving the parser.
    let html = view! { <use xlink:href="#icon"/> }.render();
    assert_eq!(html, "<use xlink:href=\"#icon\"></use>");
}

#[test]
fn custom_element_tags_render_and_match_closing_tag() {
    let html = view! { <my-widget class="a">"hi"</my-widget> }.render();
    assert_eq!(html, "<my-widget class=\"a\">hi</my-widget>");
}

#[test]
fn rust_keywords_are_valid_attribute_names() {
    // `type`, `for`, and `as` are Rust keywords; `syn::Ident`'s default parser
    // rejects them, so `<input type="text">` did not compile.
    let html = view! { <input type="text"/> }.render();
    assert_eq!(html, "<input type=\"text\"/>");

    let html = view! { <label for="email">"Email"</label> }.render();
    assert_eq!(html, "<label for=\"email\">Email</label>");
}

#[test]
fn numeric_name_segments_render() {
    let html = view! { <div data-col-2="x"></div> }.render();
    assert_eq!(html, "<div data-col-2=\"x\"></div>");
}

#[test]
fn hyphenated_names_accept_expression_values() {
    let boundary = "island-7";
    let html = view! { <div data-krab-boundary-id={boundary}></div> }.render();
    assert_eq!(html, "<div data-krab-boundary-id=\"island-7\"></div>");
}

/// `#[island]` constructs the hydration wrapper by building
/// `krab_core::Attribute` values by hand because `view!` could not express any
/// of these names. It can now.
#[test]
fn view_can_emit_a_complete_island_wrapper() {
    let props = r#"{"count":0}"#;
    let html = view! {
        <div
            data-island="Counter"
            data-props={props}
            data-krab-boundary="Counter"
            data-krab-boundary-id="Counter-0"
            data-krab-boundary-state="ssr"
        >
            <span>"0"</span>
        </div>
    }
    .render();

    for marker in [
        "data-island=\"Counter\"",
        "data-krab-boundary=\"Counter\"",
        "data-krab-boundary-id=\"Counter-0\"",
        "data-krab-boundary-state=\"ssr\"",
    ] {
        assert!(html.contains(marker), "missing {marker} in {html}");
    }
    // Serialized props are HTML-escaped, as any attribute value is.
    assert!(html.contains("data-props="), "missing props in {html}");
}

// ── Control flow (ADR 0008) ─────────────────────────────────────────────────

#[test]
fn show_renders_children_when_the_condition_holds() {
    let node = view! {
        <Show when={|| true} fallback={|| view! { <p>"absent"</p> }}>
            <p>"present"</p>
        </Show>
    };

    let html = node.render();
    assert!(html.contains("present"), "got: {html}");
    assert!(!html.contains("absent"), "got: {html}");
}

#[test]
fn show_renders_the_fallback_otherwise() {
    let node = view! {
        <Show when={|| false} fallback={|| view! { <p>"absent"</p> }}>
            <p>"present"</p>
        </Show>
    };

    let html = node.render();
    assert!(html.contains("absent"), "got: {html}");
    assert!(!html.contains("present"), "got: {html}");
}

/// Omitting `fallback` renders nothing rather than requiring an empty closure.
#[test]
fn show_without_a_fallback_renders_nothing_when_false() {
    let node = view! {
        <Show when={|| false}>
            <p>"present"</p>
        </Show>
    };

    assert_eq!(node.render(), "");
}

#[test]
fn show_accepts_several_children() {
    let node = view! {
        <Show when={|| true}>
            <p>"one"</p>
            <p>"two"</p>
        </Show>
    };

    let html = node.render();
    assert!(html.contains("one") && html.contains("two"), "got: {html}");
}

#[test]
fn for_renders_one_row_per_item_and_stamps_the_key() {
    let node = view! {
        <ul>
            <For
                each={|| vec![10u32, 20, 30]}
                key={|item: &u32| *item}
                view={|item: u32| view! { <li>{item.to_string()}</li> }}
            />
        </ul>
    };

    let html = node.render();
    for value in [10, 20, 30] {
        assert!(html.contains(&value.to_string()), "missing {value}: {html}");
        assert!(
            html.contains(&format!(r#"data-krab-node-id="{value}""#)),
            "key {value} not stamped: {html}"
        );
    }
}

#[test]
fn for_over_an_empty_list_renders_nothing() {
    let node = view! {
        <For
            each={Vec::<u32>::new}
            key={|item: &u32| *item}
            view={|item: u32| view! { <li>{item.to_string()}</li> }}
        />
    };

    assert_eq!(node.render(), "");
}

/// Control flow composes with ordinary markup around it.
#[test]
fn control_flow_nests_inside_elements() {
    let node = view! {
        <div class="wrapper">
            <Show when={|| true}>
                <span>"inner"</span>
            </Show>
        </div>
    };

    let html = node.render();
    assert!(html.starts_with(r#"<div class="wrapper">"#), "got: {html}");
    assert!(html.contains("inner"), "got: {html}");
}
