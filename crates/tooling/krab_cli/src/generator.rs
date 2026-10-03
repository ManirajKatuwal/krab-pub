use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::project_template::DependencySource;
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
            path_deps,
        } => {
            let deps = DependencySource::from_path_deps(path_deps.as_deref())?;
            let request = ServiceRequest {
                name,
                service_type: r#type,
                exposure_mode,
                protocols,
                topology,
                deps: &deps,
            };
            generate_service_in(Path::new("."), &request)
        }
        GenResource::Component { name } => generate_component(name),
        GenResource::Route { name } => generate_route(name),
        GenResource::ServerFunction { name } => generate_server_function(name),
    }
}

/// Everything `krab gen service` was asked for, bundled so the generator can
/// take a root directory as well without an eight-argument signature.
struct ServiceRequest<'a> {
    name: &'a str,
    service_type: &'a ServiceType,
    exposure_mode: &'a ExposureMode,
    protocols: &'a Option<Vec<ServiceType>>,
    topology: &'a Topology,
    deps: &'a DependencySource,
}

/// Generate a service under `root`. Root-parameterised for the same reason
/// as [`generate_component_in`]: tests must not change the process CWD.
fn generate_service_in(root: &Path, request: &ServiceRequest<'_>) -> Result<()> {
    let name = request.name;
    println!(
        "🦀 Generating service '{}' of type {:?} (mode={:?}, topology={:?})...",
        name, request.service_type, request.exposure_mode, request.topology
    );

    let selected_protocols = resolve_protocols(
        request.service_type,
        request.exposure_mode,
        request.protocols,
    )?;

    if *request.topology == Topology::SplitServices {
        return generate_split_service_topology(root, name, &selected_protocols, request.deps);
    }

    let path = root.join(name);
    if path.exists() {
        anyhow::bail!("Directory '{}' already exists", name);
    }

    fs::create_dir(&path).context("Failed to create service directory")?;

    let mut feature_names: Vec<&str> = Vec::new();
    for proto in &selected_protocols {
        let feature = protocol_feature(proto);
        if !feature_names.contains(&feature) {
            feature_names.push(feature);
        }
    }
    if feature_names.is_empty() {
        feature_names.push(DEFAULT_FEATURE);
    }

    let cargo_toml = render_service_manifest(name, &feature_names, request.deps);

    fs::write(path.join("Cargo.toml"), cargo_toml)?;
    fs::create_dir(path.join("src"))?;

    fs::write(
        path.join("src/main.rs"),
        render_service_main(request.exposure_mode, &selected_protocols),
    )?;

    if *request.exposure_mode == ExposureMode::Multi {
        generate_multi_mode_layout(&path, &selected_protocols)?;
    }

    println!("✅ Service '{}' created successfully!", name);
    println!("{}", render_service_next_step(root, &[name.to_string()]));
    // `--type rpc` produces `features = ["rest"]`, which looks like the flag was
    // ignored unless the mapping is stated.
    if selected_protocols.contains(&ServiceType::Rpc) {
        println!("👉 RPC is served over the REST surface, so it enables krab_core's `rest` feature — there is no separate `rpc` feature.");
    }

    Ok(())
}

/// The nearest directory at or above `start` whose `Cargo.toml` declares a
/// `[workspace]`, which is the manifest Cargo itself would treat as the
/// workspace root for a crate created under `start`.
///
/// A manifest that cannot be read or parsed is passed over rather than
/// treated as an error: this only chooses which hint to print.
fn enclosing_workspace_root(start: &Path) -> Option<PathBuf> {
    let start = start.canonicalize().ok()?;
    start.ancestors().find_map(|dir| {
        let raw = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
        let manifest: toml::Value = toml::from_str(&raw).ok()?;
        manifest.get("workspace")?;
        Some(dir.to_path_buf())
    })
}

/// What to do after `krab gen service`, which depends on where it ran.
///
/// It used to print `"Add '<name>' to your workspace Cargo.toml members list"`
/// unconditionally. A `krab new` project has no `[workspace]`, so there was no
/// list to add to — and adding one to a package manifest is not what makes the
/// new crate build. Inside a workspace, Cargo refuses to build a crate under
/// the workspace root that the members list does not name, so the hint is
/// right there and names the exact entry; everywhere else the crate is
/// standalone and simply builds from its own directory.
fn render_service_next_step(root: &Path, crates: &[String]) -> String {
    match enclosing_workspace_root(root) {
        Some(workspace_root) => {
            let base = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            let members = crates
                .iter()
                .map(|name| {
                    let member = base
                        .join(name)
                        .strip_prefix(&workspace_root)
                        .map(|relative| relative.to_path_buf())
                        .unwrap_or_else(|_| PathBuf::from(name));
                    format!("\"{}\"", member.to_string_lossy().replace('\\', "/"))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "👉 Add {members} to `[workspace] members` in {}; Cargo will not build a crate \
                 under the workspace root that the members list leaves out.",
                // Canonical paths carry a `\\?\` prefix on Windows; strip it
                // so the hint shows the path the user would type.
                crate::project_template::path_for_toml(&workspace_root.join("Cargo.toml"))
            )
        }
        None => {
            let commands = crates
                .iter()
                .map(|name| format!("`cd {name} && cargo build`"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "👉 No Cargo workspace encloses this directory, so the generated crate is \
                 standalone: build it with {commands}."
            )
        }
    }
}

/// Render `src/main.rs` for a generated single-crate service.
///
/// In multi mode the crate also carries `capabilities.rs`, `domain/` and
/// `adapters/`. Those used to be written without a single `mod` declaration,
/// so none of it was ever compiled — a type error in the generated domain
/// layer, or in the user's first edit to it, went unnoticed by `cargo build`.
/// The modules are now declared and `main` calls into them, which also keeps
/// the scaffold free of `dead_code` warnings under `-D warnings`.
fn render_service_main(exposure_mode: &ExposureMode, protocols: &[ServiceType]) -> String {
    let multi = *exposure_mode == ExposureMode::Multi;
    let exposure_mode_value = if multi { "multi" } else { "single" };
    let protocols_value = protocols
        .iter()
        .map(protocol_label)
        .collect::<Vec<&str>>()
        .join(",");

    let modules = if multi {
        "mod adapters;\nmod capabilities;\nmod domain;\n\n"
    } else {
        ""
    };
    let capabilities = if multi {
        "    for capability in capabilities::build_capabilities() {\n        \
         println!(\"capability={capability}\");\n    }\n"
    } else {
        ""
    };

    format!(
        r#"{modules}use anyhow::Result;
use async_trait::async_trait;
use krab_core::service::ApiService;

struct Service;

#[async_trait]
impl ApiService for Service {{
    async fn start(&self) -> Result<()> {{
        println!("Service started!");
        Ok(())
    }}
}}

#[tokio::main]
async fn main() -> Result<()> {{
    tracing_subscriber::fmt::init();
    println!("exposure_mode={exposure_mode_value}");
    println!("protocols={protocols_value}");
{capabilities}    let service = Service;
    service.start().await
}}
"#
    )
}

/// The `krab_core` Cargo feature a generated service needs for one protocol.
///
/// Two mappings are not one-to-one and are deliberate:
///
/// - `Rpc` has no feature of its own; Krab serves RPC over the REST surface.
///   `generate_service` prints a note so the substitution is visible.
/// - `Grpc` maps to `grpc-semantics`, the canonical name. The `grpc` alias was
///   removed from `krab_core` in 0.6.0, so a manifest naming it no longer
///   resolves. See ADR 0007 for why the name changed.
fn protocol_feature(protocol: &ServiceType) -> &'static str {
    match protocol {
        ServiceType::Rest => "rest",
        ServiceType::Graphql => "graphql",
        ServiceType::Rpc => "rest",
        ServiceType::Grpc => "grpc-semantics",
    }
}

/// Feature requested when protocol resolution yields nothing to map.
const DEFAULT_FEATURE: &str = "rest";

/// Render the manifest for a `krab gen service` single-crate service.
///
/// `krab_core` resolves from crates.io at the CLI's own (workspace) version.
/// The old output emitted `path = "../krab_core"`, a directory that has not
/// existed since the crates/ reorganisation (`crates/framework/krab_core`), so
/// no generated service could ever resolve its dependencies. `async-trait` is
/// declared because the generated `main.rs` implements
/// `krab_core::service::ApiService` with `#[async_trait]`.
///
/// `deps` is `--path-deps`: [`DependencySource::Registry`] reproduces the
/// crates.io line exactly, [`DependencySource::Path`] points at a checkout.
fn render_service_manifest(name: &str, feature_names: &[&str], deps: &DependencySource) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = {{ version = "1.0", features = ["full"] }}
{krab_core}
async-trait = "0.1"
anyhow = "1.0"
tracing = "0.1"
tracing-subscriber = "0.3"
serde = {{ version = "1.0", features = ["derive"] }}
"#,
        krab_core = deps.render("krab_core", KRAB_CORE_DIR, feature_names),
    )
}

