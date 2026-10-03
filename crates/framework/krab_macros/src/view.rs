//! `view!`: the parser for the HTML-like syntax and the code it generates.
//!
//! The user-facing documentation lives on the entry point in `lib.rs`.

use proc_macro2::Span;
use quote::{format_ident, quote, quote_spanned, ToTokens};
use syn::{
    ext::IdentExt,
    parse::{Parse, ParseStream},
    parse_macro_input, token, Expr, Ident, LitInt, LitStr, Result, Token,
};

pub(crate) fn expand(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let node = parse_macro_input!(input as Node);
    proc_macro::TokenStream::from(quote! {
        #node
    })
}

enum Node {
    Element(Element),
    Text(LitStr),
    Expression(Expr),
    Fragment(Vec<Node>),
    /// `<Show>` / `<For>` — expands to a `krab_core::control_flow` call rather
    /// than to markup. See ADR 0008.
    ///
    /// Boxed: `ControlFlow` holds several `Expr`s and would otherwise make
    /// every `Node` — including a bare text node — as large as the biggest
    /// control-flow form. Trees are mostly ordinary markup, so the indirection
    /// is paid only where it is used.
    ControlFlow(Box<ControlFlow>),
    /// `<Card title="x"/>` — a call to the component function `Card` with a
    /// `CardProps` built from the attributes. See ADR 0013.
    ///
    /// Boxed for the same reason as `ControlFlow`.
    Component(Box<Component>),
}

/// A component invocation, after its attributes have been checked as props.
struct Component {
    /// The function to call, exactly as written in the tag (`Card`,
    /// `ui::Card`). Its tokens keep their source spans, so "cannot find
    /// function" lands on the tag.
    path: syn::Path,
    /// One entry per attribute, in source order.
    props: Vec<Prop>,
    /// Present when the tag ended its attributes with a bare `..`, which fills
    /// every prop not written out from the props type's `Default`.
    rest: Option<Span>,
    children: Vec<Node>,
    /// Where `children` should be reported if the props type has no such field.
    children_span: Span,
}

struct Prop {
    /// The struct field the attribute names, spanned on the attribute so an
    /// unknown field is reported where it was written.
    field: Ident,
    value: Expr,
    /// A string literal is converted with `.into()`; a braced expression is
    /// passed through untouched. See ADR 0013 for why the two differ.
    literal: bool,
}

enum ControlFlow {
    Show {
        when: Expr,
        fallback: Option<Expr>,
        children: Vec<Node>,
    },
    For {
        each: Expr,
        key: Expr,
        view: Expr,
    },
    /// `<Suspense fallback={...}>children</Suspense>` — ADR 0016.
    Suspense {
        fallback: Expr,
        children: Vec<Node>,
    },
}

/// Tags `view!` expands into control flow instead of markup.
///
/// A closed set, deliberately (ADR 0008). Every other capitalised tag is a
/// component call (ADR 0013), so these names are reserved: a user component
/// called `Show` has to be written with a path, `<ui::Show/>`. `Suspense`
/// joined the set in 0.6.0 (ADR 0016).
const CONTROL_FLOW_TAGS: &[&str] = &["Show", "For", "Suspense"];

/// A tag as parsed, before `Node::parse` decides whether it is an element,
/// control flow, or a component. Attribute and child parsing is identical for
/// all three; only the meaning differs.
struct Element {
    name: String,
    /// The span of the tag name, which diagnostics about the tag point at.
    name_span: Span,
    /// `Some` when the tag names a component: its last path segment is
    /// capitalised and it is not one of `CONTROL_FLOW_TAGS`.
    component: Option<syn::Path>,
    attributes: Vec<Attribute>,
    events: Vec<EventListener>,
    /// A bare `..` among the attributes. Meaningful only on a component.
    rest: Option<Span>,
    children: Vec<Node>,
}

