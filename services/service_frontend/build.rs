use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::Path;

fn parse_route_middlewares(content: &str) -> Vec<String> {
    let mut middlewares = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        let Some(spec) = trimmed.strip_prefix("//# middleware:") else {
            continue;
        };

        for name in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            middlewares.push(name.to_string());
        }
    }

    middlewares
}

fn main() {
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let routes_dest_path = Path::new(&out_dir).join("routes.rs");
    let ssg_dest_path = Path::new(&out_dir).join("ssg.rs");

    let routes_dir = Path::new("src/routes");
    let api_dir = Path::new("src/api");
    println!("cargo:rerun-if-changed=src/routes");
    println!("cargo:rerun-if-changed=src/api");
    println!("cargo:rerun-if-env-changed=KRAB_SSG_BLOG_SLUGS");

    if !routes_dir.exists() {
        fs::create_dir_all(routes_dir).unwrap();
    }
    if !api_dir.exists() {
        fs::create_dir_all(api_dir).unwrap();
    }

    let mut modules = String::new();
    let mut discovered_routes: BTreeSet<String> = [
        "/".to_string(),
        "/about".to_string(),
        "/greet".to_string(),
        "/contact".to_string(),
    ]
    .into_iter()
    .collect();

    // Axum Router is consumed and returned (not mutated in place).
    let mut registration = String::from(
        "pub fn register_routes<S>(router: axum::Router<S>, state: S) -> axum::Router<S>\nwhere\n    S: Clone + Send + Sync + 'static,\n{\n    router\n",
    );

    if let Ok(entries) = fs::read_dir(routes_dir) {
        let mut route_entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        route_entries.sort_by_key(|e| e.path());

        for entry in route_entries {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                let stem = path.file_stem().unwrap().to_str().unwrap();
                if stem == "mod" {
                    continue;
                }

                let module_name = format!("route_{}", stem);
                let route_file = format!("/src/routes/{}.rs", stem);

                modules.push_str(&format!(
                    "mod {} {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"{}\")); }}\n",
                    module_name, route_file
                ));

                let route_path = if stem == "index" {
                    "/".to_string()
                } else {
                    format!("/{}", stem)
                };

                discovered_routes.insert(route_path.clone());

                let content = fs::read_to_string(&path).unwrap_or_default();
                let mut middlewares = String::new();
                for middleware in parse_route_middlewares(&content).into_iter().rev() {
                    middlewares.push_str(&format!(
                        ".layer(axum::middleware::from_fn_with_state(state.clone(), {}))",
                        middleware
                    ));
                }

                registration.push_str(&format!(
                    "        .route(\"{}\", axum::routing::get({}::handler){})\n",
                    route_path, module_name, middlewares
                ));
            }
        }
    }

    if let Ok(entries) = fs::read_dir(api_dir) {
        let mut api_entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        api_entries.sort_by_key(|e| e.path());

        for entry in api_entries {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                let stem = path.file_stem().unwrap().to_str().unwrap();
                if stem == "mod" {
                    continue;
                }

                let module_name = format!("api_{}", stem);
                let api_file = format!("/src/api/{}.rs", stem);

                modules.push_str(&format!(
                    "mod {} {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"{}\")); }}\n",
                    module_name, api_file
                ));

                let route_path = if stem == "index" {
                    "/api".to_string()
                } else {
                    format!("/api/{}", stem)
                };

                let content = fs::read_to_string(&path).unwrap_or_default();
                let mut method_routes = String::new();

                if content.contains("pub async fn get(") {
                    method_routes.push_str(&format!("axum::routing::get({}::get)", module_name));
                }
                if content.contains("pub async fn post(") {
                    if method_routes.is_empty() {
                        method_routes
                            .push_str(&format!("axum::routing::post({}::post)", module_name));
                    } else {
                        method_routes.push_str(&format!(".post({}::post)", module_name));
                    }
                }
                if content.contains("pub async fn put(") {
                    if method_routes.is_empty() {
                        method_routes
                            .push_str(&format!("axum::routing::put({}::put)", module_name));
                    } else {
                        method_routes.push_str(&format!(".put({}::put)", module_name));
                    }
                }
                if content.contains("pub async fn delete(") {
                    if method_routes.is_empty() {
                        method_routes
                            .push_str(&format!("axum::routing::delete({}::delete)", module_name));
                    } else {
                        method_routes.push_str(&format!(".delete({}::delete)", module_name));
                    }
                }
                if content.contains("pub async fn patch(") {
                    if method_routes.is_empty() {
                        method_routes
                            .push_str(&format!("axum::routing::patch({}::patch)", module_name));
                    } else {
                        method_routes.push_str(&format!(".patch({}::patch)", module_name));
                    }
                }

                let mut middlewares = String::new();
                for middleware in parse_route_middlewares(&content).into_iter().rev() {
                    middlewares.push_str(&format!(
                        ".layer(axum::middleware::from_fn_with_state(state.clone(), {}))",
                        middleware
                    ));
                }

                if !method_routes.is_empty() {
                    registration.push_str(&format!(
                        "        .route(\"{}\", {}{})\n",
                        route_path, method_routes, middlewares
                    ));
                }
            }
        }
    }

    registration.push_str("}\n");

    let dynamic_blog_slugs = discover_blog_slugs();
    let mut ssg_routes = discovered_routes.clone();
    for slug in &dynamic_blog_slugs {
        ssg_routes.insert(format!("/blog/{slug}"));
    }

    emit_static_html_artifacts(&ssg_routes);
    emit_asset_manifest();

    let routes_file = format!("{}\n{}", modules, registration);
    fs::write(routes_dest_path, routes_file).unwrap();

    let ssg_file = generate_ssg_module(
        discovered_routes.into_iter().collect(),
        dynamic_blog_slugs,
        ssg_routes.into_iter().collect(),
    );
    fs::write(ssg_dest_path, ssg_file).unwrap();
}