/// `krab_core`'s location inside a Krab checkout, for `--path-deps`.
const KRAB_CORE_DIR: &str = "crates/framework/krab_core";

/// Render the manifest for one protocol-adapter crate of a split topology.
/// Same registry-resolution rationale as [`render_service_manifest`].
fn render_split_adapter_manifest(
    crate_name: &str,
    domain_name: &str,
    deps: &DependencySource,
) -> String {
    format!(
        "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{domain_name} = {{ path = \"../{domain_name}\" }}\n{krab_core}\n",
        krab_core = deps.render("krab_core", KRAB_CORE_DIR, &["rest"]),
    )
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

const MULTI_DOMAIN_MOD_RS: &str = "pub mod models;\npub mod service;\n";

const MULTI_DOMAIN_MODELS_RS: &str = "\
/// The service's core entity. Replace the unit struct with real fields.
#[derive(Debug, Clone, Default)]
pub struct DomainModel;
";

const MULTI_DOMAIN_SERVICE_RS: &str = r#"use super::models::DomainModel;

/// Business logic shared by every protocol adapter. Adapters translate their
/// protocol into calls on this trait and never reach past it.
pub trait DomainService: Send + Sync {
    fn describe(&self, model: &DomainModel) -> String;
}

/// Placeholder implementation the generated adapters are wired against.
pub struct DefaultDomainService;

impl DomainService for DefaultDomainService {
    fn describe(&self, model: &DomainModel) -> String {
        format!("{model:?}")
    }
}
"#;

/// The module names of the adapters a multi-mode service gets, in protocol
/// order. Falls back to `rest` when resolution produced nothing.
fn multi_adapter_modules(selected_protocols: &[ServiceType]) -> Vec<String> {
    let mut modules: Vec<String> = selected_protocols
        .iter()
        .map(|protocol| protocol_label(protocol).replace('-', "_"))
        .collect();
    if modules.is_empty() {
        modules.push("rest".to_string());
    }
    modules
}

/// One adapter module: a thin translation onto [`MULTI_DOMAIN_SERVICE_RS`].
fn render_multi_adapter(module: &str) -> String {
    format!(
        r#"use crate::domain::models::DomainModel;
use crate::domain::service::DomainService;

/// What the `{module}` adapter serves. Replace with the real mount for this
/// protocol (a router, a schema, or an RPC table) built on `service`.
pub fn capability(service: &dyn DomainService) -> String {{
    format!("{module}: {{}}", service.describe(&DomainModel))
}}
"#
    )
}

/// `adapters/mod.rs`: declarations sorted, because rustfmt's
/// `reorder_modules` sorts them and the generated-project gate runs
/// `cargo fmt --check`.
fn render_multi_adapters_mod(modules: &[String]) -> String {
    let mut sorted = modules.to_vec();
    sorted.sort();
    sorted
        .iter()
        .map(|module| format!("pub mod {module};\n"))
        .collect()
}

/// `capabilities.rs`: calls every adapter, which is what makes the whole
/// generated module tree reachable from `main`.
///
/// One adapter renders as `vec![x]` and several as one element per line —
/// the two shapes rustfmt itself produces for these lengths.
fn render_multi_capabilities(modules: &[String]) -> String {
    let calls: Vec<String> = modules
        .iter()
        .map(|module| format!("adapters::{module}::capability(&service)"))
        .collect();
    let body = if calls.len() == 1 {
        format!("    vec![{}]\n", calls[0])
    } else {
        let mut body = "    vec![\n".to_string();
        for call in &calls {
            body.push_str(&format!("        {call},\n"));
        }
        body.push_str("    ]\n");
        body
    };
    format!(
        "use crate::adapters;\nuse crate::domain::service::DefaultDomainService;\n\n\
         /// One entry per protocol adapter this service mounts.\n\
         pub fn build_capabilities() -> Vec<String> {{\n    \
         let service = DefaultDomainService;\n{body}}}\n"
    )
}

fn generate_multi_mode_layout(path: &Path, selected_protocols: &[ServiceType]) -> Result<()> {
    let domain_dir = path.join("src/domain");
    let adapters_dir = path.join("src/adapters");
    fs::create_dir_all(&domain_dir)?;
    fs::create_dir_all(&adapters_dir)?;

    let modules = multi_adapter_modules(selected_protocols);

    fs::write(
        path.join("src/capabilities.rs"),
        render_multi_capabilities(&modules),
    )?;
    fs::write(domain_dir.join("mod.rs"), MULTI_DOMAIN_MOD_RS)?;
    fs::write(domain_dir.join("models.rs"), MULTI_DOMAIN_MODELS_RS)?;
    fs::write(domain_dir.join("service.rs"), MULTI_DOMAIN_SERVICE_RS)?;

    for module in &modules {
        fs::write(
            adapters_dir.join(format!("{module}.rs")),
            render_multi_adapter(module),
        )?;
    }
    fs::write(
        adapters_dir.join("mod.rs"),
        render_multi_adapters_mod(&modules),
    )?;
    Ok(())
}

fn generate_split_service_topology(
    root: &Path,
    name: &str,
    selected_protocols: &[ServiceType],
    deps: &DependencySource,
) -> Result<()> {
    let domain_name = format!("{}-domain", name);
    let domain_path = root.join(&domain_name);
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
        let crate_path = root.join(&crate_name);
        if crate_path.exists() {
            anyhow::bail!("Directory '{}' already exists", crate_name);
        }
        fs::create_dir(&crate_path)?;
        fs::create_dir_all(crate_path.join(format!("src/adapters/{}", label)))?;
        fs::create_dir_all(crate_path.join("src/domain"))?;
        fs::write(
            crate_path.join("Cargo.toml"),
            render_split_adapter_manifest(&crate_name, &domain_name, deps),
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
    let mut crates = vec![domain_name];
    crates.extend(created);
    println!("{}", render_service_next_step(root, &crates));
    Ok(())
}

/// Marker lines a `krab new` scaffold carries in `src/main.rs` so `krab gen`
/// can extend the module tree without parsing or reformatting the user's code.
///
/// Both sit at statement position as ordinary comments and insertions go
/// immediately above them, so rustfmt has nothing it wants to change — which
/// matters because a generated project runs `cargo fmt --all --check` in the
/// CI workflow `krab new` writes for it.
const MAIN_MODULES_MARKER: &str = "// krab:modules";

/// Marker inside the scaffold's `async fn main`, just after the Router is
/// built. Router merges are inserted above it.
const MAIN_ROUTES_MARKER: &str = "// krab:routes";

/// Marker inside the `router()` body of a generated `src/routes/mod.rs`.
const ROUTE_REGISTRATIONS_MARKER: &str = "// krab:route-registrations";

/// Initial contents of a generated `src/routes/mod.rs`.
///
/// `router()` is generic over the state type because the starter templates do
/// not agree on one: `--template default` builds a `Router` (state `()`) while
/// `--template saas` builds a `Router<AppState>`, and `Router::merge` only
/// accepts a router carrying the *same* state type. The bounds are exactly the
/// ones axum 0.8 puts on the `impl<S> Router<S>` block that provides `route`
/// and `merge`, so a generic `router::<S>()` merges into either.
///
/// Registrations accumulate as re-assignment statements rather than as a
/// `Router::new().route(..).route(..)` chain, because a generated project runs
/// both `cargo fmt --all --check` and clippy with `-D warnings` in the CI
/// `krab new` writes for it, and the obvious shapes fail one or the other:
///
/// - A method chain is collapsed onto one line by rustfmt as soon as it fits
///   in `chain_width` (60 columns), which is exactly the one-route case the
///   first `krab gen route` produces.
/// - `let router = router.route(..);` as the last statement before `router`
///   trips `clippy::let_and_return`, at every route count.
///
/// Statements at the marker's own indentation are immune to both. The
/// `unused_mut` allow covers the transient state before the first registration
/// is inserted — the generator writes this file and the first registration in
/// the same run, so it is only reachable by removing every route again.
const ROUTES_MOD_RS: &str = r#"use axum::routing::get;
use axum::Router;

/// Routes generated by `krab gen route`.
///
/// Generic over the state type so this merges into a plain `Router` and a
/// `Router<AppState>` alike.
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    // Empty until `krab gen route` inserts a registration below.
    #[allow(unused_mut)]
    let mut router = Router::new();
    // krab:route-registrations
    router
}
"#;

/// Initial contents of a generated `src/components/mod.rs`.
///
/// Nothing calls a freshly generated component, and the CI workflow `krab new`
/// writes builds with `RUSTFLAGS: -D warnings`, so `dead_code` would turn
/// scaffolding into a failed build. The allow is scoped to this module tree and
/// is meant to be deleted once the items are used.
const COMPONENTS_MOD_RS: &str = "\
// Generated by `krab gen component`.
//
// Scaffolded components are not referenced from `main.rs` yet and the generated
// CI builds with `-D warnings`, so `dead_code` would fail the build before you
// have used them. Remove this once they are wired into a view.
#![allow(dead_code)]
";

/// Initial contents of a generated `src/server_functions/mod.rs`.
/// Same `-D warnings` rationale as [`COMPONENTS_MOD_RS`].
const SERVER_FUNCTIONS_MOD_RS: &str = "\
// Generated by `krab gen server-function`.
//
// Scaffolded server functions are not referenced from `main.rs` yet and the
// generated CI builds with `-D warnings`, so `dead_code` would fail the build
// before you have mounted them. Remove this once they are wired up.
#![allow(dead_code)]
";

/// The statement `main` needs so the collected route router is actually served.
const ROUTES_MERGE: &str = "let app = app.merge(routes::router());";

/// Substring that proves the merge is already in place, however the user has
/// since reformatted or renamed the binding around it.
const ROUTES_MERGE_CALL: &str = "routes::router()";

/// Whether a generated source file was written or an existing one left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOutcome {
    Created,
    Kept,
}

