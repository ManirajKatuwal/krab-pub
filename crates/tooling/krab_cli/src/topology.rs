use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::ServiceType;

pub(crate) fn dispatch_topology_action(
    action: &crate::TopologyAction,
    diagnostics: bool,
    json: bool,
) -> Result<()> {
    match action {
        crate::TopologyAction::Doctor => run_topology_doctor(diagnostics, json),
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

/// Identifiers for the sub-checks that can be skipped when a project does not
/// contain the artifact they inspect. Kept as constants so the diagnostics
/// printer and `krab doctor` can ask "did this one actually run?" without
/// string-matching prose.
pub(crate) const CHECK_SERVICE_SOURCE_SCAN: &str = "service-source-scan";
pub(crate) const CHECK_CONTRACT_PAYLOAD_DERIVES: &str = "contract-payload-derives";
pub(crate) const CHECK_ORCHESTRATOR_SERVICE_CONFIG: &str = "orchestrator-service-config";

/// A sub-check that did not run, and why.
///
/// Recorded rather than swallowed: a check that never executed must not be
/// reported the same way as a check that executed and found nothing, or the
/// output tells the reader their project is covered when it is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkippedTopologyCheck {
    pub(crate) check: &'static str,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TopologyDoctorReport {
    pub(crate) checked_rust_files: usize,
    pub(crate) contract_path: PathBuf,
    pub(crate) violations: Vec<String>,
    /// Sub-checks that were not applicable to this project. Empty inside the
    /// framework workspace, where every inspected path exists.
    pub(crate) skipped: Vec<SkippedTopologyCheck>,
}

impl TopologyDoctorReport {
    /// Whether the named sub-check actually executed.
    pub(crate) fn ran(&self, check: &str) -> bool {
        !self.skipped.iter().any(|entry| entry.check == check)
    }
}

/// Resolve a repository-relative path against a project root.
///
/// The `.` (or empty) root deliberately yields the bare relative path rather
/// than `./foo`, so running from the process CWD prints exactly the paths the
/// report has always printed — diagnostics text and violation strings are read
/// by humans and matched by CI logs.
fn resolve_project_path(root: &Path, relative: &str) -> PathBuf {
    if root.as_os_str().is_empty() || root == Path::new(".") {
        PathBuf::from(relative)
    } else {
        root.join(relative)
    }
}

pub(crate) fn topology_doctor_report() -> Result<TopologyDoctorReport> {
    topology_doctor_report_at(Path::new("."))
}

/// Build the topology report for a project rooted at `root`.
///
/// Root-parameterised so the tests can point it at a `tempfile::TempDir`
/// instead of mutating the process-global CWD.
///
/// Every path this touches (`services/`, the `krab_core` contract source,
/// `krab.toml`) is specific to a Krab *framework* checkout. A project produced
/// by `krab new` has none of them. Absence used to be a hard `Err`, which made
/// `krab doctor` and `krab topology doctor` exit 1 with a bare "Failed reading
/// crates/framework/krab_core/src/service_contract.rs" in every generated
/// project — a framework-repo assumption presented to users as their bug. A
/// missing artifact is now a recorded skip; an artifact that is present and
/// wrong is still a violation.
pub(crate) fn topology_doctor_report_at(root: &Path) -> Result<TopologyDoctorReport> {
    let mut violations: Vec<String> = Vec::new();
    let mut skipped: Vec<SkippedTopologyCheck> = Vec::new();

    let services_dir = resolve_project_path(root, "services");
    let mut rust_files = Vec::new();
    if services_dir.exists() {
        collect_rust_files_under(&services_dir, &mut rust_files)?;
    } else {
        skipped.push(SkippedTopologyCheck {
            check: CHECK_SERVICE_SOURCE_SCAN,
            reason: format!(
                "no `{}` directory; cross-service import and ServiceEndpoint scans not applicable",
                services_dir.display()
            ),
        });
    }

    for file in &rust_files {
        let owner = owning_service_name(file);
        // A source file we listed but cannot read is a finding about this
        // project, not a reason to abandon the whole report.
        let raw = match fs::read_to_string(file) {
            Ok(raw) => raw,
            Err(err) => {
                violations.push(format!(
                    "{}: could not be read for boundary analysis: {err}",
                    file.display()
                ));
                continue;
            }
        };

        for (line_idx, line) in raw.lines().enumerate() {
            if let Some(target) = parse_direct_service_import(line) {
                // Only a crate that runs as its own process is a service
                // boundary. A library under `services/` — such as
                // `service_frontend_islands`, the islands one service renders
                // and ships as its wasm bundle — is that service's own code,
                // and importing it crosses no network boundary.
                if !is_service_process_crate(&services_dir, &target) {
                    continue;
                }
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

    let contract_path =
        resolve_project_path(root, "crates/framework/krab_core/src/service_contract.rs");
    if contract_path.exists() {
        match fs::read_to_string(&contract_path) {
            Ok(contract_raw) => {
                for issue in detect_contract_payload_violations(&contract_raw) {
                    violations.push(format!("{}: {issue}", contract_path.display()));
                }
            }
            Err(err) => violations.push(format!(
                "{}: could not be read for contract payload analysis: {err}",
                contract_path.display()
            )),
        }
    } else {
        skipped.push(SkippedTopologyCheck {
            check: CHECK_CONTRACT_PAYLOAD_DERIVES,
            reason: format!(
                "`{}` is not present; this file only exists in a Krab framework checkout",
                contract_path.display()
            ),
        });
    }

    let service_config_path = resolve_project_path(root, "krab.toml");
    if service_config_path.exists() {
        match fs::read_to_string(&service_config_path) {
            Ok(service_config_raw) => {
                for issue in detect_service_config_violations(&service_config_raw) {
                    violations.push(format!("{}: {issue}", service_config_path.display()));
                }
            }
            Err(err) => violations.push(format!(
                "{}: could not be read for orchestrator policy analysis: {err}",
                service_config_path.display()
            )),
        }
    } else {
        skipped.push(SkippedTopologyCheck {
            check: CHECK_ORCHESTRATOR_SERVICE_CONFIG,
            reason: format!(
                "no `{}`; orchestrator health/restart policy not applicable",
                service_config_path.display()
            ),
        });
    }

    // Also runs under `krab release check` (via doctor.rs). Safe there: with
    // `KRAB_RUNTIME_TOPOLOGY`/`KRAB_RUNTIME_ENDPOINTS_JSON` unset, the strict
    // parser returns the monolith default and reports no violation — it only
    // fires when those vars are set to values the services would swallow.
    if let Some(issue) = runtime_topology_env_violation() {
        violations.push(issue);
    }

    Ok(TopologyDoctorReport {
        checked_rust_files: rust_files.len(),
        contract_path,
        violations,
        skipped,
    })
}

/// Validate the runtime topology environment with the strict parser.
///
/// Returns a violation string when `KRAB_RUNTIME_TOPOLOGY` /
/// `KRAB_RUNTIME_ENDPOINTS_JSON` are set to values the services would
/// silently swallow at runtime (unrecognized topology, unparseable endpoint
/// JSON, or distributed mode with an empty endpoint map).
fn runtime_topology_env_violation() -> Option<String> {
    match krab_core::service_contract::TopologyRuntime::from_env_checked() {
        Ok(_) => None,
        Err(err) => Some(format!("runtime topology environment invalid: {err:#}")),
    }
}

/// A sub-check that did not run, in `--json` form.
#[derive(Debug, Serialize)]
struct SkippedTopologyCheckJson<'a> {
    check: &'a str,
    reason: &'a str,
}

/// The `--json` shape of `krab topology doctor`.
///
/// `checked_rust_files` and `contract_path` are `null` when their sub-check did
/// not run, mirroring the human output's rule that a skipped check is never
/// reported as if it had produced a result.
#[derive(Debug, Serialize)]
struct TopologyDoctorJson<'a> {
    success: bool,
    checked_rust_files: Option<usize>,
    contract_path: Option<String>,
    violations: &'a [String],
    skipped: Vec<SkippedTopologyCheckJson<'a>>,
}

fn run_topology_doctor(diagnostics: bool, json: bool) -> Result<()> {
    if !json {
        println!("🩺 Running topology doctor...");
    }

    // Single source of truth: the same report `krab release check` consumes,
    // so the two paths cannot drift in which checks they run.
    let report = topology_doctor_report()?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&TopologyDoctorJson {
                success: report.violations.is_empty(),
                checked_rust_files: report
                    .ran(CHECK_SERVICE_SOURCE_SCAN)
                    .then_some(report.checked_rust_files),
                contract_path: report
                    .ran(CHECK_CONTRACT_PAYLOAD_DERIVES)
                    .then(|| report.contract_path.display().to_string()),
                violations: &report.violations,
                skipped: report
                    .skipped
                    .iter()
                    .map(|entry| SkippedTopologyCheckJson {
                        check: entry.check,
                        reason: entry.reason.as_str(),
                    })
                    .collect(),
            })?
        );

        // `--json` changes only what is printed, never the exit status.
        if !report.violations.is_empty() {
            anyhow::bail!("topology doctor failed");
        }
        return Ok(());
    }

    if diagnostics {
        // Only claim a check ran when it ran. The skipped list below carries
        // the rest, so nothing silently disappears from the output.
        if report.ran(CHECK_SERVICE_SOURCE_SCAN) {
            println!("   > checked Rust files: {}", report.checked_rust_files);
        }
        if report.ran(CHECK_CONTRACT_PAYLOAD_DERIVES) {
            println!(
                "   > checked contract payload serialization derives in {}",
                report.contract_path.display()
            );
        }
        if report.ran(CHECK_ORCHESTRATOR_SERVICE_CONFIG) {
            println!("   > checked orchestrator service health/restart policy in krab.toml");
        }
        println!(
            "   > checked runtime topology env (KRAB_RUNTIME_TOPOLOGY, KRAB_RUNTIME_ENDPOINTS_JSON) with strict parsing"
        );

        if !report.skipped.is_empty() {
            println!("   > skipped (not applicable to this project):");
            for entry in &report.skipped {
                println!("      - {}: {}", entry.check, entry.reason);
            }
        }
    }

    if report.violations.is_empty() {
        if report.skipped.is_empty() {
            println!("✅ topology doctor passed");
        } else {
            // Never print an unqualified pass when part of the suite never
            // ran — a skipped check is not a green check.
            println!(
                "✅ topology doctor passed ({} check(s) skipped as not applicable)",
                report.skipped.len()
            );
        }
        return Ok(());
    }

    eprintln!(
        "❌ topology doctor found {} issue(s):",
        report.violations.len()
    );
    for issue in &report.violations {
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

    let used_ports = match fs::read_to_string("krab.toml") {
        Ok(raw) => ports_declared_in_krab_toml(&raw),
        Err(_) => BTreeSet::new(),
    };
    let preferred = preferred_split_port(&service_crate);
    let port = choose_split_port(preferred, &used_ports)?;
    if port != preferred {
        println!(
            "   > port {preferred} is already used by a service in krab.toml; using {port} instead"
        );
    }

    let cargo_toml = format!(
        "[package]\nname = \"{service_crate}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nanyhow.workspace = true\naxum.workspace = true\ntokio.workspace = true\ntracing.workspace = true\ntracing-subscriber.workspace = true\nserde.workspace = true\nserde_json.workspace = true\n"
    );

    let main_rs = format!(
        "use anyhow::Result;\nuse axum::{{routing::get, Json, Router}};\nuse serde_json::json;\nuse std::net::SocketAddr;\n\nasync fn health() -> Json<serde_json::Value> {{\n    Json(json!({{\"status\": \"ok\", \"service\": \"{service_crate}\"}}))\n}}\n\nasync fn ready() -> Json<serde_json::Value> {{\n    Json(json!({{\"status\": \"ready\", \"service\": \"{service_crate}\"}}))\n}}\n\n#[tokio::main]\nasync fn main() -> Result<()> {{\n    tracing_subscriber::fmt::init();\n    let app = Router::new()\n        .route(\"/health\", get(health))\n        .route(\"/ready\", get(ready));\n\n    let addr = SocketAddr::from(([127, 0, 0, 1], {port}));\n    println!(\"{service_crate} listening on {{}}\", addr);\n\n    let listener = tokio::net::TcpListener::bind(addr).await?;\n    axum::serve(listener, app).await?;\n    Ok(())\n}}\n"
    );

    let readme = format!(
        "# {service_crate}\n\nGenerated by `krab topology split {slug}`.\n\n## Included scaffold\n- health/readiness endpoints\n- protocol adapter capability routes\n- domain skeleton\n- contract conformance placeholder tests\n\n## Next actions\n1. Move `{slug}` domain logic behind contract traits in `krab_core`.\n2. Implement local and remote adapters for each enabled protocol.\n3. Keep transport adapters thin and run the same contract tests against each adapter.\n4. Wire topology selection through runtime config and CI matrix.\n"
    );

    let test_rs = split_contract_conformance_test(&slug);

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

/// The contract-conformance test emitted into a generated split service.
///
/// `#[ignore]`d deliberately, and it must stay that way until it asserts
/// something real.
///
/// It previously ran, and passed unconditionally: it built a literal array and
/// asserted that the array contained one of the literals it had just been built
/// from. A green check beside the words "contract conformance" told users their
/// local-vs-remote adapter parity was covered when nothing was being compared at
/// all — worse than no test, because it answered the question before anyone
/// asked it. `#[ignore]` with a reason states the gap; the `panic!` body means
/// that anyone who runs it with `--ignored`, expecting real coverage, is told
/// plainly that there is none.
fn split_contract_conformance_test(slug: &str) -> String {
    format!(
        "// Placeholder. `cargo test` reports this as ignored, which is accurate:\n\
         // local-vs-remote contract conformance for `{slug}` is NOT yet covered.\n\
         //\n\
         // To make it real, assert that the local and remote adapters agree —\n\
         // drive both through the same inputs and compare the responses:\n\
         //\n\
         //   let local  = {slug}_local_adapter().handle(request.clone()).await?;\n\
         //   let remote = {slug}_remote_adapter().handle(request).await?;\n\
         //   assert_eq!(local, remote);\n\
         //\n\
         // Then delete the #[ignore].\n\
         #[test]\n\
         #[ignore = \"local-vs-remote contract conformance for {slug} is not implemented yet\"]\n\
         fn contract_conformance_for_{slug}_split() {{\n    \
             panic!(\n        \
                 \"contract conformance for {slug} is not implemented: the local and \\\n         \
                  remote adapters are never compared. See the comment above.\"\n    \
             );\n\
         }}\n"
    )
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

/// The range split-service ports are drawn from: `3200..SPLIT_PORT_END`.
const SPLIT_PORT_START: u16 = 3200;
const SPLIT_PORT_END: u16 = 3500;

/// The port a split service asks for first: a stable hash of its crate name
/// into the split range, so re-running the scaffold proposes the same port.
///
/// `DefaultHasher` only has to be stable within one binary here — the chosen
/// port is written into the generated files, never recomputed later.
fn preferred_split_port(service_crate: &str) -> u16 {
    let mut hasher = DefaultHasher::new();
    service_crate.hash(&mut hasher);
    let span = u64::from(SPLIT_PORT_END - SPLIT_PORT_START);
    SPLIT_PORT_START + (hasher.finish() % span) as u16
}

/// Every port a `krab.toml` already assigns: `port` keys, `KRAB_PORT` in a
/// service's `env`, and the port of its health-check URL.
///
/// The health-check URL counts because a service's probe is what the
/// orchestrator treats as the service; a new service bound to that port would
/// answer its neighbour's readiness checks. An unparseable manifest yields no
/// ports — `krab topology doctor` reports it, and this only steers a default.
fn ports_declared_in_krab_toml(raw: &str) -> BTreeSet<u16> {
    let mut ports = BTreeSet::new();
    let Ok(parsed) = toml::from_str::<toml::Value>(raw) else {
        return ports;
    };
    let Some(services) = parsed.get("services").and_then(toml::Value::as_table) else {
        return ports;
    };

    let port_of_url = |url: &str| -> Option<u16> {
        let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
        let authority = after_scheme.split('/').next()?;
        authority.rsplit_once(':')?.1.parse().ok()
    };

    for service in services.values().filter_map(toml::Value::as_table) {
        if let Some(port) = service.get("port").and_then(toml::Value::as_integer) {
            if let Ok(port) = u16::try_from(port) {
                ports.insert(port);
            }
        }
        if let Some(port) = service
            .get("env")
            .and_then(|env| env.get("KRAB_PORT"))
            .and_then(|value| match value {
                toml::Value::Integer(port) => u16::try_from(*port).ok(),
                toml::Value::String(port) => port.trim().parse().ok(),
                _ => None,
            })
        {
            ports.insert(port);
        }
        let urls = [
            service
                .get("healthcheck")
                .and_then(|probe| probe.get("url"))
                .and_then(toml::Value::as_str),
            service.get("healthcheck_url").and_then(toml::Value::as_str),
        ];
        for url in urls.into_iter().flatten() {
            if let Some(port) = port_of_url(url) {
                ports.insert(port);
            }
        }
    }
    ports
}

/// The first free port in the split range, starting at `preferred` and
/// wrapping around.
///
/// The hash alone used to decide, with no probe at all, so two domains whose
/// names hashed alike were scaffolded onto one port and the second failed to
/// bind — or answered the first one's health checks. Only ports declared in
/// `krab.toml` are considered: whether some unrelated process holds a port
/// right now says nothing about the machine the service will run on.
fn choose_split_port(preferred: u16, used: &BTreeSet<u16>) -> Result<u16> {
    let span = SPLIT_PORT_END - SPLIT_PORT_START;
    let offset = preferred.saturating_sub(SPLIT_PORT_START) % span;
    (0..span)
        .map(|step| SPLIT_PORT_START + (offset + step) % span)
        .find(|candidate| !used.contains(candidate))
        .with_context(|| {
            format!(
                "every split-service port in {SPLIT_PORT_START}..{SPLIT_PORT_END} is already \
                 assigned in krab.toml; free one, or set a port by hand in the generated main.rs \
                 and krab.toml"
            )
        })
}

fn register_workspace_member(member: &str) -> Result<()> {
    let workspace = PathBuf::from("Cargo.toml");
    let raw = fs::read_to_string(&workspace)
        .with_context(|| format!("Failed reading {}", workspace.display()))?;
    match insert_workspace_member(&raw, member)? {
        Some(updated) => {
            fs::write(&workspace, updated)
                .with_context(|| format!("Failed writing {}", workspace.display()))?;
            println!("🧩 Registered workspace member: {}", member);
        }
        None => println!("🧩 Workspace member already registered: {}", member),
    }
    Ok(())
}

/// Normalise a member path for comparison: forward slashes, no `./`, no
/// trailing slash.
fn normalize_member(member: &str) -> String {
    let member = member.replace('\\', "/");
    let member = member.strip_prefix("./").unwrap_or(&member);
    member.trim_end_matches('/').to_string()
}

/// Byte range of the `[workspace] members` array in `raw`, from its `[` to
/// its `]` inclusive.
///
/// A scanner rather than a search for `members = [` and the next `]`: that
/// search matched a `members` key in any table, required exactly one space
/// either side of `=`, and stopped at the first `]` even inside a string or a
/// comment.
fn members_array_span(raw: &str) -> Result<(usize, usize)> {
    let mut in_workspace = false;
    let mut offset = 0usize;
    let mut open = None;
    for line in raw.split_inclusive('\n') {
        let trimmed = line.trim();
        // A table header, possibly followed by a comment.
        let header = trimmed.split('#').next().unwrap_or_default().trim_end();
        if header.starts_with('[') && header.ends_with(']') {
            in_workspace = header == "[workspace]";
        } else if in_workspace {
            if let Some(rest) = trimmed.strip_prefix("members") {
                if let Some(value) = rest.trim_start().strip_prefix('=') {
                    let value = value.trim_start();
                    if value.starts_with('[') {
                        let value_at =
                            line.len() - line.trim_start().len() + trimmed.len() - value.len();
                        open = Some(offset + value_at);
                        break;
                    }
                }
            }
        }
        offset += line.len();
    }
    let open = open.context("Cargo.toml has no `members = [...]` in its [workspace] table")?;

    let bytes = raw.as_bytes();
    let mut index = open + 1;
    let mut in_string: Option<u8> = None;
    while index < bytes.len() {
        let byte = bytes[index];
        match in_string {
            Some(b'"') if byte == b'\\' => index += 1,
            Some(quote) if byte == quote => in_string = None,
            Some(_) => {}
            None => match byte {
                b'"' | b'\'' => in_string = Some(byte),
                b'#' => {
                    while index < bytes.len() && bytes[index] != b'\n' {
                        index += 1;
                    }
                }
                b']' => return Ok((open, index)),
                _ => {}
            },
        }
        index += 1;
    }
    anyhow::bail!("the [workspace] members array in Cargo.toml is not closed")
}

/// Add `member` to the `[workspace] members` array in `raw`, preserving the
/// array's layout. Returns `None` when it is already a member.
///
/// Handles single-line and multi-line arrays, with or without a trailing
/// comma, and keeps a comment on the last entry attached to that entry. The
/// previous implementation inserted `    "member",\n` before the closing
/// bracket, which produced `["a", "b"    "member",` — invalid TOML — for any
/// array without a trailing comma. The result is parsed before it is returned,
/// so a layout this does not understand fails here instead of corrupting the
/// manifest.
fn insert_workspace_member(raw: &str, member: &str) -> Result<Option<String>> {
    let wanted = normalize_member(member);
    let parsed: toml::Value = toml::from_str(raw).context("Cargo.toml is not valid TOML")?;
    let already = parsed
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .is_some_and(|members| {
            members
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|existing| normalize_member(existing) == wanted)
        });
    if already {
        return Ok(None);
    }

    let (open, close) = members_array_span(raw)?;
    let body = &raw[open + 1..close];

    // End of the last value in the array (exclusive), ignoring whitespace and
    // comments, and whether that value is followed by a comma.
    let mut last_value_end: Option<usize> = None;
    let mut trailing_comma = false;
    {
        let bytes = body.as_bytes();
        let mut index = 0;
        let mut in_string: Option<u8> = None;
        while index < bytes.len() {
            let byte = bytes[index];
            match in_string {
                Some(b'"') if byte == b'\\' => index += 1,
                Some(quote) if byte == quote => {
                    in_string = None;
                    last_value_end = Some(index + 1);
                    trailing_comma = false;
                }
                Some(_) => {}
                None => match byte {
                    b'"' | b'\'' => in_string = Some(byte),
                    b'#' => {
                        while index < bytes.len() && bytes[index] != b'\n' {
                            index += 1;
                        }
                        continue;
                    }
                    b',' => trailing_comma = true,
                    byte if byte.is_ascii_whitespace() => {}
                    _ => {
                        last_value_end = Some(index + 1);
                        trailing_comma = false;
                    }
                },
            }
            index += 1;
        }
    }

    let quoted = format!("\"{wanted}\"");
    let multi_line = body.contains('\n');
    let mut updated = raw.to_string();

    match last_value_end {
        None if multi_line => {
            // `members = [\n]`: one entry on its own line.
            updated.insert_str(open + 1, &format!("\n    {quoted},"));
        }
        None => {
            // `members = []`.
            updated.replace_range(open + 1..close, &quoted);
        }
        Some(end) if multi_line => {
            let value_at = open + 1 + end;
            // Indent like the first entry, and keep the comma style the array
            // already uses.
            let first_entry_line = body
                .lines()
                .find(|line| {
                    let line = line.trim();
                    !line.is_empty() && !line.starts_with('#')
                })
                .unwrap_or("    ");
            let indent: String = first_entry_line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let indent = if indent.is_empty() {
                "    ".to_string()
            } else {
                indent
            };
            // Insert at the end of the last entry's line so a trailing comment
            // on it stays with it.
            let line_end = updated[value_at..]
                .find('\n')
                .map_or(close, |at| value_at + at);
            let line_end = line_end.min(close);
            // Strip a `\r` so a CRLF manifest keeps one line-ending style.
            let line_end = if line_end > value_at && updated.as_bytes()[line_end - 1] == b'\r' {
                line_end - 1
            } else {
                line_end
            };
            let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
            let entry = if trailing_comma {
                format!("{eol}{indent}{quoted},")
            } else {
                format!("{eol}{indent}{quoted}")
            };
            updated.insert_str(line_end, &entry);
            if !trailing_comma {
                updated.insert(value_at, ',');
            }
        }
        Some(end) => {
            let value_at = open + 1 + end;
            if trailing_comma {
                // `["a",]` or `["a", ]`: after the comma.
                let comma_at = updated[value_at..close]
                    .find(',')
                    .map_or(value_at, |at| value_at + at + 1);
                updated.insert_str(comma_at, &format!(" {quoted},"));
            } else {
                updated.insert_str(value_at, &format!(", {quoted}"));
            }
        }
    }

    let reparsed: toml::Value = toml::from_str(&updated).context(
        "registering the workspace member would have produced invalid TOML; add it by hand",
    )?;
    let registered = reparsed
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .is_some_and(|members| {
            members
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|existing| existing == wanted)
        });
    if !registered {
        anyhow::bail!(
            "could not register `{wanted}` in the workspace members array; add it by hand"
        );
    }
    Ok(Some(updated))
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

    // `port` and `service_name` are written alongside the probe URL so the
    // generated service is told the topology its own health check asserts,
    // instead of inheriting whatever KRAB_PORT is ambient.
    raw.push_str(&format!(
        "\n{header}\ncommand = \"cargo\"\nargs = [\"run\", \"--bin\", \"{service_crate}\"]\nport = {port}\nservice_name = \"{service_key}\"\nenv = {{ RUST_LOG = \"info\" }}\n\n[services.{service_key}.restart_policy]\non_exit = true\nbackoff_ms = 700\nmax_attempts = 8\n\n[services.{service_key}.healthcheck]\nurl = \"http://127.0.0.1:{port}/ready\"\ntimeout_ms = 1500\nretries = 12\ninterval_ms = 300\n"
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

/// Whether `services/<name>` builds a binary — `src/main.rs` or `src/bin/` —
/// and so runs as a separate service. A `service_*` crate that is only a
/// library (no binary target) is shared code, not a boundary.
fn is_service_process_crate(services_dir: &Path, name: &str) -> bool {
    let crate_dir = services_dir.join(name);
    crate_dir.join("src").join("main.rs").is_file() || crate_dir.join("src").join("bin").is_dir()
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
    // Two services on one port are not a port conflict at runtime: the first to
    // bind wins and the second either fails to bind or is answered by its
    // neighbour's health endpoint. Only the manifest can see it, and only
    // before anything is spawned.
    let mut ports_seen: BTreeMap<u16, &String> = BTreeMap::new();
    // Two services under one identity do not fail at all: their log lines,
    // metrics, protocol selection and migration records simply collapse into
    // one. The orchestrator rejects this at startup; checking it here means
    // `krab topology doctor` catches it without running anything.
    let mut identities_seen: BTreeMap<String, &String> = BTreeMap::new();

    for (name, service) in services {
        let Some(service_table) = service.as_table() else {
            violations.push(format!("services.{name} must be a table"));
            continue;
        };

        if let Some(port) = service_table.get("port").and_then(toml::Value::as_integer) {
            match u16::try_from(port) {
                Ok(0) | Err(_) => violations.push(format!(
                    "services.{name}.port must be a port number between 1 and 65535, got `{port}`"
                )),
                Ok(port) => {
                    if let Some(previous) = ports_seen.insert(port, name) {
                        violations.push(format!(
                            "services.{previous} and services.{name} both declare port = {port}; \
                             the orchestrator injects this as KRAB_PORT, so give each service \
                             its own port"
                        ));
                    }
                }
            }
        }

        // Mirrors `ServiceDefinition::effective_service_name`: a declared
        // name wins, a blank one is treated as absent, and the manifest key
        // is the default. Table keys are unique, so a collision can only
        // come from an explicit `service_name`.
        let identity = service_table
            .get("service_name")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(name.as_str())
            .to_string();
        if let Some(previous) = identities_seen.insert(identity.clone(), name) {
            violations.push(format!(
                "services.{previous} and services.{name} both resolve to \
                 service_name = `{identity}`; the orchestrator injects this as \
                 KRAB_SERVICE_NAME, so give each service its own name"
            ));
        }

        // The orchestrator accepts two spellings for health checks and restart
        // policy: the `[services.X.healthcheck]` / `[services.X.restart_policy]`
        // tables, and the older flat keys (`healthcheck_url`,
        // `restart_on_exit`, ...). The doctor used to reject the flat form
        // outright, so a krab.toml the orchestrator ran happily failed
        // `topology doctor`. Both are validated now; the table form is the
        // one generators emit.
        match service_table
            .get("healthcheck")
            .and_then(toml::Value::as_table)
        {
            Some(healthcheck) => {
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
            }
            None => match service_table
                .get("healthcheck_url")
                .and_then(toml::Value::as_str)
            {
                Some(url) if url.ends_with("/ready") => {}
                Some(url) => violations.push(format!(
                    "services.{name}.healthcheck_url should target /ready, got `{url}`"
                )),
                None => violations.push(format!(
                    "services.{name} missing [services.{name}.healthcheck] \
                     (or the legacy healthcheck_url key)"
                )),
            },
        }

        match service_table
            .get("restart_policy")
            .and_then(toml::Value::as_table)
        {
            Some(restart_policy) => {
                for field in ["on_exit", "backoff_ms", "max_attempts"] {
                    if !restart_policy.contains_key(field) {
                        violations.push(format!("services.{name}.restart_policy.{field} missing"));
                    }
                }
            }
            None if service_table.contains_key("restart_on_exit") => {}
            None => violations.push(format!(
                "services.{name} missing [services.{name}.restart_policy] \
                 (or the legacy restart_on_exit key)"
            )),
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::{
        choose_split_port, insert_workspace_member, ports_declared_in_krab_toml,
        preferred_split_port, SPLIT_PORT_END, SPLIT_PORT_START,
    };
    use super::{
        detect_service_config_violations, parse_direct_service_import,
        runtime_topology_env_violation, split_contract_conformance_test, topology_doctor_report_at,
        CHECK_CONTRACT_PAYLOAD_DERIVES, CHECK_ORCHESTRATOR_SERVICE_CONFIG,
        CHECK_SERVICE_SOURCE_SCAN,
    };
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    fn members_of(raw: &str) -> Vec<String> {
        let parsed: toml::Value = toml::from_str(raw).unwrap_or_else(|e| panic!("{e}\n{raw}"));
        parsed["workspace"]["members"]
            .as_array()
            .expect("members array")
            .iter()
            .map(|m| m.as_str().expect("string member").to_string())
            .collect()
    }

    /// Every layout the old insertion broke, and the ones it happened to get
    /// right: the member lands last, the TOML stays valid, and a second run
    /// changes nothing.
    #[test]
    fn workspace_member_registration_handles_every_array_layout() {
        let cases = [
            // Single-line, no trailing comma: the old code produced
            // `["a", "b"    "services/x",` here.
            "[workspace]\nmembers = [\"a\", \"b\"]\n",
            "[workspace]\nmembers = [\"a\", \"b\",]\n",
            "[workspace]\nmembers = []\n",
            "[workspace]\nmembers=[\"a\"]\nresolver = \"2\"\n",
            // Multi-line, with and without trailing comma.
            "[workspace]\nmembers = [\n    \"a\",\n    \"b\",\n]\n",
            "[workspace]\nmembers = [\n    \"a\",\n    \"b\"\n]\n",
            "[workspace]\nmembers = [\n  \"a\",\n  \"b\"]\n",
            "[workspace]\nmembers = [\n]\n",
            // A `]` inside a comment or string must not end the array early.
            "[workspace]\nmembers = [\n    \"a\", # see [docs]\n    \"b\",  # last [one]\n]\n",
            // CRLF line endings.
            "[workspace]\r\nmembers = [\r\n    \"a\",\r\n    \"b\"\r\n]\r\n",
            // `members` in another table first must not be the one edited.
            "[package.metadata.x]\nmembers = [\"nope\"]\n\n[workspace]\nmembers = [\"a\"]\n",
        ];

        for raw in cases {
            let updated = insert_workspace_member(raw, "services/x")
                .unwrap_or_else(|err| panic!("{err:#}\n{raw}"))
                .unwrap_or_else(|| panic!("services/x was reported as present:\n{raw}"));
            let members = members_of(&updated);
            assert_eq!(
                members.last().map(String::as_str),
                Some("services/x"),
                "{updated}"
            );
            assert!(
                insert_workspace_member(&updated, "services/x")
                    .expect("second run")
                    .is_none(),
                "registration is not idempotent:\n{updated}"
            );
        }
    }

    #[test]
    fn multi_line_registration_keeps_the_layout_and_the_last_entrys_comment() {
        let raw =
            "[workspace]\nmembers = [\n    \"a\",\n    \"b\"  # keep me\n]\nresolver = \"2\"\n";

        let updated = insert_workspace_member(raw, "services/x")
            .expect("insert")
            .expect("changed");

        assert_eq!(
            updated,
            "[workspace]\nmembers = [\n    \"a\",\n    \"b\",  # keep me\n    \"services/x\"\n]\nresolver = \"2\"\n"
        );
    }

    #[test]
    fn multi_line_registration_with_a_trailing_comma_adds_one_line() {
        let raw = "[workspace]\nmembers = [\n    \"a\",\n]\n";

        let updated = insert_workspace_member(raw, "services/x")
            .expect("insert")
            .expect("changed");

        assert_eq!(
            updated,
            "[workspace]\nmembers = [\n    \"a\",\n    \"services/x\",\n]\n"
        );
    }

    /// A member spelled with a leading `./` or backslashes is the same member.
    #[test]
    fn an_existing_member_is_recognised_across_spellings() {
        let raw = "[workspace]\nmembers = [\"./services/x/\"]\n";
        assert!(insert_workspace_member(raw, "services\\x")
            .expect("parse")
            .is_none());
    }

    #[test]
    fn a_manifest_without_a_workspace_members_array_is_an_error() {
        let err = insert_workspace_member("[package]\nname = \"x\"\n", "services/x")
            .expect_err("no workspace");
        assert!(err.to_string().contains("members"), "{err}");
    }

    #[test]
    fn declared_ports_come_from_port_env_and_probe_urls() {
        let raw = r#"
[services.a]
port = 3201

[services.b]
env = { KRAB_PORT = "3202" }

[services.c.healthcheck]
url = "http://127.0.0.1:3203/ready"

[services.d]
healthcheck_url = "http://localhost:3204/ready"
"#;
        assert_eq!(
            ports_declared_in_krab_toml(raw),
            BTreeSet::from([3201, 3202, 3203, 3204])
        );
        assert!(ports_declared_in_krab_toml("not toml [").is_empty());
    }

    /// The hash picks the first candidate; a taken port moves to the next
    /// free one, wrapping at the end of the range.
    #[test]
    fn split_port_selection_skips_taken_ports_and_wraps() {
        let empty = BTreeSet::new();
        assert_eq!(choose_split_port(3250, &empty).expect("free"), 3250);

        let taken = BTreeSet::from([3250, 3251]);
        assert_eq!(choose_split_port(3250, &taken).expect("free"), 3252);

        let last = SPLIT_PORT_END - 1;
        let taken = BTreeSet::from([last]);
        assert_eq!(
            choose_split_port(last, &taken).expect("free"),
            SPLIT_PORT_START
        );

        let preferred = preferred_split_port("service_billing_split");
        assert!((SPLIT_PORT_START..SPLIT_PORT_END).contains(&preferred));
    }

    #[test]
    fn an_exhausted_split_port_range_is_reported() {
        let all: BTreeSet<u16> = (SPLIT_PORT_START..SPLIT_PORT_END).collect();
        let err = choose_split_port(3300, &all).expect_err("no free port");
        assert!(err.to_string().contains("already assigned"), "{err}");
    }

    /// `service_entry` with an explicit `service_name`, for the identity checks.
    fn service_entry_named(name: &str, port: u16, service_name: Option<&str>) -> String {
        let declared = match service_name {
            Some(value) => format!(
                "service_name = \"{value}\"
"
            ),
            None => String::new(),
        };
        service_entry(name, port).replace(
            &format!(
                "[services.{name}]
"
            ),
            &format!(
                "[services.{name}]
{declared}"
            ),
        )
    }

    /// Minimal service body carrying everything the other checks demand, so a
    /// port assertion fails on the port and nothing else.
    fn service_entry(name: &str, port: u16) -> String {
        format!(
            "[services.{name}]\ncommand = \"cargo\"\nport = {port}\n\n\
             [services.{name}.restart_policy]\non_exit = true\nbackoff_ms = 700\nmax_attempts = 8\n\n\
             [services.{name}.healthcheck]\nurl = \"http://127.0.0.1:{port}/ready\"\n\
             timeout_ms = 1500\nretries = 12\ninterval_ms = 300\n\n"
        )
    }

    #[test]
    fn two_services_declaring_one_port_are_reported_with_both_names() {
        let manifest = format!(
            "{}{}",
            service_entry("auth", 3001),
            service_entry("users", 3001)
        );

        let violations = detect_service_config_violations(&manifest);

        let duplicate = violations
            .iter()
            .find(|violation| violation.contains("both declare port"))
            .unwrap_or_else(|| panic!("duplicate port not reported; got {violations:?}"));
        assert!(duplicate.contains("services.auth"), "{duplicate}");
        assert!(duplicate.contains("services.users"), "{duplicate}");
        assert!(duplicate.contains("3001"), "{duplicate}");
    }

    #[test]
    fn distinct_ports_raise_no_port_violation() {
        let manifest = format!(
            "{}{}",
            service_entry("auth", 3001),
            service_entry("users", 3002)
        );

        let violations = detect_service_config_violations(&manifest);

        assert!(
            !violations.iter().any(|v| v.contains("port")),
            "unexpected port violations: {violations:?}"
        );
    }

    /// Two services can only collide on identity through an explicit
    /// `service_name` -- manifest keys are unique by construction.
    #[test]
    fn two_services_resolving_to_one_service_name_are_reported_with_both_names() {
        let manifest = format!(
            "{}{}",
            service_entry_named("auth", 3001, Some("shared")),
            service_entry_named("users", 3002, Some("shared"))
        );

        let violations = detect_service_config_violations(&manifest);

        let duplicate = violations
            .iter()
            .find(|violation| violation.contains("both resolve to service_name"))
            .unwrap_or_else(|| panic!("duplicate identity not reported; got {violations:?}"));
        assert!(duplicate.contains("services.auth"), "{duplicate}");
        assert!(duplicate.contains("services.users"), "{duplicate}");
        assert!(duplicate.contains("shared"), "{duplicate}");
    }

    #[test]
    fn distinct_service_names_raise_no_identity_violation() {
        let manifest = format!(
            "{}{}",
            service_entry_named("auth", 3001, Some("auth-api")),
            service_entry_named("users", 3002, Some("users-api"))
        );

        let violations = detect_service_config_violations(&manifest);

        assert!(
            !violations.iter().any(|v| v.contains("service_name")),
            "unexpected identity violations: {violations:?}"
        );
    }

    /// A blank `service_name` falls back to its manifest key rather than
    /// colliding with every other blank one on the empty string.
    #[test]
    fn blank_service_names_fall_back_to_their_keys() {
        let manifest = format!(
            "{}{}",
            service_entry_named("auth", 3001, Some("   ")),
            service_entry_named("users", 3002, Some(""))
        );

        let violations = detect_service_config_violations(&manifest);

        assert!(
            !violations.iter().any(|v| v.contains("service_name")),
            "unexpected identity violations: {violations:?}"
        );
    }

    #[test]
    fn an_out_of_range_port_is_reported() {
        let manifest = "[services.auth]\ncommand = \"cargo\"\nport = 70000\n".to_string();

        let violations = detect_service_config_violations(&manifest);

        assert!(
            violations
                .iter()
                .any(|v| v.contains("services.auth.port") && v.contains("70000")),
            "unexpected violations: {violations:?}"
        );
    }

    fn clear_topology_env() {
        std::env::remove_var("KRAB_RUNTIME_TOPOLOGY");
        std::env::remove_var("KRAB_RUNTIME_ENDPOINTS_JSON");
    }

    fn write_contract_file(root: &Path, body: &str) {
        let path = root.join("crates/framework/krab_core/src");
        fs::create_dir_all(&path).expect("create contract dir");
        fs::write(path.join("service_contract.rs"), body).expect("write contract file");
    }

    /// A project produced by `krab new` has no `services/`, no `krab.toml` in
    /// the framework's shape, and certainly no `krab_core` source tree. This
    /// used to return `Err("Failed reading
    /// crates/framework/krab_core/src/service_contract.rs")`, which made
    /// `krab doctor` and `krab topology doctor` exit 1 in every generated
    /// project.
    #[test]
    #[serial_test::serial]
    fn topology_report_skips_framework_only_paths_instead_of_erroring() {
        clear_topology_env();
        let root = tempfile::tempdir().expect("tempdir");

        let report = topology_doctor_report_at(root.path())
            .expect("a project without framework paths must still produce a report");

        assert!(report.violations.is_empty(), "{:?}", report.violations);
        assert_eq!(report.checked_rust_files, 0);

        let skipped: Vec<&str> = report.skipped.iter().map(|entry| entry.check).collect();
        assert!(skipped.contains(&CHECK_SERVICE_SOURCE_SCAN), "{skipped:?}");
        assert!(
            skipped.contains(&CHECK_CONTRACT_PAYLOAD_DERIVES),
            "{skipped:?}"
        );
        assert!(
            skipped.contains(&CHECK_ORCHESTRATOR_SERVICE_CONFIG),
            "{skipped:?}"
        );
        assert!(!report.ran(CHECK_CONTRACT_PAYLOAD_DERIVES));
    }

    /// Tolerating an absent contract file must not tolerate a broken one: the
    /// skip is about applicability, not about lowering the bar.
    #[test]
    #[serial_test::serial]
    fn topology_report_still_flags_a_present_but_violating_contract_file() {
        clear_topology_env();
        let root = tempfile::tempdir().expect("tempdir");
        write_contract_file(
            root.path(),
            "pub struct ContractPayload {\n    pub id: String,\n}\n",
        );

        let report = topology_doctor_report_at(root.path()).expect("report");

        assert!(report.ran(CHECK_CONTRACT_PAYLOAD_DERIVES));
        assert!(
            report.violations.iter().any(|issue| issue
                .contains("`ContractPayload` missing #[derive(Serialize, Deserialize)]")),
            "{:?}",
            report.violations
        );
    }

    /// The same rule for the derives-are-present case: a readable, conforming
    /// contract file is a real pass, not a skip.
    #[test]
    #[serial_test::serial]
    fn topology_report_accepts_a_present_and_conforming_contract_file() {
        clear_topology_env();
        let root = tempfile::tempdir().expect("tempdir");
        write_contract_file(
            root.path(),
            "#[derive(Debug, Serialize, Deserialize)]\npub struct ContractPayload {\n    pub id: String,\n}\n",
        );

        let report = topology_doctor_report_at(root.path()).expect("report");

        assert!(report.ran(CHECK_CONTRACT_PAYLOAD_DERIVES));
        assert!(report.violations.is_empty(), "{:?}", report.violations);
    }

    /// A present `krab.toml` is checked as before — absence is the only thing
    /// that became a skip.
    #[test]
    #[serial_test::serial]
    fn topology_report_still_flags_a_present_but_violating_krab_toml() {
        clear_topology_env();
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(
            root.path().join("krab.toml"),
            "[services.frontend]\ncommand = \"cargo\"\n",
        )
        .expect("write krab.toml");

        let report = topology_doctor_report_at(root.path()).expect("report");

        assert!(report.ran(CHECK_ORCHESTRATOR_SERVICE_CONFIG));
        assert!(
            report
                .violations
                .iter()
                .any(|issue| issue.contains("missing [services.frontend.healthcheck]")),
            "{:?}",
            report.violations
        );
    }

    /// The flat keys the orchestrator still accepts pass the doctor too.
    #[test]
    fn legacy_flat_healthcheck_and_restart_keys_are_accepted() {
        let manifest = "[services.auth]\n\
             command = \"cargo\"\n\
             port = 3001\n\
             healthcheck_url = \"http://127.0.0.1:3001/ready\"\n\
             restart_on_exit = true\n";
        let violations = detect_service_config_violations(manifest);
        assert!(violations.is_empty(), "{violations:?}");

        let manifest = "[services.auth]\ncommand = \"cargo\"\nport = 3001\n\
             healthcheck_url = \"http://127.0.0.1:3001/health\"\nrestart_on_exit = true\n";
        let violations = detect_service_config_violations(manifest);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("healthcheck_url should target /ready")),
            "{violations:?}"
        );
    }

    /// The generated test must never again assert something that cannot fail.
    ///
    /// The original body was
    /// `assert!(["local_adapter", "remote_adapter", "payload_serialization"]
    ///     .contains(&"payload_serialization"))` — a literal array asserted to
    /// contain a literal it was built from. It reported green forever under a
    /// name claiming contract coverage.
    #[test]
    fn generated_conformance_test_is_ignored_and_not_tautological() {
        let generated = split_contract_conformance_test("billing");

        assert!(
            generated.contains("#[ignore = \""),
            "the placeholder must be #[ignore]d with a reason, not silently green:\n{generated}"
        );
        assert!(
            generated.contains("panic!("),
            "running it with --ignored must fail loudly rather than pass:\n{generated}"
        );
        assert!(
            !generated.contains("required_contract_checks"),
            "the self-satisfying array assertion is back:\n{generated}"
        );
        assert!(
            !generated.contains("assert!("),
            "an assert! here is almost certainly tautological again:\n{generated}"
        );
    }

    #[test]
    fn generated_conformance_test_names_the_domain_everywhere_it_should() {
        let generated = split_contract_conformance_test("billing");

        assert!(generated.contains("fn contract_conformance_for_billing_split()"));
        assert!(generated.contains("local-vs-remote contract conformance for billing"));
        // The worked example tells the reader what "make it real" means.
        assert!(generated.contains("assert_eq!(local, remote);"));
    }

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

    // Serialized: these mutate process-global env vars.
    #[test]
    #[serial_test::serial]
    fn topology_doctor_passes_clean_runtime_topology_env() {
        clear_topology_env();
        assert_eq!(runtime_topology_env_violation(), None);
    }

    #[test]
    #[serial_test::serial]
    fn topology_doctor_flags_malformed_runtime_endpoints_json() {
        clear_topology_env();
        std::env::set_var("KRAB_RUNTIME_ENDPOINTS_JSON", "{not json");

        let violation =
            runtime_topology_env_violation().expect("malformed endpoints JSON must be flagged");
        assert!(
            violation.contains("invalid KRAB_RUNTIME_ENDPOINTS_JSON"),
            "{violation}"
        );
        clear_topology_env();
    }

    #[test]
    #[serial_test::serial]
    fn topology_doctor_flags_split_mode_with_empty_endpoint_map() {
        clear_topology_env();
        std::env::set_var("KRAB_RUNTIME_TOPOLOGY", "split");

        let violation =
            runtime_topology_env_violation().expect("split mode with no endpoints must be flagged");
        assert!(violation.contains("endpoint map is empty"), "{violation}");
        clear_topology_env();
    }

    #[test]
    fn direct_service_import_parser_detects_service_crates() {
        assert_eq!(
            parse_direct_service_import("use service_users::client::UsersClient;"),
            Some("service_users".to_string())
        );
        assert_eq!(parse_direct_service_import("use crate::domain;"), None);
    }

    /// Importing a library crate under `services/` is not a cross-service
    /// import; importing another service's binary crate still is.
    #[test]
    fn library_crates_under_services_are_not_boundaries() {
        let root = tempfile::tempdir().expect("tempdir");
        let write = |path: &str, body: &str| {
            let full = root.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, body).unwrap();
        };
        write(
            "services/service_web/src/main.rs",
            "use service_web_islands::Counter;\nuse service_users::Client;\nfn main() {}\n",
        );
        write(
            "services/service_web_islands/src/lib.rs",
            "pub struct Counter;\n",
        );
        write("services/service_users/src/main.rs", "fn main() {}\n");

        let report = topology_doctor_report_at(root.path()).expect("report");
        let imports: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.contains("direct cross-service import"))
            .collect();
        assert_eq!(imports.len(), 1, "{:?}", report.violations);
        assert!(imports[0].contains("service_users"), "{imports:?}");
    }
}
