use anyhow::{Context, Result};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::ServiceType;

pub(crate) fn dispatch_topology_action(action: &crate::TopologyAction) -> Result<()> {
    match action {
        crate::TopologyAction::Doctor { diagnostics } => run_topology_doctor(*diagnostics),
        crate::TopologyAction::Split {
            domain,
            protocols,
            register,
            dry_run,
        } => run_topology_split(domain, protocols, *register, *dry_run),
    }
}

pub(crate) fn protocol_label(service_type: &ServiceType) -> &'static str {
    match service_type {
        ServiceType::Rest => "rest",
        ServiceType::Graphql => "graphql",
        ServiceType::Rpc => "rpc",
        ServiceType::Grpc => "grpc",
    }
}

pub(crate) fn collect_rust_files_under(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(path).with_context(|| format!("Failed to read directory {path:?}"))? {
        let entry = entry?;
        let file_path = entry.path();
        if file_path.is_dir() {
            collect_rust_files_under(&file_path, out)?;
            continue;
        }

        if file_path.extension().and_then(|v| v.to_str()) == Some("rs") {
            out.push(file_path);
        }
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TopologyDoctorReport {
    pub(crate) checked_rust_files: usize,
    pub(crate) contract_path: PathBuf,
    pub(crate) violations: Vec<String>,
}

pub(crate) fn topology_doctor_report() -> Result<TopologyDoctorReport> {
    let mut violations: Vec<String> = Vec::new();
    let mut rust_files = Vec::new();
    collect_rust_files_under(Path::new("services"), &mut rust_files)?;

    for file in &rust_files {
        let owner = owning_service_name(file);
        let raw = fs::read_to_string(file)
            .with_context(|| format!("Failed reading Rust source {}", file.display()))?;

        for (line_idx, line) in raw.lines().enumerate() {
            if let Some(target) = parse_direct_service_import(line) {
                if owner
                    .as_ref()
                    .map(|service| service != &target)
                    .unwrap_or(true)
                {
                    violations.push(format!(
                        "{}:{} direct cross-service import `{}` bypasses contract boundary",
                        file.display(),
                        line_idx + 1,
                        target
                    ));
                }
            }
        }

        collect_service_endpoint_block_violations(file, &raw, &mut violations);
    }

    let contract_path = PathBuf::from("crates/framework/krab_core/src/service_contract.rs");
    let contract_raw = fs::read_to_string(&contract_path)
        .with_context(|| format!("Failed reading {}", contract_path.display()))?;
    for issue in detect_contract_payload_violations(&contract_raw) {
        violations.push(format!("{}: {issue}", contract_path.display()));
    }

    let service_config_path = PathBuf::from("krab.toml");
    if service_config_path.exists() {
        let service_config_raw = fs::read_to_string(&service_config_path)
            .with_context(|| format!("Failed reading {}", service_config_path.display()))?;
        for issue in detect_service_config_violations(&service_config_raw) {
            violations.push(format!("{}: {issue}", service_config_path.display()));
        }
    }

    Ok(TopologyDoctorReport {
        checked_rust_files: rust_files.len(),
        contract_path,
        violations,
    })
}

fn run_topology_doctor(diagnostics: bool) -> Result<()> {
    println!("🩺 Running topology doctor...");

    let mut violations: Vec<String> = Vec::new();
    let mut rust_files = Vec::new();
    collect_rust_files_under(Path::new("services"), &mut rust_files)?;

    for file in &rust_files {
        let owner = owning_service_name(file);
        let raw = fs::read_to_string(file)
            .with_context(|| format!("Failed reading Rust source {}", file.display()))?;

        for (line_idx, line) in raw.lines().enumerate() {
            if let Some(target) = parse_direct_service_import(line) {
                if owner
                    .as_ref()
                    .map(|service| service != &target)
                    .unwrap_or(true)
                {
                    violations.push(format!(
                        "{}:{} direct cross-service import `{}` bypasses contract boundary",
                        file.display(),
                        line_idx + 1,
                        target
                    ));
                }
            }
        }

        collect_service_endpoint_block_violations(file, &raw, &mut violations);
    }

    let contract_path = PathBuf::from("crates/framework/krab_core/src/service_contract.rs");
    let contract_raw = fs::read_to_string(&contract_path)
        .with_context(|| format!("Failed reading {}", contract_path.display()))?;
    for issue in detect_contract_payload_violations(&contract_raw) {
        violations.push(format!("{}: {issue}", contract_path.display()));
    }

    let service_config_path = PathBuf::from("krab.toml");
    if service_config_path.exists() {
        let service_config_raw = fs::read_to_string(&service_config_path)
            .with_context(|| format!("Failed reading {}", service_config_path.display()))?;
        for issue in detect_service_config_violations(&service_config_raw) {
            violations.push(format!("{}: {issue}", service_config_path.display()));
        }
    }

    if diagnostics {
        println!("   > checked Rust files: {}", rust_files.len());
        println!(
            "   > checked contract payload serialization derives in {}",
            contract_path.display()
        );
        println!(
            "   > checked orchestrator service health/restart policy in {}",
            service_config_path.display()
        );
    }

    if violations.is_empty() {
        println!("✅ topology doctor passed");
        return Ok(());
    }

    eprintln!("❌ topology doctor found {} issue(s):", violations.len());
    for issue in &violations {
        eprintln!(" - {issue}");
    }
    anyhow::bail!("topology doctor failed")
}

fn run_topology_split(
    domain: &str,
    protocols: &Option<Vec<ServiceType>>,
    register: bool,
    dry_run: bool,
) -> Result<()> {
    let slug = normalize_domain_slug(domain)?;
    let service_crate = format!("service_{}_split", slug);
    let service_key = format!("{}_split", slug);
    let crate_dir = PathBuf::from("services").join(&service_crate);

    if crate_dir.exists() {
        anyhow::bail!(
            "Split service scaffold already exists at {}",
            crate_dir.display()
        );
    }

    let selected_protocols = resolved_split_protocols(protocols);
    let mut adapter_modules = String::new();
    let mut adapter_files: Vec<(PathBuf, String)> = Vec::new();
    for protocol in &selected_protocols {
        let label = protocol_label(protocol);
        adapter_modules.push_str(&format!("pub mod {label};\n"));
        adapter_files.push((
            crate_dir.join(format!("src/adapters/{label}.rs")),
            format!(
                "use axum::{{routing::get, Json, Router}};\nuse serde_json::json;\n\npub fn mount_{label}_routes() -> Router {{\n    Router::new().route(\"/internal/{label}/capabilities\", get(capabilities))\n}}\n\nasync fn capabilities() -> Json<serde_json::Value> {{\n    Json(json!({{\n        \"adapter\": \"{label}\",\n        \"contract\": \"{slug}\",\n        \"mode\": \"local\",\n        \"remote_ready\": false\n    }}))\n}}\n"
            ),
        ));
    }

    let mut hasher = DefaultHasher::new();
    service_crate.hash(&mut hasher);
    let port = 3200 + (hasher.finish() % 300) as u16;

    let cargo_toml = format!(
        "[package]\nname = \"{service_crate}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nanyhow.workspace = true\naxum.workspace = true\ntokio.workspace = true\ntracing.workspace = true\ntracing-subscriber.workspace = true\nserde.workspace = true\nserde_json.workspace = true\n"
    );

    let main_rs = format!(
        "use anyhow::Result;\nuse axum::{{routing::get, Json, Router}};\nuse serde_json::json;\nuse std::net::SocketAddr;\n\nasync fn health() -> Json<serde_json::Value> {{\n    Json(json!({{\"status\": \"ok\", \"service\": \"{service_crate}\"}}))\n}}\n\nasync fn ready() -> Json<serde_json::Value> {{\n    Json(json!({{\"status\": \"ready\", \"service\": \"{service_crate}\"}}))\n}}\n\n#[tokio::main]\nasync fn main() -> Result<()> {{\n    tracing_subscriber::fmt::init();\n    let app = Router::new()\n        .route(\"/health\", get(health))\n        .route(\"/ready\", get(ready));\n\n    let addr = SocketAddr::from(([127, 0, 0, 1], {port}));\n    println!(\"{service_crate} listening on {{}}\", addr);\n\n    let listener = tokio::net::TcpListener::bind(addr).await?;\n    axum::serve(listener, app).await?;\n    Ok(())\n}}\n"
    );

    let readme = format!(
        "# {service_crate}\n\nGenerated by `krab topology split {slug}`.\n\n## Included scaffold\n- health/readiness endpoints\n- protocol adapter capability routes\n- domain skeleton\n- contract conformance placeholder tests\n\n## Next actions\n1. Move `{slug}` domain logic behind contract traits in `krab_core`.\n2. Implement local and remote adapters for each enabled protocol.\n3. Keep transport adapters thin and run the same contract tests against each adapter.\n4. Wire topology selection through runtime config and CI matrix.\n"
    );

    let test_rs = format!(
        "#[test]\nfn contract_conformance_placeholder_for_{slug}_split() {{\n    // TODO: replace with shared local-vs-remote contract conformance suite.\n    let required_contract_checks = [\"local_adapter\", \"remote_adapter\", \"payload_serialization\"];\n    assert!(required_contract_checks.contains(&\"payload_serialization\"));\n}}\n"
    );

    let mut planned_files: Vec<(PathBuf, String)> = vec![
        (crate_dir.join("Cargo.toml"), cargo_toml),
        (crate_dir.join("README.md"), readme),
        (crate_dir.join("src/main.rs"), main_rs),
        (
            crate_dir.join("src/domain/mod.rs"),
            "pub mod models;\npub mod service;\n".to_string(),
        ),
        (
            crate_dir.join("src/domain/models.rs"),
            "#[derive(Debug, Clone)]\npub struct DomainModel {\n    pub id: String,\n}\n"
                .to_string(),
        ),
        (
            crate_dir.join("src/domain/service.rs"),
            "pub trait DomainService: Send + Sync {}\n".to_string(),
        ),
        (crate_dir.join("src/adapters/mod.rs"), adapter_modules),
        (crate_dir.join("tests/contract_conformance.rs"), test_rs),
    ];
    planned_files.extend(adapter_files);

    if dry_run {
        println!("🧪 Dry-run split scaffold for domain '{slug}':");
        for (path, _) in &planned_files {
            println!("   > {}", path.display());
        }
        if register {
            println!(
                "   > would register workspace member `services/{service_crate}` and service key `{service_key}`"
            );
        }
        return Ok(());
    }

    for (path, _) in &planned_files {
        if path.exists() {
            anyhow::bail!("Refusing to overwrite existing file {}", path.display());
        }
    }

    for (path, content) in &planned_files {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        fs::write(path, content).with_context(|| format!("Failed writing {}", path.display()))?;
    }

    if register {
        register_workspace_member(&format!("services/{service_crate}"))?;
        register_krab_service(&service_key, &service_crate, port)?;
    }

    println!(
        "✅ Split topology scaffold generated at {}",
        crate_dir.display()
    );
    Ok(())
}

fn normalize_domain_slug(domain: &str) -> Result<String> {
    let slug = domain.trim().to_ascii_lowercase().replace('-', "_");
    if slug.is_empty() {
        anyhow::bail!("domain must not be empty");
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        anyhow::bail!("domain must contain only lowercase letters, digits, underscore or hyphen");
    }
    Ok(slug)
}

fn resolved_split_protocols(protocols: &Option<Vec<ServiceType>>) -> Vec<ServiceType> {
    let mut selected = protocols.clone().unwrap_or_else(|| vec![ServiceType::Rest]);
    if selected.is_empty() {
        selected.push(ServiceType::Rest);
    }
    let mut deduped = Vec::new();
    for protocol in selected {
        if !deduped.contains(&protocol) {
            deduped.push(protocol);
        }
    }
    deduped
}

fn register_workspace_member(member: &str) -> Result<()> {
    let workspace = PathBuf::from("Cargo.toml");
    let mut raw = fs::read_to_string(&workspace)
        .with_context(|| format!("Failed reading {}", workspace.display()))?;
    let quoted = format!("\"{}\"", member.replace('\\', "/"));
    if raw.contains(&quoted) {
        return Ok(());
    }

    let members_pos = raw
        .find("members = [")
        .context("workspace Cargo.toml missing members array")?;
    let list_start = raw[members_pos..]
        .find('[')
        .map(|idx| members_pos + idx)
        .context("workspace members array opening bracket not found")?;
    let list_end = raw[list_start..]
        .find(']')
        .map(|idx| list_start + idx)
        .context("workspace members array closing bracket not found")?;

    let insertion = format!("    {},\n", quoted);
    raw.insert_str(list_end, &insertion);
    fs::write(&workspace, raw)
        .with_context(|| format!("Failed writing {}", workspace.display()))?;
    println!("🧩 Registered workspace member: {}", member);
    Ok(())
}

fn register_krab_service(service_key: &str, service_crate: &str, port: u16) -> Result<()> {
    let config_path = PathBuf::from("krab.toml");
    let mut raw = fs::read_to_string(&config_path)
        .with_context(|| format!("Failed reading {}", config_path.display()))?;
    let header = format!("[services.{service_key}]");
    if raw.contains(&header) {
        return Ok(());
    }

    if !raw.ends_with('\n') {
        raw.push('\n');
    }

    raw.push_str(&format!(
        "\n{header}\ncommand = \"cargo\"\nargs = [\"run\", \"--bin\", \"{service_crate}\"]\nenv = {{ RUST_LOG = \"info\" }}\n\n[services.{service_key}.restart_policy]\non_exit = true\nbackoff_ms = 700\nmax_attempts = 8\n\n[services.{service_key}.healthcheck]\nurl = \"http://127.0.0.1:{port}/ready\"\ntimeout_ms = 1500\nretries = 12\ninterval_ms = 300\n"
    ));

    fs::write(&config_path, raw)
        .with_context(|| format!("Failed writing {}", config_path.display()))?;
    println!("🧩 Registered orchestrator entry: services.{service_key}");
    Ok(())
}

fn owning_service_name(path: &Path) -> Option<String> {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    for index in 0..parts.len().saturating_sub(1) {
        if parts[index] == "services" && parts[index + 1].starts_with("service_") {
            return Some(parts[index + 1].to_string());
        }
    }
    None
}

fn parse_direct_service_import(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    for prefix in ["use ", "pub use ", "extern crate "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let token = rest
                .split(|c: char| c == ':' || c == ';' || c.is_whitespace())
                .next()
                .unwrap_or_default();
            if token.starts_with("service_") {
                return Some(token.to_string());
            }
        }
    }
    None
}

fn collect_service_endpoint_block_violations(file: &Path, raw: &str, violations: &mut Vec<String>) {
    let lines: Vec<&str> = raw.lines().collect();
    let mut idx = 0usize;

    while idx < lines.len() {
        if !lines[idx].contains("ServiceEndpoint {") {
            idx += 1;
            continue;
        }

        let start_line = idx + 1;
        let mut block = String::new();
        let mut brace_depth = 0i32;

        while idx < lines.len() {
            let line = lines[idx];
            block.push_str(line);
            block.push('\n');

            brace_depth += line.chars().filter(|c| *c == '{').count() as i32;
            brace_depth -= line.chars().filter(|c| *c == '}').count() as i32;

            if brace_depth <= 0 {
                break;
            }

            idx += 1;
        }

        let uses_defaults = block.contains("..ServiceEndpoint::default()");
        let has_timeout = block.contains("timeout_ms");
        let has_retries = block.contains("max_retries");
        if !uses_defaults && (!has_timeout || !has_retries) {
            let mut missing = Vec::new();
            if !has_timeout {
                missing.push("timeout_ms");
            }
            if !has_retries {
                missing.push("max_retries");
            }
            violations.push(format!(
                "{}:{} ServiceEndpoint block missing {}",
                file.display(),
                start_line,
                missing.join(" and ")
            ));
        }

        idx += 1;
    }
}

fn detect_contract_payload_violations(raw: &str) -> Vec<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let mut violations = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("pub struct ") {
            continue;
        }

        let name = trimmed
            .trim_start_matches("pub struct ")
            .split(|c: char| c == '{' || c.is_whitespace())
            .next()
            .unwrap_or("UnknownContractStruct");

        let derive_window_start = idx.saturating_sub(3);
        let derive_window = &lines[derive_window_start..=idx];
        let has_serialize = derive_window
            .iter()
            .any(|entry| entry.contains("Serialize"));
        let has_deserialize = derive_window
            .iter()
            .any(|entry| entry.contains("Deserialize"));

        if !has_serialize || !has_deserialize {
            violations.push(format!(
                "line {} struct `{}` missing #[derive(Serialize, Deserialize)]",
                idx + 1,
                name
            ));
        }
    }

    violations
}