/// Write a generated file, refusing to clobber one that is already there.
///
/// The component, route, and server-function generators used a bare
/// `fs::write`, so re-running `krab gen component Counter` silently replaced
/// hand-written code with boilerplate and still reported success.
///
/// An existing file is reported rather than treated as an error: the wiring
/// that follows is idempotent, so a second `krab gen route about` restores a
/// `mod routes;` line the user deleted without touching the route module.
fn write_new_file(path: &Path, contents: &str) -> Result<FileOutcome> {
    if path.exists() {
        return Ok(FileOutcome::Kept);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory '{}'", parent.display()))?;
    }
    fs::write(path, contents).with_context(|| format!("Failed to write '{}'", path.display()))?;
    Ok(FileOutcome::Created)
}

fn report_file(outcome: FileOutcome, kind: &str, name: &str, relative: &Path) {
    match outcome {
        FileOutcome::Created => {
            println!("✅ {} '{}' created at {}", kind, name, relative.display())
        }
        FileOutcome::Kept => println!(
            "↩️  {} '{}' already exists at {} — kept as is, nothing was overwritten.",
            kind,
            name,
            relative.display()
        ),
    }
}

/// True when some line of `content`, ignoring surrounding whitespace, satisfies
/// `matches`.
fn has_line(content: &str, matches: impl Fn(&str) -> bool) -> bool {
    content.lines().any(|line| matches(line.trim()))
}

/// Insert `line` immediately above the first line equal to `marker`, at the
/// marker's own indentation.
///
/// Returns `None` when `content` carries no such marker; callers degrade to
/// printed advice rather than guessing where the line belongs.
fn insert_above_marker(content: &str, marker: &str, line: &str) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    let mut inserted = false;
    for raw in content.split('\n') {
        if !inserted && raw.trim() == marker {
            let indent: String = raw.chars().take_while(|c| c.is_whitespace()).collect();
            // `split('\n')` leaves the `\r` of a CRLF line ending on the line,
            // so match it and the file keeps one line ending style throughout.
            let eol = if raw.ends_with('\r') { "\r" } else { "" };
            out.push(format!("{indent}{line}{eol}"));
            inserted = true;
        }
        out.push(raw.to_string());
    }
    inserted.then(|| out.join("\n"))
}

/// The module name of a `mod x;` or `pub mod x;` line, if it is one.
fn module_declaration_name(line: &str) -> Option<&str> {
    let name = line
        .strip_prefix("pub ")
        .unwrap_or(line)
        .strip_prefix("mod ")?
        .strip_suffix(';')?;
    let is_ident = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    is_ident.then_some(name)
}

/// Index inside the run of declarations `lines[start..end]` at which `name`
/// belongs, keeping the run alphabetical.
///
/// rustfmt's `reorder_modules` is on by default and sorts a consecutive run of
/// `mod` items by name, so inserting in generation order would leave the
/// project failing the `cargo fmt --all --check` its own CI runs.
fn sorted_position(lines: &[String], start: usize, end: usize, name: &str) -> usize {
    for (offset, line) in lines[start..end].iter().enumerate() {
        match module_declaration_name(line.trim()) {
            Some(existing) if existing > name => return start + offset,
            _ => continue,
        }
    }
    end
}

/// Insert a `mod <name>;` declaration into the run of declarations immediately
/// above `marker`, alphabetically. Returns `None` when the marker is absent.
fn insert_module_declaration(content: &str, marker: &str, decl: &str) -> Option<String> {
    let name = module_declaration_name(decl)?;
    let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
    let marker_at = lines.iter().position(|line| line.trim() == marker)?;

    let mut start = marker_at;
    while start > 0 && module_declaration_name(lines[start - 1].trim()).is_some() {
        start -= 1;
    }

    let eol = if lines[marker_at].ends_with('\r') {
        "\r"
    } else {
        ""
    };
    let at = sorted_position(&lines, start, marker_at, name);
    lines.insert(at, format!("{decl}{eol}"));
    Some(lines.join("\n"))
}

/// Add a `pub mod` declaration to the trailing run of declarations in a module
/// index, alphabetically.
///
/// A blank line separates the run from the item above it. Both that and the
/// ordering exist so the file stays `rustfmt --check` clean as declarations
/// accumulate — see [`sorted_position`].
fn append_declaration(content: &str, decl: &str) -> String {
    let Some(name) = module_declaration_name(decl) else {
        return content.to_string();
    };
    let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();

    // Trailing empty elements are the final newline, not content.
    let mut end = lines.len();
    while end > 0 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && module_declaration_name(lines[start - 1].trim()).is_some() {
        start -= 1;
    }

    if start == end {
        lines.insert(end, String::new());
        lines.insert(end + 1, decl.to_string());
    } else {
        lines.insert(sorted_position(&lines, start, end, name), decl.to_string());
    }
    lines.join("\n")
}

