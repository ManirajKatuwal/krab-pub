use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::topology::protocol_label;
use crate::{ExposureMode, GenResource, ServiceType, Topology};

pub(crate) fn dispatch_gen_resource(resource: &GenResource) -> Result<()> {
    match resource {
        GenResource::Service {
            name,
            r#type,
            exposure_mode,
            protocols,
            topology,
        } => generate_service(name, r#type, exposure_mode, protocols, topology),
        GenResource::Component { name } => generate_component(name),
        GenResource::Route { name } => generate_route(name),
        GenResource::ServerFunction { name } => generate_server_function(name),
    }
}

fn generate_service(
    name: &str,
    service_type: &ServiceType,
    exposure_mode: &ExposureMode,
    protocols: &Option<Vec<ServiceType>>,
    topology: &Topology,
) -> Result<()> {
    println!(
        "🦀 Generating service '{}' of type {:?} (mode={:?}, topology={:?})...",
        name, service_type, exposure_mode, topology
    );

    let selected_protocols = resolve_protocols(service_type, exposure_mode, protocols)?;

    if *topology == Topology::SplitServices {
        return generate_split_service_topology(name, &selected_protocols);
    }

    let path = PathBuf::from(name);
    if path.exists() {
        anyhow::bail!("Directory '{}' already exists", name);
    }

    fs::create_dir(&path).context("Failed to create service directory")?;

    let mut feature_names: Vec<&str> = Vec::new();
    for proto in &selected_protocols {
        let feature = match proto {
            ServiceType::Rest => Some("rest"),
            ServiceType::Graphql => Some("graphql"),
            ServiceType::Rpc => Some("rest"),
            ServiceType::Grpc => Some("grpc"),
        };
        if let Some(feature) = feature {
            if !feature_names.contains(&feature) {
                feature_names.push(feature);
            }
        }
    }
    if feature_names.is_empty() {
        feature_names.push("rest");
    }

    let cargo_toml = format!(
        r#"[package]
name = "{}"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = {{ version = "1.0", features = ["full"] }}
krab_core = {{ path = "../krab_core", features = [{}] }}
anyhow = "1.0"
tracing = "0.1"
tracing-subscriber = "0.3"
serde = {{ version = "1.0", features = ["derive"] }}
"#,
        name,
        feature_names
            .iter()
            .map(|f| format!("\"{}\"", f))
            .collect::<Vec<String>>()
            .join(", ")
    );

    fs::write(path.join("Cargo.toml"), cargo_toml)?;
    fs::create_dir(path.join("src"))?;

    let mut main_rs = r#"use anyhow::Result;
use krab_core::service::{ApiService, ServiceConfig};
use async_trait::async_trait;

struct Service;

#[async_trait]
impl ApiService for Service {
    async fn start(&self) -> Result<()> {
        println!("Service started!");
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    println!("exposure_mode=__EXPOSURE_MODE__");
    println!("protocols=__PROTOCOLS__");
    let service = Service;
    service.start().await
}
"#
    .to_string();
    let exposure_mode_value = match exposure_mode {
        ExposureMode::Single => "single",
        ExposureMode::Multi => "multi",
    };
    let protocols_value = selected_protocols
        .iter()
        .map(protocol_label)
        .collect::<Vec<&str>>()
        .join(",");
    main_rs = main_rs
        .replace("__EXPOSURE_MODE__", exposure_mode_value)
        .replace("__PROTOCOLS__", &protocols_value);
    fs::write(path.join("src/main.rs"), main_rs)?;

    if *exposure_mode == ExposureMode::Multi {
        generate_multi_mode_layout(&path, &selected_protocols)?;
    }

    println!("✅ Service '{}' created successfully!", name);
    println!(
        "👉 Add '{}' to your workspace Cargo.toml members list.",
        name
    );

    Ok(())
}

fn resolve_protocols(
    service_type: &ServiceType,
    exposure_mode: &ExposureMode,
    protocols: &Option<Vec<ServiceType>>,
) -> Result<Vec<ServiceType>> {
    let mut selected = if *exposure_mode == ExposureMode::Single {
        vec![service_type.clone()]
    } else {
        protocols
            .clone()
            .unwrap_or_else(|| vec![service_type.clone()])
    };

    if selected.is_empty() {
        selected.push(service_type.clone());
    }

    let mut deduped = Vec::new();
    for p in selected {
        if !deduped.contains(&p) {
            deduped.push(p);
        }
    }
    Ok(deduped)
}

fn generate_multi_mode_layout(path: &Path, selected_protocols: &[ServiceType]) -> Result<()> {
    let domain_dir = path.join("src/domain");
    let adapters_dir = path.join("src/adapters");
    fs::create_dir_all(&domain_dir)?;
    fs::create_dir_all(&adapters_dir)?;

    fs::write(
        path.join("src/capabilities.rs"),
        "pub fn build_capabilities() {}\n",
    )?;
    fs::write(
        path.join("src/domain/mod.rs"),
        "pub mod models;\npub mod service;\n",
    )?;
    fs::write(
        path.join("src/domain/models.rs"),
        "#[derive(Debug, Clone)]\npub struct DomainModel;\n",
    )?;
    fs::write(
        path.join("src/domain/service.rs"),
        "pub trait DomainService: Send + Sync {}\n",
    )?;

    let mut mod_rs = String::new();
    for protocol in selected_protocols {
        let label = protocol_label(protocol);
        let module_name = label.replace('-', "_");
        mod_rs.push_str(&format!("pub mod {};\n", module_name));
        fs::write(
            adapters_dir.join(format!("{}.rs", module_name)),
            format!("pub fn mount_{}() {{}}\n", module_name),
        )?;
    }
    if mod_rs.is_empty() {
        mod_rs.push_str("pub mod rest;\n");
        fs::write(adapters_dir.join("rest.rs"), "pub fn mount_rest() {}\n")?;
    }
    fs::write(adapters_dir.join("mod.rs"), mod_rs)?;
    Ok(())
}

fn generate_split_service_topology(name: &str, selected_protocols: &[ServiceType]) -> Result<()> {
    let domain_name = format!("{}-domain", name);
    let domain_path = PathBuf::from(&domain_name);
    if domain_path.exists() {
        anyhow::bail!("Directory '{}' already exists", domain_name);
    }

    fs::create_dir(&domain_path)?;
    fs::create_dir(domain_path.join("src"))?;
    fs::write(
        domain_path.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            domain_name
        ),
    )?;
    fs::write(
        domain_path.join("src/lib.rs"),
        "pub fn shared_domain_marker() -> &'static str { \"shared\" }\n",
    )?;

    let mut created = Vec::new();
    for protocol in selected_protocols {
        let label = protocol_label(protocol);
        let crate_name = format!("{}-{}", name, label);
        let crate_path = PathBuf::from(&crate_name);
        if crate_path.exists() {
            anyhow::bail!("Directory '{}' already exists", crate_name);
        }
        fs::create_dir(&crate_path)?;
        fs::create_dir_all(crate_path.join(format!("src/adapters/{}", label)))?;
        fs::create_dir_all(crate_path.join("src/domain"))?;
        fs::write(
            crate_path.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{} = {{ path = \"../{}\" }}\nkrab_core = {{ path = \"../krab_core\", features = [\"rest\"] }}\n",
                crate_name, domain_name, domain_name
            ),
        )?;
        fs::write(
            crate_path.join("src/main.rs"),
            format!("fn main() {{ println!(\"{} adapter service\"); }}\n", label),
        )?;
        fs::write(crate_path.join("src/domain/mod.rs"), "pub use crate::*;\n")?;
        fs::write(
            crate_path.join(format!("src/adapters/{}/mod.rs", label)),
            format!("pub fn mount_{}() {{}}\n", label.replace('-', "_")),
        )?;
        fs::write(
            crate_path.join("src/adapters/mod.rs"),
            format!("pub mod {};\n", label.replace('-', "_")),
        )?;
        created.push(crate_name);
    }

    println!(
        "✅ Split topology generated with shared domain crate: {}",
        domain_name
    );
    println!("👉 Generated protocol crates: {}", created.join(", "));
    Ok(())
}

fn generate_component(name: &str) -> Result<()> {
    println!("🦀 Generating component '{}'...", name);
    let path = PathBuf::from(format!("src/components/{}.rs", name.to_lowercase()));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let content = format!(
        r#"use krab_core::prelude::*;

#[component]
pub fn {}() -> impl IntoView {{
    view! {{
        <div class="{}">
            "We are crabs"
        </div>
    }}
}}
"#,
        name,
        name.to_lowercase()
    );
    fs::write(&path, content)?;
    println!("✅ Component '{}' created at {:?}", name, path);
    Ok(())
}

fn generate_route(name: &str) -> Result<()> {
    println!("🦀 Generating route '{}'...", name);
    let path = PathBuf::from(format!("src/routes/{}.rs", name.to_lowercase()));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let content = format!(
        r#"use krab_core::prelude::*;

#[route(path = "/{}")]
pub fn {}() -> impl IntoView {{
    view! {{
        <div>
            "Route: {}"
        </div>
    }}
}}
"#,
        name.to_lowercase(),
        name,
        name
    );
    fs::write(&path, content)?;
    println!("✅ Route '{}' created at {:?}", name, path);
    Ok(())
}

fn generate_server_function(name: &str) -> Result<()> {
    println!("🦀 Generating server function '{}'...", name);
    let path = PathBuf::from(format!("src/server_functions/{}.rs", name.to_lowercase()));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let content = render_server_function(name);
    fs::write(&path, content)?;
    println!("Server function '{}' created at {:?}", name, path);
    Ok(())
}

fn render_server_function(name: &str) -> String {
    format!(
        r#"use krab_core::server_fn::{{validate_server_fn, ServerFnError}};
use krab_macros::server;

#[server]
pub async fn {}(input: String) -> Result<String, ServerFnError> {{
    validate_server_fn(!input.trim().is_empty(), "input is required")?;
    Ok(format!("Hello from server: {{}}", input))
}}
"#,
        name
    )
}

#[cfg(test)]
mod tests {
    use super::render_server_function;

    #[test]
    fn server_function_generator_uses_supported_macro_contract() {
        let rendered = render_server_function("load_user");

        assert!(rendered.contains("#[server]"));
        assert!(!rendered.contains("endpoint ="));
        assert!(rendered.contains("validate_server_fn"));
        assert!(rendered.contains("Result<String, ServerFnError>"));
    }
}
