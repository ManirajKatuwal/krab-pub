use axum::response::Html;
use krab_core::Render;
use krab_macros::view;

/// The contact page.
///
/// No inline script and no `onsubmit=` attribute: Krab's CSP
/// (`script-src 'self' 'wasm-unsafe-eval'`) blocks both, which left the form
/// unwired. The submit handler is `/_krab/contact.js`, which attaches itself
/// to `#contact-form` with `addEventListener`.
pub async fn handler() -> Html<String> {
    Html(view! {
        <div>
            <h1>"Contact Us"</h1>
            <p>"Send us a message and we will follow up."</p>
            <form id="contact-form">
                <label r#for="name">"Name"</label>
                <input id="name" name="name" required="true" />

                <label r#for="email">"Email"</label>
                <input id="email" name="email" r#type="email" required="true" />

                <label r#for="message">"Message"</label>
                <textarea id="message" name="message" required="true"></textarea>

                <button r#type="submit">"Send"</button>
            </form>
            <p id="contact-result"></p>
            <script r#type="module" src="/_krab/contact.js"></script>
        </div>
    }.render())
}
