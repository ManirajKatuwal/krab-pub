use axum::response::Html;
use krab_core::Render;
use krab_macros::view;

pub async fn handler() -> Html<String> {
    let script = r#"
        async function submitContact(event) {
            event.preventDefault();
            const form = event.target;
            const result = document.getElementById('contact-result');
            const payload = {
                name: form.name.value,
                email: form.email.value,
                message: form.message.value,
            };

            try {
                const response = await fetch('/api/contact', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(payload),
                });
                const data = await response.json();
                if (response.ok) {
                    result.textContent = 'Message queued successfully.';
                    result.dataset.state = 'success';
                } else {
                    result.textContent = data.message || 'Submission failed.';
                    result.dataset.state = 'error';
                }
            } catch (err) {
                result.textContent = 'Submission failed due to network error.';
                result.dataset.state = 'error';
                console.error('contact submission failed', err);
            }
        }
    "#;

    Html(view! {
        <div>
            <h1>"Contact Us"</h1>
            <p>"Send us a message and we will follow up."</p>
            <form onsubmit="submitContact(event)">
                <label r#for="name">"Name"</label>
                <input id="name" name="name" required="true" />

                <label r#for="email">"Email"</label>
                <input id="email" name="email" r#type="email" required="true" />

                <label r#for="message">"Message"</label>
                <textarea id="message" name="message" required="true"></textarea>

                <button r#type="submit">"Send"</button>
            </form>
            <p id="contact-result"></p>
            <script>{script}</script>
        </div>
    }.render())
}