fn discover_blog_slugs() -> Vec<String> {
    let mut slugs = BTreeSet::new();

    if let Ok(env_slugs) = env::var("KRAB_SSG_BLOG_SLUGS") {
        for raw in env_slugs.split(',') {
            let slug = raw.trim();
            if !slug.is_empty() {
                slugs.insert(slug.to_string());
            }
        }
    }

    let hook_file = Path::new("public").join("ssg_blog_slugs.txt");
    println!("cargo:rerun-if-changed={}", hook_file.display());
    if let Ok(content) = fs::read_to_string(&hook_file) {
        for line in content.lines() {
            let slug = line.trim();
            if !slug.is_empty() && !slug.starts_with('#') {
                slugs.insert(slug.to_string());
            }
        }
    }

    if slugs.is_empty() {
        slugs.insert("welcome".to_string());
        slugs.insert("integration-check".to_string());
    }

    slugs.into_iter().collect()
}

fn emit_static_html_artifacts(routes: &BTreeSet<String>) {
    let out_dir = Path::new("public").join("__ssg");
    let _ = fs::create_dir_all(&out_dir);

    for route in routes {
        let file_name = route_to_file_name(route);
        let file_path = out_dir.join(file_name);
        let html = format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\" /><title>Krab SSG {}</title></head><body><h1>SSG Route: {}</h1></body></html>",
            html_escape(route),
            html_escape(route)
        );
        let _ = fs::write(file_path, html);
    }
}

fn emit_asset_manifest() {
    let out_dir = Path::new("public").join("__ssg");
    let _ = fs::create_dir_all(&out_dir);
    let manifest = "{\"assets\":{\"krab_client.js\":{\"path\":\"/pkg/krab_client.js\",\"integrity\":\"sha256-demo-manifest-checksum\",\"immutable\":true}}}";
    let _ = fs::write(out_dir.join("asset-manifest.json"), manifest);
}

fn route_to_file_name(route: &str) -> String {
    if route == "/" {
        return "index.html".to_string();
    }
    format!(
        "{}.html",
        route
            .trim_start_matches('/')
            .replace('/', "_")
            .replace(['{', '}'], "")
    )
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn generate_ssg_module(
    discovered_routes: Vec<String>,
    blog_slugs: Vec<String>,
    static_output_routes: Vec<String>,
) -> String {
    let discovered = vec_to_static_str_array(&discovered_routes);
    let rendered_routes = vec_to_static_str_array(&static_output_routes);
    let blog_slugs_literal = vec_to_owned_string_vec(&blog_slugs);

    let manifest_json = format!(
        "{{\"discovered_routes\":{},\"dynamic_params\":{{\"/blog/{{slug}}\":{}}},\"static_output_routes\":{},\"asset_manifest\":\"/__ssg/asset-manifest.json\"}}",
        json_string_array(&discovered_routes),
        json_string_array(&blog_slugs),
        json_string_array(&static_output_routes)
    );

    format!(
        "\
pub fn discovered_static_routes() -> &'static [&'static str] {{
    &[{}]
}}

pub fn enumerate_dynamic_route_params() -> std::collections::BTreeMap<&'static str, Vec<String>> {{
    let mut map = std::collections::BTreeMap::new();
    map.insert(\"/blog/{{slug}}\", vec![{}]);
    map
}}

pub fn ssg_output_routes() -> &'static [&'static str] {{
    &[{}]
}}

pub fn ssg_manifest_json() -> &'static str {{
    r#\"{}\"#
}}
",
        discovered, blog_slugs_literal, rendered_routes, manifest_json
    )
}

fn vec_to_static_str_array(items: &[String]) -> String {
    items
        .iter()
        .map(|r| format!("\"{}\"", escape_rust_string(r)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn vec_to_owned_string_vec(items: &[String]) -> String {
    items
        .iter()
        .map(|r| format!("\"{}\".to_string()", escape_rust_string(r)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn json_string_array(items: &[String]) -> String {
    let body = items
        .iter()
        .map(|v| format!("\"{}\"", escape_json(v)))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{}]", body)
}

fn escape_rust_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

fn escape_json(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}
