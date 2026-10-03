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

// ── Component composition (ADR 0013) ────────────────────────────────────────
//
// A capitalised tag calls the function of that name with a `{Name}Props`
// built from its attributes. These components follow the same convention as
// `#[island]`: one props argument, returning `krab_core::Node`.
#[allow(non_snake_case)]
mod components {
    use krab_core::Node;
    use krab_macros::view;

    pub struct CardProps {
        pub title: String,
        pub count: i32,
        pub children: Node,
    }

    pub fn Card(props: CardProps) -> Node {
        view! {
            <section class="card">
                <h2>{props.title}</h2>
                <span class="count">{props.count}</span>
                {props.children}
            </section>
        }
    }

    #[derive(Default)]
    pub struct ButtonProps {
        pub label: String,
        pub disabled: bool,
        pub aria_label: Option<String>,
        pub r#type: &'static str,
    }

    pub fn Button(props: ButtonProps) -> Node {
        let kind = if props.r#type.is_empty() {
            "button"
        } else {
            props.r#type
        };
        let aria = props.aria_label.unwrap_or_default();
        let disabled = if props.disabled { "true" } else { "false" };
        view! {
            <button type={kind} aria-label={aria} data-disabled={disabled}>{props.label}</button>
        }
    }

    pub mod ui {
        use krab_core::Node;
        use krab_macros::view;

        pub struct BadgeProps {
            pub label: &'static str,
        }

        pub fn Badge(props: BadgeProps) -> Node {
            view! { <em>{props.label}</em> }
        }

        #[derive(Default)]
        pub struct PanelProps {
            pub children: Node,
        }

        pub fn Panel(props: PanelProps) -> Node {
            view! { <div class="panel">{props.children}</div> }
        }
    }
}

use components::{ui, Button, ButtonProps, Card, CardProps};

#[test]
fn component_with_children_renders_through_its_function() {
    let count = 3;
    let html = view! {
        <Card title="Inbox" count={count}>
            <p>"You have mail"</p>
        </Card>
    }
    .render();

    assert_eq!(
        html,
        r#"<section class="card"><h2>Inbox</h2><span class="count">3</span><p>You have mail</p></section>"#
    );
}

#[test]
fn several_children_reach_the_component_as_one_fragment() {
    let html = view! {
        <Card title="List" count={2}>
            <p>"one"</p>
            "two"
        </Card>
    }
    .render();

    assert!(html.ends_with("<p>one</p>two</section>"), "got: {html}");
}

#[test]
fn path_tag_calls_the_function_at_that_path() {
    let html = view! { <div><ui::Badge label="new"/></div> }.render();
    assert_eq!(html, "<div><em>new</em></div>");
}