struct Attribute {
    name: String,
    span: Span,
    value: Expr,
    /// Written as a string literal rather than `{expression}`.
    literal: bool,
}

struct EventListener {
    name: String,
    span: Span,
    value: Expr,
}

fn starts_uppercase(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
}

/// Parse a tag name: either an HTML name or a component path.
///
/// Grammar: `HtmlName | Ident ('::' Ident)*`, where the path form is taken
/// when the name contains `::` or starts with an uppercase letter.
///
/// No HTML or SVG element name begins with an uppercase ASCII letter (SVG's
/// camelCase names all start lowercase) and none contains `::`, so the split
/// cannot misread valid markup. Before ADR 0013 a capitalised tag was a compile
/// error for exactly that reason; the syntax was reserved, and is now spent.
///
/// Returns the name as written (`ui::Card`), the span of its first segment, and
/// the path when the tag is a component.
fn parse_tag_name(input: ParseStream) -> Result<(String, Span, Option<syn::Path>)> {
    let first = Ident::parse_any(input)?;
    let span = first.span();

    if !input.peek(Token![::]) && !starts_uppercase(&strip_raw(&first)) {
        let name = parse_html_name_rest(input, &first)?;
        return Ok((name, span, None));
    }

    let mut segments = vec![first];
    while input.peek(Token![::]) {
        input.parse::<Token![::]>()?;
        segments.push(Ident::parse_any(input)?);
    }
    let display = segments
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("::");

    // The rule is on the last segment because that is the item being named:
    // `ui::Card` is a component in module `ui`. A lowercase last segment
    // (`ui::card`) is neither an HTML name nor a component by this rule, and
    // guessing which the author meant would make the capitalisation rule
    // something to remember rather than something to read.
    let last = &segments[segments.len() - 1];
    if !starts_uppercase(&strip_raw(last)) {
        return Err(syn::Error::new(
            last.span(),
            format!(
                "<{display}> is a path, so `view!` reads it as a component, but `{last}` is not capitalised.\n  \
                 A component tag names its function, and component functions are capitalised\n  \
                 (the same convention as #[island]):\n    \
                 <ui::Card title=\"x\"/>  calls  ui::Card(ui::CardProps {{ .. }})\n  \
                 HTML element names never contain `::`."
            ),
        ));
    }

    if input.peek(Token![-]) || (input.peek(Token![:]) && !input.peek(Token![::])) {
        return Err(input.error(format!(
            "<{display}...> starts with a capital letter, so `view!` reads it as a component, and a\n  \
             component tag is a Rust path: `-` and `:` cannot appear in it.\n  \
             Custom elements are lowercase by the HTML spec: <my-widget>."
        )));
    }

    if segments.len() == 1 && CONTROL_FLOW_TAGS.contains(&display.as_str()) {
        return Ok((display, span, None));
    }

    let path = syn::Path {
        leading_colon: None,
        segments: segments.into_iter().map(syn::PathSegment::from).collect(),
    };
    Ok((display, span, Some(path)))
}

/// The struct field an attribute on a component sets: `aria-label` becomes
/// `aria_label`, and a keyword such as `type` becomes `r#type`.
fn prop_field(name: &str, span: Span) -> Result<Ident> {
    if name.contains(':') {
        return Err(syn::Error::new(
            span,
            format!(
                "`{name}` cannot be a prop: props are struct fields, and `:` has no field spelling.\n  \
                 Namespaced attributes and directives apply only to HTML elements. Pass the value\n  \
                 as a plain prop and put the attribute on an element inside the component."
            ),
        ));
    }
    let field = name.replace('-', "_");
    // `parse_str` rather than `Ident::new_raw`: the latter panics on names
    // that cannot be raw (`self`, `crate`), and a panic in a proc macro is a
    // far worse diagnostic than this one.
    let mut ident = syn::parse_str::<Ident>(&field)
        .or_else(|_| syn::parse_str::<Ident>(&format!("r#{field}")))
        .map_err(|_| {
            syn::Error::new(
                span,
                format!("`{name}` does not map to a Rust field name (tried `{field}`)."),
            )
        })?;
    // Spanned on the attribute so rustc's "struct has no field named ..."
    // points at what the author wrote rather than at the macro invocation.
    ident.set_span(span);
    Ok(ident)
}