/// Ensure `src/<dir>/mod.rs` exists, declares `pub mod <stem>;`, and — for
/// routes — registers the handler on the router it collects.
///
/// Every step is a no-op once it has been done, so re-running a generator
/// neither duplicates a declaration nor errors.
fn ensure_module_index(
    root: &Path,
    dir: &str,
    stem: &str,
    template: &str,
    registration: Option<&str>,
) -> Result<()> {
    let relative = PathBuf::from("src").join(dir).join("mod.rs");
    let mod_rs = root.join(&relative);
    if !mod_rs.exists() {
        if let Some(parent) = mod_rs.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory '{}'", parent.display()))?;
        }
        fs::write(&mod_rs, template)
            .with_context(|| format!("Failed to write '{}'", relative.display()))?;
        println!("✅ Module index created at {}", relative.display());
    }

    let original = fs::read_to_string(&mod_rs)
        .with_context(|| format!("Failed to read '{}'", relative.display()))?;
    let mut content = original.clone();

    if let Some(registration) = registration {
        if !has_line(&content, |line| line == registration) {
            match insert_above_marker(&content, ROUTE_REGISTRATIONS_MARKER, registration) {
                Some(updated) => content = updated,
                // A hand-written mod.rs the generator did not create. Its
                // router is the user's to shape, so say what is missing rather
                // than reshape it.
                None => println!(
                    "👉 {} has no `{}` marker; add `{}` to its router yourself.",
                    relative.display(),
                    ROUTE_REGISTRATIONS_MARKER,
                    registration
                ),
            }
        }
    }

    let decl = format!("pub mod {stem};");
    let declared = has_line(&content, |line| line == decl);
    if !declared {
        content = append_declaration(&content, &decl);
    }

    if content != original {
        fs::write(&mod_rs, &content)
            .with_context(|| format!("Failed to write '{}'", relative.display()))?;
        if declared {
            println!("✅ Updated {}", relative.display());
        } else {
            println!("✅ Declared `{}` in {}", decl, relative.display());
        }
    }
    Ok(())
}

/// Where a line the generator could not insert has to go by hand.
const MODULE_PLACEMENT: &str = "top-level item, next to the other `mod` declarations";
const MERGE_PLACEMENT: &str = "inside `async fn main`, after the Router is built";

/// Advice printed when `src/main.rs` cannot be wired automatically.
///
/// `krab gen` also runs inside the Krab framework workspace and against
/// projects scaffolded before the markers existed, where there is no safe place
/// to insert. Rather than guess — or rewrite a `main.rs` it did not generate —
/// the generator prints the exact lines and where each one goes.
fn render_manual_wiring(reason: &str, manual: &[(String, &str)]) -> String {
    let mut out = format!("👉 {reason}, so nothing was wired up. Add by hand:\n");
    for (line, placement) in manual {
        out.push_str(&format!("     {line}\n       ({placement})\n"));
    }
    out
}

/// Wire a generated module into the scaffolded `src/main.rs`.
///
/// `src/main.rs` belongs to the user. When it is missing, or carries none of
/// the markers `krab new` writes, nothing is written and the caller is told
/// exactly which lines to add. Insertion is line-based above a marker at
/// statement position, so the file is never rewritten wholesale and never
/// reformatted.
fn wire_into_main(root: &Path, dir: &str, merge: Option<&str>) -> Result<()> {
    let main_rs = root.join("src").join("main.rs");
    let Ok(original) = fs::read_to_string(&main_rs) else {
        print!(
            "{}",
            render_manual_wiring(
                "There is no src/main.rs here",
                &manual_wiring_lines(dir, merge)
            )
        );
        return Ok(());
    };

    let mut content = original.clone();
    let mut manual: Vec<(String, &str)> = Vec::new();

    let decl = format!("mod {dir};");
    let public_decl = format!("pub {decl}");
    if !has_line(&content, |line| line == decl || line == public_decl) {
        match insert_module_declaration(&content, MAIN_MODULES_MARKER, &decl) {
            Some(updated) => content = updated,
            None => manual.push((decl, MODULE_PLACEMENT)),
        }
    }

    if let Some(merge) = merge {
        if !content.contains(ROUTES_MERGE_CALL) {
            match insert_above_marker(&content, MAIN_ROUTES_MARKER, merge) {
                Some(updated) => content = updated,
                None => manual.push((merge.to_string(), MERGE_PLACEMENT)),
            }
        }
    }

    if content != original {
        fs::write(&main_rs, &content).context("Failed to write 'src/main.rs'")?;
        println!("✅ Wired into src/main.rs");
    }
    if !manual.is_empty() {
        print!(
            "{}",
            render_manual_wiring("src/main.rs carries no `// krab:` marker", &manual)
        );
    }
    Ok(())
}

/// The lines a user has to add when there is no `src/main.rs` to edit at all.
fn manual_wiring_lines(dir: &str, merge: Option<&str>) -> Vec<(String, &'static str)> {
    let mut lines = vec![(format!("mod {dir};"), MODULE_PLACEMENT)];
    if let Some(merge) = merge {
        lines.push((merge.to_string(), MERGE_PLACEMENT));
    }
    lines
}

fn generate_component(name: &str) -> Result<()> {
    generate_component_in(Path::new("."), name)
}

/// `root` is the project root. The public entry point passes `Path::new(".")`;
/// tests pass a `TempDir`. The generators take a root rather than resolving
/// against the working directory so the wiring can be tested without a
/// process-wide `set_current_dir`.
fn generate_component_in(root: &Path, name: &str) -> Result<()> {
    println!("🦀 Generating component '{}'...", name);
    let stem = name.to_lowercase();
    let relative = PathBuf::from(format!("src/components/{stem}.rs"));

    let outcome = write_new_file(&root.join(&relative), &render_component(name))?;
    report_file(outcome, "Component", name, &relative);

    ensure_module_index(root, "components", &stem, COMPONENTS_MOD_RS, None)?;
    wire_into_main(root, "components", None)
}

/// Render a plain (non-island) component.
///
/// Components are ordinary functions returning [`krab_core::Node`]; `view!`
/// builds the node tree. Interactive components additionally carry `#[island]`
/// and take a single serialisable props struct.
fn render_component(name: &str) -> String {
    format!(
        r#"use krab_core::Node;
use krab_macros::view;

/// Renders the `{name}` component.
#[allow(non_snake_case)]
pub fn {name}() -> Node {{
    view! {{
        <div class="{class}">
            "We are crabs"
        </div>
    }}
}}
"#,
        name = name,
        class = name.to_lowercase()
    )
}

fn generate_route(name: &str) -> Result<()> {
    generate_route_in(Path::new("."), name)
}

/// See [`generate_component_in`] for why this takes a root.
fn generate_route_in(root: &Path, name: &str) -> Result<()> {
    println!("🦀 Generating route '{}'...", name);
    let stem = name.to_lowercase();
    let relative = PathBuf::from(format!("src/routes/{stem}.rs"));

    let outcome = write_new_file(&root.join(&relative), &render_route(name))?;
    report_file(outcome, "Route", name, &relative);

    ensure_module_index(
        root,
        "routes",
        &stem,
        ROUTES_MOD_RS,
        Some(&route_registration(&stem)),
    )?;
    wire_into_main(root, "routes", Some(ROUTES_MERGE))
}

/// The statement `src/routes/mod.rs` collects for one generated route.
///
/// `get` and the module are in scope inside that file, so the call names
/// `<stem>::handler` rather than the crate-root path `routes::<stem>::handler`.
/// See [`ROUTES_MOD_RS`] for why this is a statement and not a chained call.
fn route_registration(stem: &str) -> String {
    format!("router = router.route(\"/{stem}\", get({stem}::handler));")
}