fn detect_service_config_violations(raw: &str) -> Vec<String> {
    let parsed: toml::Value = match toml::from_str(raw) {
        Ok(value) => value,
        Err(err) => return vec![format!("invalid krab.toml: {err}")],
    };

    let Some(services) = parsed.get("services").and_then(toml::Value::as_table) else {
        return Vec::new();
    };

    let mut violations = Vec::new();
    for (name, service) in services {
        let Some(service_table) = service.as_table() else {
            violations.push(format!("services.{name} must be a table"));
            continue;
        };

        let Some(healthcheck) = service_table
            .get("healthcheck")
            .and_then(toml::Value::as_table)
        else {
            violations.push(format!(
                "services.{name} missing [services.{name}.healthcheck]"
            ));
            continue;
        };

        match healthcheck.get("url").and_then(toml::Value::as_str) {
            Some(url) if url.ends_with("/ready") => {}
            Some(url) => violations.push(format!(
                "services.{name}.healthcheck.url should target /ready, got `{url}`"
            )),
            None => violations.push(format!("services.{name}.healthcheck.url missing")),
        }

        for field in ["timeout_ms", "retries", "interval_ms"] {
            if !healthcheck.contains_key(field) {
                violations.push(format!("services.{name}.healthcheck.{field} missing"));
            }
        }

        let Some(restart_policy) = service_table
            .get("restart_policy")
            .and_then(toml::Value::as_table)
        else {
            violations.push(format!(
                "services.{name} missing [services.{name}.restart_policy]"
            ));
            continue;
        };

        for field in ["on_exit", "backoff_ms", "max_attempts"] {
            if !restart_policy.contains_key(field) {
                violations.push(format!("services.{name}.restart_policy.{field} missing"));
            }
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::{detect_service_config_violations, parse_direct_service_import};

    #[test]
    fn topology_doctor_allows_ready_probe_and_restart_policy() {
        let violations = detect_service_config_violations(
            r#"
[services.frontend]
command = "cargo"
args = ["run", "--bin", "service_frontend"]

[services.frontend.restart_policy]
on_exit = true
backoff_ms = 700
max_attempts = 8

[services.frontend.healthcheck]
url = "http://127.0.0.1:3000/ready"
timeout_ms = 1500
retries = 12
interval_ms = 300
"#,
        );

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn topology_doctor_flags_liveness_probe_used_for_readiness() {
        let violations = detect_service_config_violations(
            r#"
[services.frontend]
command = "cargo"
args = ["run", "--bin", "service_frontend"]

[services.frontend.restart_policy]
on_exit = true
backoff_ms = 700
max_attempts = 8

[services.frontend.healthcheck]
url = "http://127.0.0.1:3000/health"
timeout_ms = 1500
retries = 12
interval_ms = 300
"#,
        );

        assert!(
            violations
                .iter()
                .any(|issue| issue.contains("should target /ready")),
            "{violations:?}"
        );
    }

    #[test]
    fn topology_doctor_flags_missing_restart_policy() {
        let violations = detect_service_config_violations(
            r#"
[services.frontend]
command = "cargo"
args = ["run", "--bin", "service_frontend"]

[services.frontend.healthcheck]
url = "http://127.0.0.1:3000/ready"
timeout_ms = 1500
retries = 12
interval_ms = 300
"#,
        );

        assert!(
            violations
                .iter()
                .any(|issue| issue.contains("missing [services.frontend.restart_policy]")),
            "{violations:?}"
        );
    }

    #[test]
    fn direct_service_import_parser_detects_service_crates() {
        assert_eq!(
            parse_direct_service_import("use service_users::client::UsersClient;"),
            Some("service_users".to_string())
        );
        assert_eq!(parse_direct_service_import("use crate::domain;"), None);
    }
}