/// Parse an HTML tag or attribute name into its source text.
///
/// Grammar: `Ident (('-' | ':') (Ident | LitInt))*`
///
/// Tag and attribute names were parsed as a bare [`syn::Ident`], which cannot
/// represent most real HTML. Two separate consequences:
///
/// - A Rust identifier contains no `-` or `:`, so `data-testid`, `aria-label`,
///   `xlink:href`, and every custom element (`<my-widget>`) were unparseable.
///   The framework's own `#[island]` macro builds `data-island` and
///   `data-krab-boundary-id` by constructing `krab_core::Attribute` values
///   directly, because `view!` could not express them.
/// - Rust keywords are not `Ident`s to `syn`'s default parser, so `type`,
///   `for`, `as`, and `loop` were rejected — meaning no `<input type="text">`
///   and no `<label for="name">`. [`IdentExt::parse_any`] accepts them.
///
/// Returns the reassembled name and the span of its first segment, which is
/// what diagnostics point at.
fn parse_html_name(input: ParseStream) -> Result<(String, Span)> {
    let first = Ident::parse_any(input)?;
    let span = first.span();
    Ok((parse_html_name_rest(input, &first)?, span))
}

/// The tail of [`parse_html_name`], for callers that have already consumed the
/// first segment to decide what kind of name they are looking at.
fn parse_html_name_rest(input: ParseStream, first: &Ident) -> Result<String> {
    let mut name = strip_raw(first);

    loop {
        // `::` is a path separator, never part of an HTML name. Leaving it to
        // the caller keeps `{some::path}` expressions parsing as before.
        let separator = if input.peek(Token![-]) {
            input.parse::<Token![-]>()?;
            '-'
        } else if input.peek(Token![:]) && !input.peek(Token![::]) {
            input.parse::<Token![:]>()?;
            ':'
        } else {
            break;
        };
        name.push(separator);

        if input.peek(Ident::peek_any) {
            name.push_str(&strip_raw(&Ident::parse_any(input)?));
        } else if input.peek(LitInt) {
            let segment: LitInt = input.parse()?;
            name.push_str(&segment.to_string());
        } else {
            return Err(input.error(format!(
                "expected a name segment after '{separator}' in '{name}'.\n  \
                 Names are made of segments joined by '-' or ':':\n    \
                 <div data-testid=\"x\">\n    \
                 <use xlink:href=\"#icon\"/>"
            )));
        }
    }

    Ok(name)
}

/// `r#type` reaches the parser with its raw prefix intact; HTML wants `type`.
fn strip_raw(ident: &Ident) -> String {
    let text = ident.to_string();
    text.strip_prefix("r#").unwrap_or(&text).to_string()
}

/// Parse child nodes up to the matching `</...>`.
///
/// The `is_empty` check is what makes an unclosed tag diagnosable. Without it
/// the loop hands an exhausted stream to [`Node::parse`], whose first act is to
/// reject an empty stream with "view! macro body is empty" — so
/// `view! { <div>"hi" }` reported that its body was empty, pointing at the
/// whole macro, rather than naming the tag that was never closed.
///
/// The loop condition is also the De Morgan dual of what it replaced
/// (`!peek(<) || !peek2(/)`), which is why the two copies of this loop each
/// carried a `break` on the negation of their own condition — unreachable in
/// both.
fn parse_children(
    input: ParseStream,
    open_tag: &str,
    close_tag: &str,
    open_span: Span,
) -> Result<Vec<Node>> {
    let mut children = Vec::new();
    while !(input.peek(Token![<]) && input.peek2(Token![/])) {
        if input.is_empty() {
            return Err(syn::Error::new(
                open_span,
                format!(
                    "unclosed `{open_tag}`: reached the end of the `view!` body without a matching `{close_tag}`.\n  \
                     Every element needs a closing tag, or `/>` if it has no children:\n    \
                     view! {{ {open_tag}\"text\"{close_tag} }}\n    \
                     view! {{ <img src=\"a.png\"/> }}"
                ),
            ));
        }
        children.push(input.parse()?);
    }
    Ok(children)
}