/// Render a route module.
///
/// The exported item is `pub async fn handler()` returning an Axum response,
/// which is what `krab gen route` registers: it adds
/// `router = router.route("/<stem>", get(<stem>::handler));` to the router in
/// `src/routes/mod.rs` and merges that router into the scaffold's `main.rs`.
///
/// A route module is *not* discovered by filesystem scanning. Only
/// `service_frontend` inside this workspace does that, from its own build.rs;
/// a `krab new` project has no build script, so registration is explicit.
fn render_route(name: &str) -> String {
    format!(
        r#"use axum::response::Html;
use krab_core::Render;
use krab_macros::view;

/// Handles `GET /{path}`.
pub async fn handler() -> Html<String> {{
    Html(
        view! {{
            <div>
                "Route: {name}"
            </div>
        }}
        .render(),
    )
}}
"#,
        path = name.to_lowercase(),
        name = name
    )
}

fn generate_server_function(name: &str) -> Result<()> {
    generate_server_function_in(Path::new("."), name)
}

/// See [`generate_component_in`] for why this takes a root.
fn generate_server_function_in(root: &Path, name: &str) -> Result<()> {
    println!("🦀 Generating server function '{}'...", name);
    let stem = name.to_lowercase();
    let relative = PathBuf::from(format!("src/server_functions/{stem}.rs"));

    let outcome = write_new_file(&root.join(&relative), &render_server_function(name))?;
    report_file(outcome, "Server function", name, &relative);

    ensure_module_index(
        root,
        "server_functions",
        &stem,
        SERVER_FUNCTIONS_MOD_RS,
        None,
    )?;
    wire_into_main(root, "server_functions", None)?;
    print!("{}", render_server_function_endpoint_hint(name, &stem));
    Ok(())
}

/// The one wiring step a server function still needs by hand.
///
/// `#[server]` expands to a `<name>_handler` Axum handler and, on wasm32,
/// rewrites the function body into a POST to `/api/rpc/<name>`. The module is
/// declared automatically, but which router that handler belongs on — and
/// under what state — is the user's decision, so an island calling the function
/// gets a 404 until it is mounted.
fn render_server_function_endpoint_hint(name: &str, stem: &str) -> String {
    format!(
        "👉 Mount the generated handler on your Router: `.route(\"/api/rpc/{name}\", post(server_functions::{stem}::{name}_handler))` — that path is what the client half of `#[server]` calls.\n"
    )
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
    use super::{
        enclosing_workspace_root, generate_service_in, render_service_next_step, ServiceRequest,
    };
    use super::{
        generate_component_in, generate_route_in, generate_server_function_in, protocol_feature,
        render_component, render_manual_wiring, render_route, render_server_function,
        render_server_function_endpoint_hint, render_service_manifest,
        render_split_adapter_manifest, route_registration, write_new_file, FileOutcome,
        DEFAULT_FEATURE, MERGE_PLACEMENT, MODULE_PLACEMENT,
    };
    use crate::project_template::DependencySource;
    use crate::{ExposureMode, ServiceType, Topology};
    use anyhow::Result;
    use clap::ValueEnum;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn dependencies_of(manifest: &str) -> toml::value::Table {
        let parsed: toml::Value = toml::from_str(manifest)
            .unwrap_or_else(|e| panic!("generated manifest is not valid TOML: {e}\n{manifest}"));
        parsed
            .get("dependencies")
            .and_then(|d| d.as_table())
            .expect("generated manifest has no [dependencies]")
            .clone()
    }

    /// The generated `main.rs` uses `#[async_trait]`, but the manifest never
    /// declared the crate, and `krab_core` pointed at `../krab_core` — a path
    /// that has not existed since the crates/ reorganisation. Neither output
    /// could compile.
    #[test]
    fn service_manifest_parses_declares_async_trait_and_registry_krab_core() {
        let manifest = render_service_manifest(
            "demo_service",
            &["rest", "graphql"],
            &DependencySource::Registry,
        );
        let deps = dependencies_of(&manifest);

        assert!(
            deps.contains_key("async-trait"),
            "generated main.rs uses #[async_trait]; the manifest must declare async-trait"
        );

        let core = deps.get("krab_core").expect("krab_core is a dependency");
        assert!(
            core.get("path").is_none(),
            "krab_core must not be a path dependency: '../krab_core' does not exist \
             relative to a generated service"
        );
        assert_eq!(
            core.get("version").and_then(|v| v.as_str()),
            Some(crate::project_template::FRAMEWORK_VERSION),
            "krab_core must resolve from the registry at the workspace version"
        );
        let features: Vec<&str> = core
            .get("features")
            .and_then(|f| f.as_array())
            .expect("krab_core carries a feature list")
            .iter()
            .filter_map(|f| f.as_str())
            .collect();
        assert_eq!(features, vec!["rest", "graphql"]);
    }

    /// Feature names `krab_core` actually declares, read from its manifest so a
    /// rename or removal fails this suite instead of shipping a generator that
    /// scaffolds unresolvable projects.
    fn krab_core_features() -> Vec<String> {
        let krab_core_manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../framework/krab_core/Cargo.toml")
            .canonicalize()
            .expect("krab_core manifest not found");
        let parsed: toml::Value = toml::from_str(
            &std::fs::read_to_string(krab_core_manifest).expect("krab_core manifest unreadable"),
        )
        .expect("krab_core manifest is not valid TOML");
        parsed
            .get("features")
            .and_then(|f| f.as_table())
            .expect("krab_core declares no [features]")
            .keys()
            .cloned()
            .collect()
    }

    /// Former `krab_core` feature aliases: `grpc` -> `grpc-semantics` and
    /// `db` -> `db-postgres`, removed in 0.6.0. The declared-features check
    /// below now catches them on its own, since they are no longer declared;
    /// this list stays as a named guard so a reintroduced alias fails with a
    /// message that says why. `ServiceType::Grpc` emitted `grpc` until 0.4.0.
    const DEPRECATED_FEATURE_ALIASES: &[&str] = &["grpc", "db"];

    /// Every `krab_core` feature `generate_service` can put in a manifest.
    ///
    /// Enumerated from the `ServiceType` variants rather than a literal list,
    /// so a new `--type` is covered the moment it is added.
    fn features_generator_can_emit() -> Vec<&'static str> {
        let mut features: Vec<&'static str> = ServiceType::value_variants()
            .iter()
            .map(protocol_feature)
            .collect();
        // The empty-protocol fallback in generate_service.
        features.push(DEFAULT_FEATURE);
        features.sort_unstable();
        features.dedup();
        features
    }

    /// Every feature the service generator can request must exist in
    /// `krab_core`'s manifest, and none of them may be a deprecated alias.
    #[test]
    fn service_manifest_features_exist_in_krab_core() {
        let declared = krab_core_features();
        let emitted = features_generator_can_emit();

        for feature in &emitted {
            assert!(
                declared.iter().any(|d| d == feature),
                "generator can request krab_core feature {feature:?}, which krab_core does \
                 not declare. Declared: {declared:?}"
            );
            assert!(
                !DEPRECATED_FEATURE_ALIASES.contains(feature),
                "generator emits deprecated krab_core alias {feature:?}; scaffold against the \
                 canonical feature instead so removing the alias cannot break generated projects"
            );
        }
    }

    /// `grpc-semantics` is the canonical name (ADR 0007); `rest` covers RPC
    /// because Krab has no separate RPC feature.
    #[test]
    fn protocol_features_use_canonical_names() {
        assert_eq!(protocol_feature(&ServiceType::Rest), "rest");
        assert_eq!(protocol_feature(&ServiceType::Graphql), "graphql");
        assert_eq!(protocol_feature(&ServiceType::Grpc), "grpc-semantics");
        assert_eq!(protocol_feature(&ServiceType::Rpc), "rest");
    }

    /// Re-running `krab gen component Counter` replaced a hand-edited
    /// `src/components/counter.rs` with boilerplate and still printed a success
    /// line. The generated source is now never overwritten — but the re-run is
    /// reported, not failed, because the wiring steps that follow it are how a
    /// user repairs a `mod components;` line they deleted.
    #[test]
    fn write_new_file_keeps_an_existing_file_instead_of_clobbering_it() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("src/components/counter.rs");

        assert_eq!(
            write_new_file(&path, "generated\n").expect("first write creates the file"),
            FileOutcome::Created
        );

        fs::write(&path, "hand written\n").expect("simulate a user edit");
        assert_eq!(
            write_new_file(&path, "generated\n").expect("a re-run is not an error"),
            FileOutcome::Kept
        );
        assert_eq!(
            fs::read_to_string(&path).expect("file still readable"),
            "hand written\n",
            "the user's content must survive the second write"
        );
    }

    #[test]
    fn write_new_file_creates_missing_parent_directories() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("src/server_functions/load_user.rs");

        write_new_file(&path, "contents\n").expect("parents are created on demand");

        assert_eq!(
            fs::read_to_string(&path).expect("file written"),
            "contents\n"
        );
    }

    /// The endpoint has to match what `#[server]` compiles into the client half
    /// of the function — `/api/rpc/<fn name>`, keeping the function's own
    /// casing, not the lowercased file stem. This is the one step the generator
    /// cannot take for the user: which router the handler belongs on, and under
    /// what state, is a design decision.
    #[test]
    fn server_function_hint_mounts_the_endpoint_the_macro_calls() {
        let hint = render_server_function_endpoint_hint("load_user", "load_user");

        assert!(hint.contains("/api/rpc/load_user"));
        assert!(hint.contains("load_user_handler"));
        assert!(hint.contains("server_functions::load_user::load_user_handler"));
    }

    // ---------------------------------------------------------------------
    // Module-tree wiring.
    //
    // `krab gen route about` wrote src/routes/about.rs and stopped. A project
    // scaffolded by `krab new` has no build.rs and no `mod routes;`, so the
    // file was never compiled and the route silently did not exist.
    // ---------------------------------------------------------------------

    /// The `src/main.rs` a `krab new` scaffold emits, reduced to the parts the
    /// wiring depends on: the two marker comments, at the indentation the
    /// template puts them at.
    const SCAFFOLD_MAIN: &str = r#"use axum::routing::get;