#[test]
fn path_tag_with_children_closes_with_the_same_path() {
    let html = view! {
        <ui::Panel>
            <ui::Badge label="inside"/>
        </ui::Panel>
    }
    .render();
    assert_eq!(html, r#"<div class="panel"><em>inside</em></div>"#);
}

#[test]
fn bare_rest_fills_omitted_props_from_default() {
    let html = view! { <Button label="Save" ../> }.render();
    assert_eq!(
        html,
        r#"<button type="button" aria-label="" data-disabled="false">Save</button>"#
    );
}

#[test]
fn rest_leaves_children_to_default_when_there_is_no_content() {
    let html = view! { <ui::Panel ../> }.render();
    assert_eq!(html, r#"<div class="panel"></div>"#);
}

#[test]
fn hyphenated_and_keyword_attributes_map_to_fields() {
    let html = view! {
        <Button label="Go" disabled={true} aria-label={Some("go".to_string())} type="submit"/>
    }
    .render();
    assert_eq!(
        html,
        r#"<button type="submit" aria-label="go" data-disabled="true">Go</button>"#
    );
}

#[test]
fn components_nest_and_can_be_the_root() {
    let html = view! {
        <Card title="Outer" count={1}>
            <Card title="Inner" count={2}>
                <ui::Badge label="deep"/>
            </Card>
        </Card>
    }
    .render();
    assert!(
        html.contains(
            r#"<h2>Inner</h2><span class="count">2</span><em>deep</em></section></section>"#
        ),
        "got: {html}"
    );
}

#[test]
fn component_tag_is_the_same_call_as_writing_it_out() {
    let by_tag = view! { <ui::Badge label="x"/> }.render();
    let by_call = ui::Badge(ui::BadgeProps { label: "x" }).render();
    assert_eq!(by_tag, by_call);

    let by_tag = view! { <Button label="y" ../> }.render();
    let by_call = Button(ButtonProps {
        label: "y".into(),
        ..Default::default()
    })
    .render();
    assert_eq!(by_tag, by_call);

    // `CardProps` is used by name only through the tag above; this keeps the
    // import honest.
    let _ = |props: CardProps| Card(props);
}

#[test]
fn components_compose_with_control_flow() {
    let html = view! {
        <Show when={|| true}>
            <ui::Badge label="shown"/>
        </Show>
    }
    .render();
    assert!(html.contains("<em>shown</em>"), "got: {html}");
}

// ── Context through component tags (ADR 0014) ───────────────────────────────
//
// Each tag runs its component in a context scope of its own, so a provide in
// one component reaches the components it renders and not its siblings.
#[allow(non_snake_case)]
mod context_components {
    use krab_core::signal::{provide_context, use_context};
    use krab_core::Node;
    use krab_macros::view;

    #[derive(Clone)]
    struct Theme(&'static str);

    pub struct ThemedProps {
        pub theme: &'static str,
    }

    pub fn Themed(props: ThemedProps) -> Node {
        provide_context(Theme(props.theme));
        view! { <div><Label/></div> }
    }

    pub struct LabelProps {}

    pub fn Label(_props: LabelProps) -> Node {
        let theme = use_context::<Theme>()
            .map(|theme| theme.0)
            .unwrap_or("none");
        view! { <span>{theme}</span> }
    }
}

// A tag needs both names in scope: the function and its props type.
use context_components::{Themed, ThemedProps};

#[test]
fn a_context_provided_by_a_component_reaches_what_it_renders_and_not_its_siblings() {
    let html = krab_core::signal::with_owner(|| {
        view! {
            <section>
                <Themed theme="dark"/>
                <context_components::Label/>
                <Themed theme="light"/>
            </section>
        }
        .render()
    });
    assert_eq!(
        html,
        "<section><div><span>dark</span></div><span>none</span><div><span>light</span></div></section>"
    );
}

// ── Suspense (ADR 0016) ─────────────────────────────────────────────────────

#[test]
fn suspense_renders_the_fallback_while_a_resource_is_pending() {
    let html = krab_core::signal::with_owner(|| {
        view! {
            <section>
                <Suspense fallback={|| view! { <p>"Loading"</p> }}>
                    {{
                        let user = krab_core::resource::create_resource(
                            || 1u32,
                            |n| async move { Ok::<_, String>(n) },
                        );
                        move || view! { <p>{format!("{:?}", user.value().get())}</p> }
                    }}
                </Suspense>
            </section>
        }
        .render()
    });
    assert!(html.contains("<p>Loading</p>"), "{html}");
    assert!(html.contains(":pending-->"), "{html}");
    assert!(html.contains(":resolved-->"), "{html}");
    assert!(krab_core::render_stream::is_finalized_ssr_snapshot(&html));
}

#[test]
fn suspense_renders_the_children_when_data_is_there() {
    let html = krab_core::signal::with_owner(|| {
        view! {
            <Suspense fallback={|| view! { <p>"Loading"</p> }}>
                <h2>"Profile"</h2>
                {{
                    let user = krab_core::resource::create_resource_with_initial(
                        Some("ada".to_string()),
                        || 1u32,
                        |_| async move { Ok::<_, String>(String::new()) },
                    );
                    move || view! { <p>{user.value().get().unwrap_or_default()}</p> }
                }}
            </Suspense>
        }
        .render()
    });
    assert!(html.contains("<h2>Profile</h2><p>ada</p>"), "{html}");
    assert!(!html.contains("Loading"), "{html}");
}

// ── Reactive attributes (ADR 0015) ──────────────────────────────────────────

#[test]
fn a_closure_attribute_is_dynamic_and_renders_its_current_value() {
    let (label, set_label) = krab_core::signal::create_signal("one".to_string());
    let node = view! { <div title={move || label.get()}></div> };

    let krab_core::Node::Element(element) = &node else {
        panic!("expected an element");
    };
    assert!(element.attributes[0].is_dynamic());

    assert_eq!(node.render(), "<div title=\"one\"></div>");
    set_label.set("two".to_string());
    assert_eq!(node.render(), "<div title=\"two\"></div>");
}

#[test]
fn a_boolean_closure_attribute_is_present_or_omitted() {
    let (busy, set_busy) = krab_core::signal::create_signal(false);
    let node = view! { <button disabled={move || busy.get()}>"Go"</button> };

    assert_eq!(node.render(), "<button>Go</button>");
    set_busy.set(true);
    assert_eq!(node.render(), "<button disabled=\"\">Go</button>");
}

#[test]
fn an_option_closure_attribute_is_omitted_on_none() {
    let (href, set_href) = krab_core::signal::create_signal(None::<String>);
    let node = view! { <a href={move || href.get()}>"link"</a> };

    assert_eq!(node.render(), "<a>link</a>");
    set_href.set(Some("/x?a=1&b=2".to_string()));
    assert_eq!(node.render(), "<a href=\"/x?a=1&amp;b=2\">link</a>");
}

#[test]
fn a_non_closure_expression_attribute_stays_static() {
    let count = 3;
    let node = view! { <span data-count={count}></span> };
    let krab_core::Node::Element(element) = &node else {
        panic!("expected an element");
    };
    assert!(!element.attributes[0].is_dynamic());
    assert_eq!(node.render(), "<span data-count=\"3\"></span>");
}
