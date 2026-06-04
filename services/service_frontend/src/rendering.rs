use crate::frontend_env::{
    hydration_budget_for_route, hydration_preload_links_html, normalize_public_base_url,
    HydrationMode,
};
use krab_core::Render;
use krab_macros::view;
use serde_json::json;

#[derive(Clone)]
struct SeoMeta {
    title: String,
    description: String,
    path: String,
    og_type: &'static str,
}

pub(crate) fn canonical_url(base_url: &str, path: &str) -> String {
    let normalized_path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    format!("{}{}", base_url.trim_end_matches('/'), normalized_path)
}

fn render_seo_page(meta: SeoMeta, body_html: String) -> String {
    let hydration_mode = HydrationMode::from_env();
    let hydration_budget = hydration_budget_for_route(&meta.path, hydration_mode);
    let hydration_preloads = hydration_preload_links_html(&hydration_budget);

    let base_url = normalize_public_base_url();
    let canonical = canonical_url(&base_url, &meta.path);
    let structured_data = serde_json::to_string(&json!({
        "@context": "https://schema.org",
        "@type": "WebPage",
        "name": meta.title,
        "url": canonical,
        "description": meta.description
    }))
    .unwrap_or_else(|_| "{}".to_string());

    format!(
        "<!DOCTYPE html><html><head><title>{title}</title><meta charset=\"utf-8\" /><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" /><meta name=\"description\" content=\"{description}\" /><meta name=\"robots\" content=\"index,follow\" /><link rel=\"canonical\" href=\"{canonical}\" /><meta property=\"og:title\" content=\"{title}\" /><meta property=\"og:description\" content=\"{description}\" /><meta property=\"og:type\" content=\"{og_type}\" /><meta property=\"og:url\" content=\"{canonical}\" /><meta property=\"og:site_name\" content=\"Krab\" /><meta name=\"twitter:card\" content=\"summary_large_image\" /><meta name=\"twitter:title\" content=\"{title}\" /><meta name=\"twitter:description\" content=\"{description}\" />{hydration_preloads}<script type=\"application/ld+json\">{structured_data}</script></head><body>{body}</body></html>",
        title = meta.title,
        description = meta.description,
        canonical = canonical,
        og_type = meta.og_type,
        hydration_preloads = hydration_preloads,
        structured_data = structured_data,
        body = body_html,
    )
}

pub(crate) fn render_about_page() -> String {
    render_seo_page(
        SeoMeta {
            title: "About | Krab Framework".to_string(),
            description: "About Krab full-stack Rust framework".to_string(),
            path: "/about".to_string(),
            og_type: "website",
        },
        view! { <h1>"About Page"</h1> }.render(),
    )
}

pub(crate) fn render_greet_page() -> String {
    let name = "Visitor";
    render_seo_page(
        SeoMeta {
            title: "Greet | Krab Framework".to_string(),
            description: "Greeting page for Krab framework".to_string(),
            path: "/greet".to_string(),
            og_type: "website",
        },
        view! {
            <div>
                "Hello, " {name} "!"
                <p>"Welcome to the site."</p>
            </div>
        }
        .render(),
    )
}

pub(crate) fn render_blog_page(slug: &str) -> String {
    let path = format!("/blog/{slug}");
    render_seo_page(
        SeoMeta {
            title: format!("Blog Post: {slug} | Krab Framework"),
            description: format!("Blog content for {slug} in Krab framework"),
            path,
            og_type: "article",
        },
        view! {
            <div>
                <h1>"Blog Post: " {slug}</h1>
                <p>"Content for " {slug}</p>
            </div>
        }
        .render(),
    )
}