impl Attribute {
    /// The value as an expression that owns its data: a string literal becomes
    /// `"...".to_string()`, which is what control-flow attributes have always
    /// received.
    fn into_owned_string_value(self) -> Expr {
        let value = self.value;
        if self.literal {
            syn::parse_quote!(#value.to_string())
        } else {
            value
        }
    }
}

impl Element {
    /// Reinterpret a parsed component tag as a call to the component function.
    ///
    /// The checks here are the ones rustc cannot make well. An unknown prop or
    /// a missing one is left to rustc, whose struct-literal errors already name
    /// the field and — because every field ident carries its attribute's span —
    /// point at the right place.
    fn into_component(self, path: syn::Path) -> Result<Component> {
        let name = &self.name;

        if let Some(event) = self.events.first() {
            return Err(syn::Error::new(
                event.span,
                format!(
                    "`on:{}` attaches a DOM event listener, and <{name}> is a component, not an element:\n  \
                     it has no DOM node of its own for the listener to attach to.\n  \
                     Pass the handler as a prop and attach it to an element inside the component:\n    \
                     <{name} on_{}={{move |_| ...}}/>",
                    event.name,
                    event.name.replace(['-', ':'], "_"),
                ),
            ));
        }

        let mut props = Vec::with_capacity(self.attributes.len());
        for attribute in self.attributes {
            let field = prop_field(&attribute.name, attribute.span)?;
            if field == "children" && !self.children.is_empty() {
                return Err(syn::Error::new(
                    attribute.span,
                    format!(
                        "<{name}> sets `children` twice: once as an attribute and once as the content\n  \
                         between its tags. Keep one."
                    ),
                ));
            }
            props.push(Prop {
                field,
                value: attribute.value,
                literal: attribute.literal,
            });
        }

        Ok(Component {
            path,
            props,
            rest: self.rest,
            children: self.children,
            children_span: self.name_span,
        })
    }