use axum::{Json, Router};

// krab:modules

async fn index() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/", get(index));
    // krab:routes

    println!("{app:?}");
}
"#;

    fn project(main_rs: Option<&str>) -> Result<TempDir> {
        let temp = TempDir::new()?;
        fs::create_dir_all(temp.path().join("src"))?;
        if let Some(main_rs) = main_rs {
            fs::write(temp.path().join("src/main.rs"), main_rs)?;
        }
        Ok(temp)
    }

    fn read(root: &Path, relative: &str) -> String {
        fs::read_to_string(root.join(relative))
            .unwrap_or_else(|e| panic!("{relative} unreadable: {e}"))
    }

    fn occurrences(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    #[test]
    fn gen_route_creates_a_module_index_that_collects_the_handler() -> Result<()> {
        let temp = project(Some(SCAFFOLD_MAIN))?;
        generate_route_in(temp.path(), "About")?;

        let mod_rs = read(temp.path(), "src/routes/mod.rs");
        assert!(mod_rs.contains("pub mod about;"), "{mod_rs}");
        assert!(
            mod_rs.contains("router = router.route(\"/about\", get(about::handler));"),
            "{mod_rs}"
        );
        // Generic over the state type: the default template merges into a
        // `Router` and the saas template into a `Router<AppState>`.
        assert!(
            mod_rs.contains("pub fn router<S>() -> Router<S>"),
            "{mod_rs}"
        );
        assert!(
            mod_rs.contains("S: Clone + Send + Sync + 'static,"),
            "{mod_rs}"
        );
        // `use axum::routing::get;` would be an unused import — and the
        // generated CI builds with `-D warnings` — if the registration were
        // not inserted in the same run that creates the file.
        assert!(mod_rs.contains("use axum::routing::get;"), "{mod_rs}");

        let main_rs = read(temp.path(), "src/main.rs");
        assert!(main_rs.contains("\nmod routes;\n"), "{main_rs}");
        assert!(
            main_rs.contains("    let app = app.merge(routes::router());\n"),
            "{main_rs}"
        );
        Ok(())
    }

    /// Insertions go immediately above the markers, which stay in place so the
    /// next `krab gen` finds them.
    #[test]
    fn gen_route_inserts_above_the_markers_and_keeps_them() -> Result<()> {
        let temp = project(Some(SCAFFOLD_MAIN))?;
        generate_route_in(temp.path(), "about")?;

        let main_rs = read(temp.path(), "src/main.rs");
        assert!(
            main_rs.contains("mod routes;\n// krab:modules"),
            "{main_rs}"
        );
        assert!(
            main_rs.contains("    let app = app.merge(routes::router());\n    // krab:routes"),
            "{main_rs}"
        );

        let mod_rs = read(temp.path(), "src/routes/mod.rs");
        assert!(
            mod_rs.contains(
                "    router = router.route(\"/about\", get(about::handler));\n    \
                 // krab:route-registrations"
            ),
            "{mod_rs}"
        );
        Ok(())
    }

    /// Running a generator twice must be a no-op, not a duplicate and not an
    /// error: users re-run them.
    #[test]
    fn gen_route_is_idempotent() -> Result<()> {
        let temp = project(Some(SCAFFOLD_MAIN))?;
        generate_route_in(temp.path(), "About")?;

        let main_after_first = read(temp.path(), "src/main.rs");
        let mod_after_first = read(temp.path(), "src/routes/mod.rs");
        let route_after_first = read(temp.path(), "src/routes/about.rs");

        generate_route_in(temp.path(), "About").expect("a second run must not error");

        assert_eq!(read(temp.path(), "src/main.rs"), main_after_first);
        assert_eq!(read(temp.path(), "src/routes/mod.rs"), mod_after_first);
        assert_eq!(read(temp.path(), "src/routes/about.rs"), route_after_first);
        assert_eq!(occurrences(&main_after_first, "mod routes;"), 1);
        assert_eq!(occurrences(&mod_after_first, "pub mod about;"), 1);
        Ok(())
    }

    /// A second route stacks onto the same module index and needs no second
    /// merge in `main.rs`.
    #[test]
    fn a_second_route_extends_the_same_index_and_merge() -> Result<()> {
        let temp = project(Some(SCAFFOLD_MAIN))?;
        generate_route_in(temp.path(), "about")?;
        generate_route_in(temp.path(), "contact")?;

        let mod_rs = read(temp.path(), "src/routes/mod.rs");
        assert!(mod_rs.contains("pub mod about;"), "{mod_rs}");
        assert!(mod_rs.contains("pub mod contact;"), "{mod_rs}");
        assert!(
            mod_rs.contains("router = router.route(\"/contact\", get(contact::handler));"),
            "{mod_rs}"
        );

        let main_rs = read(temp.path(), "src/main.rs");
        assert_eq!(occurrences(&main_rs, "routes::router()"), 1);
        assert_eq!(occurrences(&main_rs, "mod routes;"), 1);
        Ok(())
    }

    /// The scaffolded CI builds with `-D warnings`, and nothing calls a freshly
    /// generated component or server function, so `dead_code` would fail the
    /// build the generator just produced.
    #[test]
    fn component_and_server_function_indexes_allow_dead_code() -> Result<()> {
        let temp = project(Some(SCAFFOLD_MAIN))?;
        generate_component_in(temp.path(), "Counter")?;
        generate_server_function_in(temp.path(), "load_user")?;

        for (relative, decl) in [
            ("src/components/mod.rs", "pub mod counter;"),
            ("src/server_functions/mod.rs", "pub mod load_user;"),
        ] {
            let mod_rs = read(temp.path(), relative);
            assert!(
                mod_rs.starts_with("// Generated by `krab gen "),
                "{relative} must say why the allow is there:\n{mod_rs}"
            );
            assert!(
                mod_rs.contains("#![allow(dead_code)]"),
                "{relative}:\n{mod_rs}"
            );
            assert!(mod_rs.contains(decl), "{relative}:\n{mod_rs}");
        }

        let main_rs = read(temp.path(), "src/main.rs");
        assert!(main_rs.contains("\nmod components;\n"), "{main_rs}");
        assert!(main_rs.contains("\nmod server_functions;\n"), "{main_rs}");
        // Only routes merge a router; the other two are declarations only.
        assert!(!main_rs.contains("components::router()"), "{main_rs}");
        Ok(())
    }

    #[test]
    fn gen_component_is_idempotent() -> Result<()> {
        let temp = project(Some(SCAFFOLD_MAIN))?;
        generate_component_in(temp.path(), "Counter")?;
        let after_first = read(temp.path(), "src/main.rs");

        generate_component_in(temp.path(), "Counter").expect("a second run must not error");

        assert_eq!(read(temp.path(), "src/main.rs"), after_first);
        assert_eq!(
            occurrences(
                &read(temp.path(), "src/components/mod.rs"),
                "pub mod counter;"
            ),
            1
        );
        Ok(())
    }

    /// `krab gen` also runs inside the Krab framework workspace, whose
    /// `src/main.rs` files carry no markers. The file must come out byte for
    /// byte unchanged and the command must still succeed.
    #[test]
    fn a_main_without_markers_is_never_rewritten() -> Result<()> {
        const FOREIGN_MAIN: &str = "fn main() {\n    println!(\"not a krab scaffold\");\n}\n";
        let temp = project(Some(FOREIGN_MAIN))?;

        generate_route_in(temp.path(), "about").expect("must degrade, not fail");

        assert_eq!(
            read(temp.path(), "src/main.rs"),
            FOREIGN_MAIN,
            "a main.rs the generator did not write must not be touched"
        );
        // The source file and its index are still produced — only the crate
        // root is off limits.
        assert!(temp.path().join("src/routes/about.rs").exists());
        assert!(read(temp.path(), "src/routes/mod.rs").contains("pub mod about;"));
        Ok(())
    }

    /// No `src/main.rs` at all — a library crate, or a directory that is not a
    /// project root.
    #[test]
    fn a_missing_main_degrades_instead_of_failing() -> Result<()> {
        let temp = project(None)?;

        generate_route_in(temp.path(), "about").expect("must degrade, not fail");

        assert!(!temp.path().join("src/main.rs").exists());
        assert!(temp.path().join("src/routes/about.rs").exists());
        Ok(())
    }

    /// rustfmt's `reorder_modules` sorts a consecutive run of `mod` items, so
    /// declarations added in generation order would leave the project failing
    /// the `cargo fmt --all --check` its own CI runs.
    #[test]
    fn declarations_are_inserted_alphabetically() -> Result<()> {
        let temp = project(Some(SCAFFOLD_MAIN))?;
        // Deliberately reverse-alphabetical generation order.
        generate_server_function_in(temp.path(), "load_user")?;
        generate_route_in(temp.path(), "zebra")?;
        generate_route_in(temp.path(), "about")?;
        generate_component_in(temp.path(), "Counter")?;

        let main_rs = read(temp.path(), "src/main.rs");
        assert!(
            main_rs
                .contains("mod components;\nmod routes;\nmod server_functions;\n// krab:modules"),
            "{main_rs}"
        );

        let mod_rs = read(temp.path(), "src/routes/mod.rs");
        assert!(
            mod_rs.contains("pub mod about;\npub mod zebra;\n"),
            "{mod_rs}"
        );
        Ok(())
    }

    /// Everything above is a substring assertion, and substring assertions are
    /// how the chain-collapse and module-reordering hazards got past review in
    /// the first place. Only rustfmt settles whether generated output survives
    /// the `cargo fmt --all --check` the scaffolded CI runs.
    #[test]
    fn generated_output_is_rustfmt_clean() -> Result<()> {
        let available = std::process::Command::new("rustfmt")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if !available {
            eprintln!("rustfmt is not on PATH; skipping the formatting gate");
            return Ok(());
        }

        let temp = project(Some(SCAFFOLD_MAIN))?;
        generate_route_in(temp.path(), "about")?;
        generate_component_in(temp.path(), "Counter")?;
        generate_server_function_in(temp.path(), "load_user")?;
        // A second route: one route and several routes format differently.
        generate_route_in(temp.path(), "contact")?;

        // `src/main.rs` pulls in every module the generators declared, so this
        // covers each generated file plus the wiring inserted into main.
        let output = std::process::Command::new("rustfmt")
            .args(["--edition", "2021", "--check"])
            .arg(temp.path().join("src/main.rs"))
            .output()?;
        assert!(
            output.status.success() && output.stdout.is_empty(),
            "generated output is not rustfmt-clean:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    /// The degradation path is only useful if it prints the lines verbatim.
    #[test]
    fn manual_wiring_advice_names_the_exact_lines_and_where_they_go() {
        let advice = render_manual_wiring(
            "src/main.rs carries no `// krab:` markers",
            &[
                ("mod routes;".to_string(), MODULE_PLACEMENT),
                (
                    "let app = app.merge(routes::router());".to_string(),
                    MERGE_PLACEMENT,
                ),
            ],
        );

        assert!(advice.contains("mod routes;"), "{advice}");
        assert!(
            advice.contains("let app = app.merge(routes::router());"),
            "{advice}"
        );
        assert!(advice.contains("top-level item"), "{advice}");
        assert!(advice.contains("inside `async fn main`"), "{advice}");
    }

    /// The registration is written into `src/routes/mod.rs`, where `get` and
    /// the route module are already in scope — so it must not be spelled with
    /// the crate-root path a `main.rs` would need.
    ///
    /// It is a statement, not a chained `.route(..)`: rustfmt collapses a
    /// one-element chain onto a single line, which is exactly what the first
    /// `krab gen route` produces, and the generated project's CI runs
    /// `cargo fmt --all --check`.
    #[test]
    fn route_registration_is_a_statement_relative_to_the_routes_module() {
        let registration = route_registration("about");

        assert_eq!(
            registration,
            "router = router.route(\"/about\", get(about::handler));"
        );
        assert!(!registration.contains("routes::about"));
        assert!(
            !registration.trim_start().starts_with('.'),
            "a chained call is not rustfmt-stable at one route: {registration}"
        );
    }

    #[test]
    fn split_adapter_manifest_parses_and_uses_registry_krab_core() {
        let manifest = render_split_adapter_manifest(
            "users-rest",
            "users-domain",
            &DependencySource::Registry,
        );
        let deps = dependencies_of(&manifest);

        let core = deps.get("krab_core").expect("krab_core is a dependency");
        assert!(core.get("path").is_none());
        assert_eq!(
            core.get("version").and_then(|v| v.as_str()),
            Some(crate::project_template::FRAMEWORK_VERSION)
        );

        // The shared domain crate is generated alongside, so a sibling path
        // dependency is correct there.
        let domain = deps.get("users-domain").expect("domain dep present");
        assert_eq!(
            domain.get("path").and_then(|p| p.as_str()),
            Some("../users-domain")
        );
    }

    // ---------------------------------------------------------------------
    // `krab gen service`.
    // ---------------------------------------------------------------------

    fn gen_service(
        root: &Path,
        name: &str,
        exposure_mode: ExposureMode,
        protocols: Option<Vec<ServiceType>>,
        deps: &DependencySource,
    ) -> Result<()> {
        generate_service_in(
            root,
            &ServiceRequest {
                name,
                service_type: &ServiceType::Rest,
                exposure_mode: &exposure_mode,
                protocols: &protocols,
                topology: &Topology::SingleService,
                deps,
            },
        )
    }

    fn rustfmt_available() -> bool {
        std::process::Command::new("rustfmt")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// Multi mode wrote `capabilities.rs`, `domain/` and `adapters/` and
    /// declared none of them, so none of it was ever compiled. Every file must
    /// now be reachable from `main.rs`.
    #[test]
    fn multi_mode_declares_and_calls_every_generated_module() -> Result<()> {
        let temp = TempDir::new()?;
        gen_service(
            temp.path(),
            "billing",
            ExposureMode::Multi,
            Some(vec![ServiceType::Rest, ServiceType::Graphql]),
            &DependencySource::Registry,
        )?;
        let root = temp.path().join("billing");

        let main_rs = read(&root, "src/main.rs");
        for decl in ["mod adapters;", "mod capabilities;", "mod domain;"] {
            assert!(
                main_rs.lines().any(|line| line.trim() == decl),
                "src/main.rs must declare `{decl}`:\n{main_rs}"
            );
        }
        assert!(
            main_rs.contains("capabilities::build_capabilities()"),
            "{main_rs}"
        );

        let adapters_mod = read(&root, "src/adapters/mod.rs");
        assert_eq!(adapters_mod, "pub mod graphql;\npub mod rest;\n");
        let capabilities = read(&root, "src/capabilities.rs");
        for module in ["rest", "graphql"] {
            assert!(
                capabilities.contains(&format!("adapters::{module}::capability(&service)")),
                "{capabilities}"
            );
            assert!(root.join(format!("src/adapters/{module}.rs")).is_file());
        }
        // Every domain item is used by an adapter, so `-D warnings` has no
        // dead code to reject.
        let adapter = read(&root, "src/adapters/rest.rs");
        assert!(
            adapter.contains("service.describe(&DomainModel)"),
            "{adapter}"
        );
        Ok(())
    }

    /// Single mode must not grow module declarations for files it never
    /// writes.
    #[test]
    fn single_mode_declares_no_modules() -> Result<()> {
        let temp = TempDir::new()?;
        gen_service(
            temp.path(),
            "payments",
            ExposureMode::Single,
            None,
            &DependencySource::Registry,
        )?;
        let root = temp.path().join("payments");

        let main_rs = read(&root, "src/main.rs");
        assert!(!main_rs.contains("mod "), "{main_rs}");
        assert!(!root.join("src/capabilities.rs").exists());
        assert!(
            main_rs.contains("println!(\"protocols=rest\");"),
            "{main_rs}"
        );
        Ok(())
    }

    /// The generated-project gate runs `cargo fmt --check` over a generated
    /// service, so the multi-mode tree has to come out rustfmt-clean for one
    /// adapter and for several (`vec!` formats differently for each).
    #[test]
    fn generated_services_are_rustfmt_clean() -> Result<()> {
        if !rustfmt_available() {
            eprintln!("rustfmt is not on PATH; skipping the formatting gate");
            return Ok(());
        }
        let temp = TempDir::new()?;
        for (name, mode, protocols) in [
            ("single_svc", ExposureMode::Single, None),
            (
                "multi_one",
                ExposureMode::Multi,
                Some(vec![ServiceType::Rest]),
            ),
            (
                "multi_all",
                ExposureMode::Multi,
                Some(vec![
                    ServiceType::Rest,
                    ServiceType::Graphql,
                    ServiceType::Rpc,
                    ServiceType::Grpc,
                ]),
            ),
        ] {
            gen_service(
                temp.path(),
                name,
                mode,
                protocols,
                &DependencySource::Registry,
            )?;
            let output = std::process::Command::new("rustfmt")
                .args(["--edition", "2021", "--check"])
                .arg(temp.path().join(name).join("src/main.rs"))
                .output()?;
            assert!(
                output.status.success() && output.stdout.is_empty(),
                "{name} is not rustfmt-clean:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    /// `--path-deps` reaches the service manifest the same way it reaches
    /// `krab new`'s: an absolute path into the checkout, no version.
    #[test]
    fn path_deps_point_the_service_manifest_at_the_checkout() -> Result<()> {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()?;
        let deps = DependencySource::from_path_deps(Some(&repo_root))?;
        let temp = TempDir::new()?;
        gen_service(temp.path(), "svc", ExposureMode::Single, None, &deps)?;

        let manifest = read(temp.path(), "svc/Cargo.toml");
        let deps_table = dependencies_of(&manifest);
        let core = deps_table.get("krab_core").expect("krab_core dependency");
        assert!(core.get("version").is_none(), "{manifest}");
        let path = core
            .get("path")
            .and_then(|p| p.as_str())
            .expect("krab_core is a path dependency");
        assert!(
            path.ends_with("crates/framework/krab_core"),
            "{path} should point into the checkout"
        );
        assert!(
            Path::new(path).join("Cargo.toml").is_file(),
            "{path} must resolve"
        );
        assert!(!path.contains('\\'), "TOML-hostile separator in {path}");
        Ok(())
    }

    #[test]
    fn path_deps_reject_a_directory_that_is_not_a_krab_checkout() {
        let temp = TempDir::new().expect("tempdir");
        let err = DependencySource::from_path_deps(Some(temp.path()))
            .expect_err("an empty directory is not a checkout");
        assert!(err.to_string().contains("not a Krab checkout"), "{err}");
    }

    /// No `[workspace]` anywhere above: there is no members list to add to,
    /// and the hint used to say there was.
    #[test]
    fn next_step_outside_a_workspace_says_the_crate_is_standalone() -> Result<()> {
        let temp = TempDir::new()?;
        // A `krab new` project root: a package manifest, no [workspace].
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )?;

        assert_eq!(enclosing_workspace_root(temp.path()), None);
        let hint = render_service_next_step(temp.path(), &["svc".to_string()]);
        assert!(!hint.contains("members"), "{hint}");
        assert!(hint.contains("cd svc && cargo build"), "{hint}");
        Ok(())
    }

    /// Inside a workspace the hint names the exact member entry, relative to
    /// the workspace root rather than the directory the command ran in.
    #[test]
    fn next_step_inside_a_workspace_names_the_member_path() -> Result<()> {
        let temp = TempDir::new()?;
        fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = []\nresolver = \"2\"\n",
        )?;
        let services = temp.path().join("services");
        fs::create_dir_all(&services)?;

        assert_eq!(
            enclosing_workspace_root(&services),
            Some(temp.path().canonicalize()?)
        );
        let hint = render_service_next_step(&services, &["svc".to_string()]);
        assert!(hint.contains("\"services/svc\""), "{hint}");
        assert!(hint.contains("[workspace] members"), "{hint}");
        Ok(())
    }

    /// Items that do not exist in Krab. Earlier templates were written against
    /// another framework's API and generated code that could not compile.
    const PHANTOM_API: &[&str] = &[
        "krab_core::prelude",
        "IntoView",
        "impl View",
        "#[component]",
        "#[route(",
    ];

    fn assert_no_phantom_api(rendered: &str) {
        for item in PHANTOM_API {
            assert!(
                !rendered.contains(item),
                "generated code references `{item}`, which does not exist in Krab:\n{rendered}"
            );
        }
    }

    #[test]
    fn server_function_generator_uses_supported_macro_contract() {
        let rendered = render_server_function("load_user");

        assert!(rendered.contains("#[server]"));
        assert!(!rendered.contains("endpoint ="));
        assert!(rendered.contains("validate_server_fn"));
        assert!(rendered.contains("Result<String, ServerFnError>"));
        assert_no_phantom_api(&rendered);
    }

    #[test]
    fn component_generator_uses_supported_node_contract() {
        let rendered = render_component("Counter");

        assert!(rendered.contains("use krab_core::Node;"));
        assert!(rendered.contains("use krab_macros::view;"));
        assert!(rendered.contains("pub fn Counter() -> Node {"));
        assert!(rendered.contains("view! {"));
        assert!(rendered.contains(r#"class="counter""#));
        assert_no_phantom_api(&rendered);
    }

    #[test]
    fn route_generator_matches_build_script_discovery_contract() {
        let rendered = render_route("About");

        // build.rs registers `<module>::handler` for every src/routes/*.rs.
        assert!(rendered.contains("pub async fn handler() -> Html<String> {"));
        assert!(rendered.contains("use krab_core::Render;"));
        assert!(rendered.contains(".render(),"));
        assert!(rendered.contains("GET /about"));
        assert_no_phantom_api(&rendered);
    }
}