    /// Reinterpret a parsed `<Show>` / `<For>` element as control flow.
    ///
    /// Diagnostics matter more than usual here: these tags look like markup, so
    /// a missing attribute has to say which one and why, not just fail to
    /// compile somewhere inside the expansion.
    fn into_control_flow(self) -> Result<ControlFlow> {
        let span = Span::call_site();

        if !self.events.is_empty() {
            return Err(syn::Error::new(
                span,
                format!(
                    "<{}> is control flow, not an element, so it has no event listeners.
                       Put the handler on an element inside it.",
                    self.name
                ),
            ));
        }

        if let Some(rest) = self.rest {
            return Err(syn::Error::new(
                rest,
                format!(
                    "`..` fills omitted props from `Default`, and <{}> is control flow, not a component.",
                    self.name
                ),
            ));
        }

        let take = |attributes: &mut Vec<Attribute>, wanted: &str| -> Option<Expr> {
            attributes
                .iter()
                .position(|attr| attr.name == wanted)
                .map(|index| attributes.remove(index).into_owned_string_value())
        };

        let mut attributes = self.attributes;

        match self.name.as_str() {
            "Show" => {
                let when = take(&mut attributes, "when").ok_or_else(|| {
                    syn::Error::new(
                        span,
                        "<Show> requires `when`, a closure returning bool.\n  \
                         Example:\n  \
                           <Show when={move || logged_in.get()}>...</Show>",
                    )
                })?;
                let fallback = take(&mut attributes, "fallback");
                reject_unknown(&attributes, "Show", &["when", "fallback"], span)?;

                Ok(ControlFlow::Show {
                    when,
                    fallback,
                    children: self.children,
                })
            }
            "For" => {
                if !self.children.is_empty() {
                    return Err(syn::Error::new(
                        span,
                        "<For> renders each row through `view`, so it takes no children.\n  \
                         Move the markup into the `view` closure and close the tag with `/>`.",
                    ));
                }

                let each = take(&mut attributes, "each").ok_or_else(|| {
                    syn::Error::new(
                        span,
                        "<For> requires `each`, a closure returning the items.\n  \
                         Example: each={move || todos.get()}",
                    )
                })?;

                // Deliberately not optional. Falling back to positional keys
                // would silently reintroduce the behaviour <For> exists to
                // prevent: inserting a row shifts every row's identity, losing
                // focus and selection on all of them. See ADR 0008.
                let key = take(&mut attributes, "key").ok_or_else(|| {
                    syn::Error::new(
                        span,
                        "<For> requires `key`, a closure returning a stable, unique id per item.\n  \
                         Without it rows match by position, so inserting one row renumbers every\n  \
                         row after it and they lose focus and DOM state.\n  \
                         Example: key={|todo: &Todo| todo.id}",
                    )
                })?;

                let view = take(&mut attributes, "view").ok_or_else(|| {
                    syn::Error::new(
                        span,
                        "<For> requires `view`, a closure rendering one item.\n  \
                         Example: view={|todo: Todo| view! { <li>{todo.title}</li> }}",
                    )
                })?;

                reject_unknown(&attributes, "For", &["each", "key", "view"], span)?;

                Ok(ControlFlow::For { each, key, view })
            }
            "Suspense" => {
                // Required rather than defaulting to nothing: a boundary with
                // no fallback renders an empty hole while it loads, which is
                // exactly what `<Suspense>` is for avoiding. Ask for it.
                let fallback = take(&mut attributes, "fallback").ok_or_else(|| {
                    syn::Error::new(
                        span,
                        "<Suspense> requires `fallback`, a closure rendering what to show while\n  \
                         the resources inside it load.\n  \
                         Example:\n  \
                           <Suspense fallback={|| view! { <p>\"Loading…\"</p> }}>...</Suspense>",
                    )
                })?;
                reject_unknown(&attributes, "Suspense", &["fallback"], span)?;

                Ok(ControlFlow::Suspense {
                    fallback,
                    children: self.children,
                })
            }
            other => Err(syn::Error::new(
                span,
                format!("unknown control-flow tag <{other}>"),
            )),
        }
    }
}

/// Reject attributes a control-flow tag does not understand.
///
/// Silently ignoring them would make a typo (`fallbck`) look like a working
/// fallback that never renders.
fn reject_unknown(attributes: &[Attribute], tag: &str, known: &[&str], span: Span) -> Result<()> {
    if let Some(unexpected) = attributes.first() {
        return Err(syn::Error::new(
            span,
            format!(
                "<{tag}> has no attribute `{}`. It accepts: {}.",
                unexpected.name,
                known.join(", ")
            ),
        ));
    }
    Ok(())
}

impl Parse for Node {
    fn parse(input: ParseStream) -> Result<Self> {
        if input.is_empty() {
            return Err(input.error(
                "view! macro body is empty. Provide at least one element, text, or expression:\n  \
                 view! { <div>\"Hello\"</div> }\n  \
                 view! { \"Static text\" }\n  \
                 view! { {my_variable} }",
            ));
        }

        if input.peek(Token![<]) {
            if input.peek2(Token![>]) {
                // Fragment <>...</>
                let open_span = input.span();
                input.parse::<Token![<]>()?;
                input.parse::<Token![>]>()?;
                let children = parse_children(input, "<>", "</>", open_span)?;
                input.parse::<Token![<]>()?;
                input.parse::<Token![/]>()?;
                input.parse::<Token![>]>()?;
                Ok(Node::Fragment(children))
            } else {
                let mut element: Element = input.parse()?;
                // Parsed as an ordinary element first, then reinterpreted:
                // attribute and child parsing is identical, and only the
                // meaning differs.
                if let Some(path) = element.component.take() {
                    return Ok(Node::Component(Box::new(element.into_component(path)?)));
                }
                if CONTROL_FLOW_TAGS.contains(&element.name.as_str()) {
                    return Ok(Node::ControlFlow(Box::new(element.into_control_flow()?)));
                }
                if let Some(rest) = element.rest {
                    return Err(syn::Error::new(
                        rest,
                        format!(
                            "`..` fills omitted props from `Default`, and <{}> is an HTML element,\n  \
                             not a component. Only a capitalised tag takes props.",
                            element.name
                        ),
                    ));
                }
                Ok(Node::Element(element))
            }
        } else if input.peek(token::Brace) {
            let content;
            syn::braced!(content in input);
            let expr: Expr = content.parse()?;
            Ok(Node::Expression(expr))
        } else {
            let text: LitStr = input.parse()?;
            Ok(Node::Text(text))
        }
    }
}

impl Parse for Element {
    fn parse(input: ParseStream) -> Result<Self> {
        input.parse::<Token![<]>()?;
        let (name, name_span, component) = parse_tag_name(input)?;

        let mut attributes = Vec::new();
        let mut events = Vec::new();
        let mut rest = None;
        loop {
            if input.peek(Token![>]) || input.peek(Token![/]) {
                break;
            }

            // `..` — the same spelling as Rust's struct update syntax, which is
            // what it expands to. Accepted here on every tag and rejected
            // later on the ones that are not components, so the diagnostic can
            // say what the tag is.
            if input.peek(Token![..]) {
                let dots: Token![..] = input.parse()?;
                let span = dots.spans[0];
                if rest.is_some() {
                    return Err(syn::Error::new(span, "`..` may appear only once in a tag"));
                }
                rest = Some(span);
                continue;
            }

            let (attr_name_str, attr_span) = parse_html_name(input)?;

            // `on:click` now parses as a single name, because ':' is a legal
            // separator. Event handlers are therefore recognised by prefix
            // after the fact, rather than by special-casing a bare `on`
            // followed by ':' during parsing — the old approach becomes
            // ambiguous once ':' can appear inside a name at all.
            if let Some(event_name) = attr_name_str.strip_prefix("on:") {
                if event_name.is_empty() {
                    return Err(syn::Error::new(
                        attr_span,
                        "event handler needs a name after 'on:', e.g. on:click={handler}",
                    ));
                }
                input.parse::<Token![=]>()?;

                let value: Expr = if input.peek(token::Brace) {
                    let content;
                    syn::braced!(content in input);
                    content.parse()?
                } else {
                    return Err(input.error(format!(
                        "Expected expression block for event handler '{attr_name_str}'.\n  \
                         Example: on:click={{move |_| count.set(count.get() + 1)}}"
                    )));
                };

                events.push(EventListener {
                    name: event_name.to_string(),
                    span: attr_span,
                    value,
                });
                continue;
            }

            input.parse::<Token![=]>()?;

            // The literal is kept as written rather than wrapped in
            // `.to_string()` here: an element attribute stringifies it at
            // codegen either way, and a component prop needs the literal
            // itself so that `.into()` can pick the field's type.
            let (value, literal): (Expr, bool) = if input.peek(LitStr) {
                let lit: LitStr = input.parse()?;
                (syn::parse_quote!(#lit), true)
            } else if input.peek(token::Brace) {
                let content;
                syn::braced!(content in input);
                (content.parse()?, false)
            } else {
                return Err(input.error(format!(
                    "Expected string literal or {{expression}} for attribute '{}' value.\n  \
                             Examples:\n    \
                             <div class=\"my-class\">  (string literal)\n    \
                             <div class={{my_var}}>  (expression block)",
                    attr_name_str
                )));
            };

            attributes.push(Attribute {
                name: attr_name_str,
                span: attr_span,
                value,
                literal,
            });
        }

        if input.peek(Token![/]) {
            input.parse::<Token![/]>()?;
            input.parse::<Token![>]>()?;
            return Ok(Element {
                name,
                name_span,
                component,
                attributes,
                events,
                rest,
                children: Vec::new(),
            });
        }

        input.parse::<Token![>]>()?;

        let children = parse_children(
            input,
            &format!("<{name}>"),
            &format!("</{name}>"),
            name_span,
        )?;

        input.parse::<Token![<]>()?;
        input.parse::<Token![/]>()?;
        // Parsed with the same grammar as the opening tag so the comparison is
        // on full names — `</my-widget>` must match `<my-widget>`, and the
        // diagnostic must print the hyphenated name rather than its first
        // segment. The same holds for component paths: `</ui::Card>` closes
        // `<ui::Card>`, and `</Card>` does not, even if both resolve to the
        // same function — the check is on spelling, which is all a macro sees.
        let (closing_name, closing_span, _) = parse_tag_name(input)?;

        if closing_name != name {
            return Err(syn::Error::new(
                closing_span,
                format!("Mismatched closing tag: expected </{name}>, found </{closing_name}>"),
            ));
        }

        input.parse::<Token![>]>()?;

        Ok(Element {
            name,
            name_span,
            component,
            attributes,
            events,
            rest,
            children,
        })
    }
}

impl ToTokens for Node {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        match self {
            Node::Component(component) => component.to_tokens(tokens),
            // `as_ref()` rather than a box pattern, which is still unstable.
            Node::ControlFlow(control) => match control.as_ref() {
                ControlFlow::Show {
                    when,
                    fallback,
                    children,
                } => {
                    // Children are wrapped in a fragment so `<Show>` can hold
                    // more than one node without the user adding a container
                    // element.
                    let shown = quote! {
                        krab_core::Node::Fragment(vec![#(#children),*])
                    };
                    let fallback = match fallback {
                        Some(expr) => quote! { #expr },
                        // Rendering nothing is the sane default for a
                        // conditional, and an empty fragment produces no markup.
                        None => quote! { || krab_core::Node::Fragment(Vec::new()) },
                    };

                    tokens.extend(quote! {
                        krab_core::control_flow::show(#when, move || #shown, #fallback)
                    });
                }
                ControlFlow::For { each, key, view } => {
                    tokens.extend(quote! {
                        krab_core::control_flow::for_each(#each, #key, #view)
                    });
                }
                ControlFlow::Suspense { fallback, children } => {
                    // The children are a closure, not a value: `suspense` runs
                    // it inside the scope that provides the boundary, which is
                    // how resources created while building them find it (ADR
                    // 0014 builds component children eagerly, *outside* the
                    // parent's scope — that would miss the boundary). Not
                    // `move`: it runs once, before `suspense` returns, so it
                    // may borrow like the surrounding code.
                    tokens.extend(quote! {
                        krab_core::suspense::suspense(
                            #fallback,
                            || krab_core::Node::Fragment(vec![#(#children),*]),
                        )
                    });
                }
            },
            Node::Element(el) => {
                let name = &el.name;
                let attrs = &el.attributes;
                let events = &el.events;
                let children = &el.children;
                tokens.extend(quote! {
                    krab_core::Node::Element(krab_core::Element {
                        tag: #name.to_string(),
                        attributes: vec![#(#attrs),*],
                        children: vec![#(#children),*],
                        events: vec![#(#events),*],
                    })
                });
            }
            Node::Text(text) => {
                tokens.extend(quote! {
                    krab_core::Node::Text(#text.to_string())
                });
            }
            Node::Expression(expr) => {
                // Expressions should evaluate to something that can be converted to a Node.
                // We use the `IntoNode` trait for this.
                tokens.extend(quote! {
                     krab_core::IntoNode::into_node(#expr)
                });
            }
            Node::Fragment(children) => {
                tokens.extend(quote! {
                    krab_core::Node::Fragment(vec![#(#children),*])
                });
            }
        }
    }
}

/// `<ui::Card title="x" count={n}>...</ui::Card>` expands to
///
/// ```text
/// {
///     let props = ui::CardProps {
///         title: Into::into("x"),
///         count: n,
///         children: <the content, as one krab_core::Node>,
///     };
///     krab_core::signal::with_owner(move || ui::Card(props))
/// }
/// ```
///
/// The props type is the tag's path with `Props` appended to its last
/// segment. That is a naming convention, not a lookup: a macro cannot see the
/// function's signature, so it has to be told the type's name, and the
/// convention is how.
impl ToTokens for Component {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        let path = &self.path;

        let mut props_path = self.path.clone();
        if let Some(last) = props_path.segments.last_mut() {
            last.ident = format_ident!("{}Props", last.ident, span = last.ident.span());
        }

        let fields = self.props.iter().map(|prop| {
            let field = &prop.field;
            let value = &prop.value;
            if prop.literal {
                // `Into` so that one string literal serves a `String`, a
                // `&'static str`, or a `Cow<'static, str>` field alike.
                quote_spanned! {field.span()=>
                    #field: ::core::convert::Into::into(#value)
                }
            } else {
                quote! { #field: #value }
            }
        });

        // One child is passed as itself; several are wrapped in a fragment so
        // the field is always a single `Node`, whatever the call site wrote.
        let children = match self.children.as_slice() {
            [] => None,
            [only] => Some(quote! { #only }),
            many => Some(quote! { krab_core::Node::Fragment(vec![#(#many),*]) }),
        }
        .map(|value| {
            let field = Ident::new("children", self.children_span);
            quote! { #field: #value, }
        });

        let rest = self.rest.map(|span| {
            quote_spanned! {span=> ..::core::default::Default::default() }
        });

        // Props are built outside the closure, so `?`, `return`, and moves in
        // attribute and child expressions mean what they mean in any other
        // expression. Only the call runs inside the component's own context
        // scope (ADR 0014): what it provides reaches what it renders, and not
        // its siblings. `mixed_site` keeps the binding out of the user's
        // namespace.
        let props = Ident::new("__krab_props", Span::mixed_site());
        tokens.extend(quote! {
            {
                let #props = #props_path {
                    #(#fields,)*
                    #children
                    #rest
                };
                krab_core::signal::with_owner(move || #path(#props))
            }
        });
    }
}

impl ToTokens for Attribute {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        let name = &self.name;
        let value = &self.value;
        // A closure literal is the reactive form (ADR 0015): it is kept as the
        // attribute's source and re-evaluated by an effect in the browser.
        // Anything else is stringified once, as before. The test is on the
        // syntax, not the type — a macro cannot see types — so a closure held
        // in a variable (`title={f}`) is still stringified and fails to
        // compile for want of `Display`; write `title={move || f()}`.
        if matches!(value, Expr::Closure(_)) {
            tokens.extend(quote_spanned! {self.span=>
                krab_core::Attribute::dynamic(#name, #value)
            });
            return;
        }
        tokens.extend(quote! {
            krab_core::Attribute::new(#name.to_string(), (#value).to_string())
        });
    }
}

impl ToTokens for EventListener {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        let name = &self.name;
        let value = &self.value;
        tokens.extend(quote! {
            #[cfg(feature = "web")]
            krab_core::EventListener {
                name: #name.to_string(),
                callback: std::rc::Rc::new(#value),
            }
        });
    }
}
