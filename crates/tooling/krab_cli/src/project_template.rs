use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

use crate::ProjectTemplate;

/// Version requirement written into generated `Cargo.toml` files.
///
/// Derived from the CLI's own package version, which is the workspace version,
/// so a `krab` release always scaffolds against the matching `krab_core`. This
/// was previously a hard-coded `"0.1.0"` literal that silently fell a version
/// behind the workspace and produced projects that could not resolve.
pub(crate) const FRAMEWORK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How a generated project should depend on the framework crates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DependencySource {
    /// Resolve from crates.io at [`FRAMEWORK_VERSION`]. The default, and what a
    /// real user gets.
    Registry,
    /// Resolve from a local checkout of this repository. Used by the
    /// `generated-project` CI gate, which must build scaffolded output before
    /// the corresponding version exists on crates.io — and by anyone testing a
    /// framework change against a fresh project.
    Path(PathBuf),
}

/// Render a filesystem path for embedding in a `Cargo.toml` string.
///
/// Two Windows-specific hazards, both of which produce a manifest Cargo
/// refuses to parse:
///
/// - `Path::canonicalize` returns a verbatim path (`\\?\C:\...`). Cargo rejects
///   it with `invalid path url`, so the prefix is stripped.
/// - A backslash is an invalid escape inside a TOML basic string, so separators
///   are normalised to `/`. Cargo accepts forward slashes on every platform.
fn path_for_toml(path: &Path) -> String {
    let text = path.to_string_lossy();
    let stripped = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| text.strip_prefix(r"\\?\").map(str::to_string))
        .unwrap_or_else(|| text.to_string());
    stripped.replace('\\', "/")
}

impl DependencySource {
    /// Render one dependency line, e.g. `krab_core = { ... }`.
    ///
    /// `crate_dir` is the crate's location relative to the repository root; it
    /// is only consulted for [`DependencySource::Path`].
    fn render(&self, crate_name: &str, crate_dir: &str, features: &[&str]) -> String {
        let features = if features.is_empty() {
            String::new()
        } else {
            let list = features
                .iter()
                .map(|f| format!("\"{f}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!(", features = [{list}]")
        };

        match self {
            DependencySource::Registry => {
                format!("{crate_name} = {{ version = \"{FRAMEWORK_VERSION}\"{features} }}")
            }
            DependencySource::Path(root) => {
                let path = path_for_toml(&root.join(crate_dir));
                format!("{crate_name} = {{ path = \"{path}\"{features} }}")
            }
        }
    }
}

#[derive(Clone, Copy)]
struct TemplateMetadata {
    flag: &'static str,
    description: &'static str,
    starter_scope: Option<&'static str>,
    axum_dep: &'static str,
    extra_deps: &'static str,
    /// Cargo features to enable on `krab_core`, one per element.
    ///
    /// This was a single comma-joined string interpolated straight into
    /// `features = ["{...}"]`, which produced `features = ["db, rest"]` — one
    /// feature literally named `db, rest`, which does not exist. Every `saas`
    /// project ever scaffolded failed to resolve.
    extra_features: &'static [&'static str],
}

fn template_metadata(template: &ProjectTemplate) -> TemplateMetadata {
    match template {
        ProjectTemplate::Saas => TemplateMetadata {
            flag: "saas",
            description: "SaaS service skeleton with auth-ready layers and tenant API scaffolding",
            starter_scope: Some(
                "This starter provides auth-ready HTTP layers, tenant API scaffolding, and release-aware defaults. It does not include a completed auth flow, database schema, or multi-tenant persistence model.",
            ),
            axum_dep: r#"axum = "0.8"
"#,
            extra_deps: r#"sqlx = { version = "0.8", features = ["runtime-tokio-rustls", "postgres"] }
argon2 = "0.5"
jsonwebtoken = "9.0"
"#,
            // `db-postgres`, not the deprecated `db` alias — a scaffolded
            // project should start on the name that will still exist at 0.2.0.
            extra_features: &["db-postgres", "rest"],
        },
        ProjectTemplate::EdgeSsr => TemplateMetadata {
            flag: "edge-ssr",
            description: "Edge SSR policy skeleton with explicit SSR, ISR, and streaming metadata",
            starter_scope: Some(
                "This starter wires route render policy, edge eligibility metadata, and stale-while-revalidate ISR serving on `/`. It does not ship streamed SSR output out of the box.",
            ),
            axum_dep: r#"axum = "0.8"
"#,
            extra_deps: "",
            extra_features: &["rest"],
        },
        ProjectTemplate::EventStream => TemplateMetadata {
            flag: "event-stream",
            description: "Event-stream dashboard with WebSocket and SSE",
            starter_scope: None,
            axum_dep: r#"axum = { version = "0.8", features = ["ws"] }
"#,
            extra_deps: r#"tokio-stream = "0.1"
futures-util = "0.3"
"#,
            extra_features: &["rest"],
        },
        ProjectTemplate::Default => TemplateMetadata {
            flag: "default",
            description: "Minimal Krab service",
            starter_scope: None,
            axum_dep: r#"axum = "0.8"
"#,
            extra_deps: "",
            extra_features: &["rest"],
        },
    }
}

fn generate_readme(name: &str, metadata: TemplateMetadata) -> String {
    let starter_scope = metadata
        .starter_scope
        .map(|note| format!("## Starter Scope\n\n{}\n\n", note))
        .unwrap_or_default();

    format!(
        r#"# {name}

> {template_desc}

Generated with `krab new {name} --template {template_flag}`.

{starter_scope}## Quick Start

```bash
cp .env.example .env
cargo run
```

## Development

```bash
krab dev --watch
krab doctor --diagnostics
```

## Testing

```bash
cargo test
```

> Note: the `krab` governance commands (`release certify`, `contract check`,
> `db lifecycle`) currently operate on the Krab framework workspace itself —
> they are hardcoded to its internal services and are not wired to generated
> projects. Use your project's own CI (see `.github/workflows/ci.yaml`) as the
> release gate.

## Deployment

```bash
docker build -t {name} .
kubectl apply -f deploy/kubernetes.yaml
```
"#,
        template_desc = metadata.description,
        template_flag = metadata.flag,
    )
}

fn write_project_from_template(
    path: &Path,
    name: &str,
    template: &ProjectTemplate,
    deps: &DependencySource,
) -> Result<()> {
    if path.exists() {
        anyhow::bail!("Directory '{}' already exists", path.display());
    }
    let metadata = template_metadata(template);

    /*
    ///
    /// The generator writes a minimal but release-aware project skeleton including source layout,
    /// environment template, CI workflow, deployment manifest, Dockerfile, and README.
    pub(super) fn generate_project_from_template(name: &str, template: &ProjectTemplate) -> Result<()> {
        println!(
            "🦀 Scaffolding new Krab project '{}' (template: {:?})...",
            name, template
        );
        let path = PathBuf::from(name);
        if path.exists() {
            anyhow::bail!("Directory '{}' already exists", name);
        }
    */

    for dir in [
        "",
        "src",
        "src/routes",
        "src/api",
        "public",
        ".github/workflows",
        "deploy",
        "docs",
    ] {
        fs::create_dir_all(path.join(dir))?;
    }

    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
description = "{template_desc}"

[dependencies]
{krab_core_dep}
{krab_macros_dep}
tokio = {{ version = "1.0", features = ["full"] }}
{axum_dep}serde = {{ version = "1.0", features = ["derive"] }}
serde_json = "1.0"
tracing = "0.1"
tracing-subscriber = {{ version = "0.3", features = ["json", "env-filter"] }}
{extra_deps}
"#,
        template_desc = metadata.description,
        krab_core_dep = deps.render(
            "krab_core",
            "crates/framework/krab_core",
            metadata.extra_features
        ),
        // `view!`, `#[island]`, and `#[server]` are the framework's headline
        // features and live in `krab_macros`. It was absent from every
        // generated manifest, so a scaffolded project could not use any of
        // them without the user working out the dependency themselves.
        krab_macros_dep = deps.render("krab_macros", "crates/framework/krab_macros", &[]),
        axum_dep = metadata.axum_dep,
        extra_deps = metadata.extra_deps
    );
    fs::write(path.join("Cargo.toml"), cargo_toml)?;

    let main_rs = match template {
        ProjectTemplate::Saas => generate_saas_main(name),
        ProjectTemplate::EdgeSsr => generate_edge_ssr_main(name),
        ProjectTemplate::EventStream => generate_event_stream_main(name),
        ProjectTemplate::Default => generate_default_main(name),
    };
    fs::write(path.join("src/main.rs"), main_rs)?;

    let env_example = match template {
        ProjectTemplate::Saas => format!(
            r#"# {name} Environment Configuration
KRAB_ENVIRONMENT=dev
KRAB_HOST=0.0.0.0
KRAB_PORT=3000
KRAB_AUTH_MODE=jwt
KRAB_JWT_SECRET=change-me-in-production
KRAB_OIDC_ISSUER=https://auth.example.com
KRAB_OIDC_AUDIENCE={name}
DATABASE_URL=postgres://localhost:5432/{name}
KRAB_SECRETS_SOURCE=env
"#
        ),
        _ => format!(
            r#"# {name} Environment Configuration
KRAB_ENVIRONMENT=dev
KRAB_HOST=0.0.0.0
KRAB_PORT=3000
KRAB_AUTH_MODE=static
KRAB_SECRETS_SOURCE=env
"#
        ),
    };
    fs::write(path.join(".env.example"), env_example)?;

    let project_toml = generate_project_toml(name);
    fs::write(path.join("krab.toml"), project_toml)?;

    let ci_yaml = generate_ci_workflow(name, template);
    fs::write(path.join(".github/workflows/ci.yaml"), ci_yaml)?;

    let deploy_yaml = generate_deploy_manifest(name, template);
    fs::write(path.join("deploy/kubernetes.yaml"), deploy_yaml)?;

    let dockerfile = generate_dockerfile(name);
    fs::write(path.join("Dockerfile"), dockerfile)?;

    let readme = generate_readme(name, metadata);
    fs::write(path.join("README.md"), readme)?;

    fs::write(path.join(".gitignore"), "target/\ndist/\n.env\n*.swp\n")?;

    println!("✅ Project '{}' created successfully!", name);
    println!("   Template: {:?}", template);
    println!("   Next: cd {} && cp .env.example .env && cargo run", name);
    Ok(())
}

/// Scaffold a new Krab project from one of the supported starter templates.
///
/// The generator writes a minimal but release-aware project skeleton including source layout,
/// environment template, CI workflow, deployment manifest, Dockerfile, and README.
///
/// `path_deps` points the generated manifest at a local checkout of this
/// repository instead of crates.io. It exists so the `generated-project` CI
/// gate can build scaffolded output against the working tree.
pub(super) fn generate_project_from_template(
    name: &str,
    template: &ProjectTemplate,
    path_deps: Option<&Path>,
) -> Result<()> {
    println!(
        "🦀 Scaffolding new Krab project '{}' (template: {:?})...",
        name, template
    );

    let deps = match path_deps {
        // Canonicalise so the generated manifest holds an absolute path. A
        // relative one would be interpreted relative to the *generated*
        // project, not the working directory the user typed it in.
        Some(root) => DependencySource::Path(root.canonicalize().map_err(|err| {
            anyhow::anyhow!(
                "--path-deps root '{}' is not readable: {err}",
                root.display()
            )
        })?),
        None => DependencySource::Registry,
    };

    let path = PathBuf::from(name);
    write_project_from_template(&path, name, template, &deps)
}

/// Generate the default minimal service entrypoint used by `krab new`.
fn generate_default_main(name: &str) -> String {
    format!(
        r#"use axum::routing::get;
use axum::{{Json, Router}};
use krab_core::config::KrabConfig;
use krab_core::telemetry::init_tracing;
use serde_json::json;
use std::net::SocketAddr;

// Named handlers rather than inline closures: the closure form put the whole
// route on one line, whose width depends on the project name, so `cargo fmt
// --check` failed for any name long enough to push it past 100 columns. The
// generated project runs that exact check in its own CI.
async fn index() -> Json<serde_json::Value> {{
    Json(json!({{ "service": "{name}", "status": "ok" }}))
}}

async fn health() -> Json<serde_json::Value> {{
    Json(json!({{ "status": "ok" }}))
}}

async fn ready() -> Json<serde_json::Value> {{
    Json(json!({{ "status": "ready" }}))
}}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {{
    init_tracing("{name}");
    let cfg = KrabConfig::from_env_checked("{name}", 3000)?;
    cfg.validate_all()?;
    let addr: SocketAddr = format!("{{}}:{{}}", cfg.host, cfg.port).parse()?;

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/ready", get(ready));

    tracing::info!(service = "{name}", %addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}}
"#
    )
}

/// Generate the SaaS-oriented starter with runtime state, auth-ready HTTP layers, and tenant APIs.
fn generate_saas_main(name: &str) -> String {
    format!(
        r#"use axum::routing::get;
use axum::{{Json, Router}};
use krab_core::config::KrabConfig;
use krab_core::http::{{apply_common_http_layers, HasRuntimeState, RuntimeState}};
use krab_core::telemetry::init_tracing;
use serde_json::json;
use std::net::SocketAddr;

#[derive(Clone)]
struct AppState {{
    runtime: RuntimeState,
}}

impl HasRuntimeState for AppState {{
    fn runtime_state(&self) -> &RuntimeState {{
        &self.runtime
    }}
}}

async fn health_handler() -> Json<serde_json::Value> {{
    Json(json!({{ "service": "{name}", "status": "ok" }}))
}}

async fn ready_handler() -> Json<serde_json::Value> {{
    Json(json!({{ "service": "{name}", "status": "ready" }}))
}}

async fn tenants_handler() -> Json<serde_json::Value> {{
    Json(json!({{ "tenants": [], "total": 0 }}))
}}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {{
    init_tracing("{name}");
    let cfg = KrabConfig::from_env_checked("{name}", 3000)?;
    cfg.validate_all()?;
    let addr: SocketAddr = format!("{{}}:{{}}", cfg.host, cfg.port).parse()?;

    let state = AppState {{
        runtime: RuntimeState::new(),
    }};

    let app: Router<AppState> = Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/api/v1/tenants", get(tenants_handler));

    let app = apply_common_http_layers(app, state.clone()).with_state(state);

    tracing::info!(service = "{name}", %addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}}
"#
    )
}

fn generate_edge_ssr_main(name: &str) -> String {
    format!(
        r###"use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use axum::{{Json, Router}};
use krab_core::config::KrabConfig;
use krab_core::isr::{{IsrCache, IsrPolicy}};
use krab_core::render_policy::{{CacheMode, EdgeCapability, RenderMode, RouteRenderPolicy}};
use krab_core::telemetry::init_tracing;
use serde_json::json;
use std::net::SocketAddr;
use std::time::Duration;

#[derive(Clone)]
struct AppState {{
    isr_cache: IsrCache,
}}

fn home_render_policy() -> RouteRenderPolicy {{
    RouteRenderPolicy::new(
        "/",
        RenderMode::Server,
        CacheMode::Isr {{
            revalidate_after: Duration::from_secs(30),
        }},
    )
    .with_edge_capability(EdgeCapability::Eligible)
    .with_streaming(true)
}}

const HOME_REVALIDATE: Duration = Duration::from_secs(30);

/// Serve `/` through the ISR cache: fresh entries are returned as-is, stale
/// entries are returned immediately and re-rendered behind the response, and a
/// miss renders and populates.
async fn home_handler(State(state): State<AppState>) -> Html<String> {{
    // A cache failure degrades to a render; it never fails the request.
    if let Ok(Some((cached, stale))) = state.isr_cache.serve("/").await {{
        if !stale {{
            return Html(cached);
        }}
        // Stale-while-revalidate: answer from cache, refresh for the next hit.
        let _ = state
            .isr_cache
            .put("/", render_home(), IsrPolicy::revalidate(HOME_REVALIDATE))
            .await;
        return Html(cached);
    }}

    let html = render_home();
    let _ = state
        .isr_cache
        .put("/", html.clone(), IsrPolicy::revalidate(HOME_REVALIDATE))
        .await;
    Html(html)
}}

fn render_home() -> String {{
    let policy = home_render_policy();
    format!(
        r##"<!DOCTYPE html>
<html>
<head>
    <title>{name}</title>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
</head>
<body>
    <h1>Welcome to {name}</h1>
    <p>Render policy skeleton: server + ISR + edge eligible + streaming</p>
    <p>Route pattern: {{}}</p>
</body>
</html>"##,
        policy.route_pattern
    )
}}

async fn health_handler() -> Json<serde_json::Value> {{
    Json(json!({{ "service": "{name}", "status": "ok" }}))
}}

async fn ready_handler() -> Json<serde_json::Value> {{
    Json(json!({{ "service": "{name}", "status": "ready" }}))
}}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {{
    init_tracing("{name}");
    let cfg = KrabConfig::from_env_checked("{name}", 3000)?;
    cfg.validate_all()?;
    let addr: SocketAddr = format!("{{}}:{{}}", cfg.host, cfg.port).parse()?;
    let home_policy = home_render_policy();
    if let Err(err) = home_policy.validates() {{
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, err).into());
    }}

    let state = AppState {{
        isr_cache: IsrCache::new(),
    }};

    let app = Router::new()
        .route("/", get(home_handler))
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .with_state(state);

    tracing::info!(service = "{name}", %addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}}
"###
    )
}

fn generate_event_stream_main(name: &str) -> String {
    format!(
        r#"use axum::extract::ws::{{Message, WebSocket, WebSocketUpgrade}};
use axum::response::{{sse, Sse}};
use axum::routing::get;
use axum::{{Json, Router}};
use futures_util::StreamExt;
use krab_core::config::KrabConfig;
use krab_core::telemetry::init_tracing;
use serde_json::json;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;
use tokio_stream::wrappers::IntervalStream;

async fn health_handler() -> Json<serde_json::Value> {{
    Json(json!({{ "service": "{name}", "status": "ok" }}))
}}

async fn ready_handler() -> Json<serde_json::Value> {{
    Json(json!({{ "service": "{name}", "status": "ready" }}))
}}

async fn dashboard_handler() -> Json<serde_json::Value> {{
    Json(json!({{
        "events_total": 0,
        "events_per_second": 0.0,
        "active_streams": 0,
    }}))
}}

fn unix_millis() -> u128 {{
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}}

async fn events_sse() -> Sse<impl futures_util::Stream<Item = Result<sse::Event, Infallible>>> {{
    let ticks = IntervalStream::new(tokio::time::interval(Duration::from_secs(1)));
    let stream = ticks.map(|_| {{
        let data = json!({{ "ts": unix_millis(), "event": "tick" }});
        Ok(sse::Event::default().data(data.to_string()))
    }});
    Sse::new(stream)
}}

async fn ws_handler(ws: WebSocketUpgrade) -> impl axum::response::IntoResponse {{
    ws.on_upgrade(handle_ws)
}}

async fn handle_ws(mut socket: WebSocket) {{
    while let Some(Ok(msg)) = socket.recv().await {{
        if let Message::Text(text) = msg {{
            let echo = format!("echo: {{}}", text);
            if socket.send(Message::Text(echo.into())).await.is_err() {{
                break;
            }}
        }}
    }}
}}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {{
    init_tracing("{name}");
    let cfg = KrabConfig::from_env_checked("{name}", 3000)?;
    cfg.validate_all()?;
    let addr: SocketAddr = format!("{{}}:{{}}", cfg.host, cfg.port).parse()?;

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/api/dashboard", get(dashboard_handler))
        .route("/api/events", get(events_sse))
        .route("/api/ws", get(ws_handler));

    tracing::info!(service = "{name}", %addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}}
"#
    )
}

fn generate_ci_workflow(name: &str, template: &ProjectTemplate) -> String {
    let extra_steps = match template {
        ProjectTemplate::Saas => format!(
            r#"
      - name: Run database migration checks
        run: cargo test --package {name} -- db_
        env:
          DATABASE_URL: postgres://localhost:5432/{name}_test
"#
        ),
        _ => String::new(),
    };

    format!(
        r#"name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: -D warnings

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy

      - name: Cache cargo registry
        uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            target
          key: ${{{{ runner.os }}}}-cargo-${{{{ hashFiles('**/Cargo.lock') }}}}

      - name: Check formatting
        run: cargo fmt --all --check

      - name: Clippy
        run: cargo clippy --all-targets --all-features -- -D warnings

      - name: Run tests
        run: cargo test --all-features
        env:
          KRAB_ENVIRONMENT: dev
          KRAB_AUTH_MODE: static
{extra_steps}
      - name: Dependency policy gate
        uses: EmbarkStudios/cargo-deny-action@v2
        with:
          command: check advisories licenses bans sources
"#
    )
}

fn generate_deploy_manifest(name: &str, template: &ProjectTemplate) -> String {
    let (replicas, memory_limit) = match template {
        ProjectTemplate::Saas => ("3", "512Mi"),
        ProjectTemplate::EdgeSsr => ("2", "256Mi"),
        ProjectTemplate::EventStream => ("2", "384Mi"),
        ProjectTemplate::Default => ("1", "128Mi"),
    };

    format!(
        r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: {name}
  labels:
    app: {name}
spec:
  replicas: {replicas}
  selector:
    matchLabels:
      app: {name}
  template:
    metadata:
      labels:
        app: {name}
    spec:
      containers:
        - name: {name}
          image: {name}:latest
          ports:
            - containerPort: 3000
          env:
            - name: KRAB_ENVIRONMENT
              valueFrom:
                configMapKeyRef:
                  name: {name}-config
                  key: KRAB_ENVIRONMENT
            - name: KRAB_PORT
              value: "3000"
          securityContext:
            runAsNonRoot: true
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop: ["ALL"]
          resources:
            requests:
              memory: "64Mi"
              cpu: "100m"
            limits:
              memory: "{memory_limit}"
              cpu: "500m"
          startupProbe:
            httpGet:
              path: /ready
              port: 3000
            periodSeconds: 5
            failureThreshold: 12
          livenessProbe:
            httpGet:
              path: /health
              port: 3000
            initialDelaySeconds: 5
            periodSeconds: 10
          readinessProbe:
            httpGet:
              path: /ready
              port: 3000
            initialDelaySeconds: 3
            periodSeconds: 5
---
apiVersion: v1
kind: Service
metadata:
  name: {name}
spec:
  selector:
    app: {name}
  ports:
    - port: 80
      targetPort: 3000
  type: ClusterIP
"#
    )
}

fn generate_dockerfile(name: &str) -> String {
    format!(
        r#"# Build stage
FROM rust:1.77-slim-bookworm AS builder
WORKDIR /app
# No Cargo.lock COPY: generated projects do not ship one until the first
# local build, and a COPY of a missing file fails the whole build. Commit
# your Cargo.lock and add it here for reproducible container builds.
COPY Cargo.toml ./
COPY src/ src/
RUN cargo build --release --bin {name}

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/{name} /app/{name}
COPY public/ public/

ENV KRAB_HOST=0.0.0.0
ENV KRAB_PORT=3000
EXPOSE 3000

CMD ["/app/{name}"]
"#
    )
}

fn generate_project_toml(name: &str) -> String {
    format!(
        r#"[project]
frontend_bin = "{name}"
public_dir = "public"
dist_dir = "dist"
server_paths = ["src"]
public_paths = ["public"]
bootstrap_bin = "{name}"
hmr_signal_path = "dist/.hmr_signal"
"#
    )
}

#[cfg(test)]
mod tests {
    use super::{
        generate_ci_workflow, generate_deploy_manifest, generate_edge_ssr_main,
        write_project_from_template, DependencySource, FRAMEWORK_VERSION,
    };
    use crate::ProjectTemplate;
    use anyhow::Result;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn generate_fixture(name: &str, template: &ProjectTemplate) -> Result<(TempDir, PathBuf)> {
        generate_fixture_with(name, template, &DependencySource::Registry)
    }

    fn generate_fixture_with(
        name: &str,
        template: &ProjectTemplate,
        deps: &DependencySource,
    ) -> Result<(TempDir, PathBuf)> {
        let temp_dir = TempDir::new()?;
        let project_dir = temp_dir.path().join(name);
        write_project_from_template(&project_dir, name, template, deps)?;
        Ok((temp_dir, project_dir))
    }

    fn assert_common_scaffold(project_dir: &Path, name: &str) -> Result<()> {
        for relative in [
            "Cargo.toml",
            "src/main.rs",
            ".env.example",
            "krab.toml",
            ".github/workflows/ci.yaml",
            "deploy/kubernetes.yaml",
            "Dockerfile",
            "README.md",
            ".gitignore",
        ] {
            assert!(
                project_dir.join(relative).exists(),
                "expected {relative} to be generated"
            );
        }

        let project_toml = fs::read_to_string(project_dir.join("krab.toml"))?;
        assert!(project_toml.contains(&format!("frontend_bin = \"{name}\"")));
        assert!(project_toml.contains(&format!("bootstrap_bin = \"{name}\"")));

        let readme = fs::read_to_string(project_dir.join("README.md"))?;
        assert!(readme.contains("krab doctor --diagnostics"));
        // Governance commands are framework-internal today; the README must
        // not tell a consumer to run them, only note their current scope.
        assert!(
            !readme.contains("krab release certify --out release-evidence"),
            "generated README instructs consumers to run framework-internal governance"
        );
        assert!(readme.contains("operate on the Krab framework workspace itself"));

        // The Dockerfile must not COPY files the scaffold does not create:
        // generated projects have no committed Cargo.lock, and a COPY of a
        // missing file fails `docker build` outright.
        let dockerfile = fs::read_to_string(project_dir.join("Dockerfile"))?;
        assert!(
            !dockerfile.contains("COPY Cargo.toml Cargo.lock"),
            "generated Dockerfile copies a Cargo.lock that does not exist in a fresh project"
        );
        assert!(dockerfile.contains("COPY Cargo.toml ./"));
        Ok(())
    }

    const ALL_TEMPLATES: [ProjectTemplate; 4] = [
        ProjectTemplate::Default,
        ProjectTemplate::Saas,
        ProjectTemplate::EdgeSsr,
        ProjectTemplate::EventStream,
    ];

    /// Feature names `krab_core` actually declares, read from its manifest.
    ///
    /// Read rather than hard-coded so renaming or splitting a `krab_core`
    /// feature fails this test instead of silently shipping a template that
    /// requests a feature which no longer exists.
    fn krab_core_features() -> Vec<String> {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../framework/krab_core/Cargo.toml")
            .canonicalize()
            .expect("krab_core manifest not found");
        let parsed: toml::Value =
            toml::from_str(&fs::read_to_string(manifest).expect("krab_core manifest unreadable"))
                .expect("krab_core manifest is not valid TOML");

        parsed
            .get("features")
            .and_then(|f| f.as_table())
            .expect("krab_core declares no [features]")
            .keys()
            .cloned()
            .collect()
    }

    fn generated_dependencies(project_dir: &Path) -> toml::value::Table {
        let raw = fs::read_to_string(project_dir.join("Cargo.toml"))
            .expect("generated Cargo.toml unreadable");
        let parsed: toml::Value = toml::from_str(&raw)
            .unwrap_or_else(|e| panic!("generated Cargo.toml is not valid TOML: {e}\n{raw}"));
        parsed
            .get("dependencies")
            .and_then(|d| d.as_table())
            .expect("generated Cargo.toml has no [dependencies]")
            .clone()
    }

    /// The `saas` template used to emit `features = ["db, rest"]` — a single
    /// feature named `db, rest`, which does not exist. Cargo rejected it, so
    /// every `krab new --template saas` produced a project that could not
    /// resolve. Substring assertions did not catch it; parsing does.
    #[test]
    fn every_template_requests_only_real_krab_core_features() -> Result<()> {
        let declared = krab_core_features();

        for template in ALL_TEMPLATES {
            let (_temp, project_dir) = generate_fixture("demo-features", &template)?;
            let deps = generated_dependencies(&project_dir);

            let requested = deps
                .get("krab_core")
                .and_then(|d| d.get("features"))
                .and_then(|f| f.as_array())
                .unwrap_or_else(|| panic!("{template:?}: krab_core features is not an array"));

            assert!(
                !requested.is_empty(),
                "{template:?}: krab_core has no features; it has no default features either"
            );

            for feature in requested {
                let name = feature
                    .as_str()
                    .unwrap_or_else(|| panic!("{template:?}: non-string feature {feature:?}"));
                assert!(
                    declared.iter().any(|d| d == name),
                    "{template:?}: requests krab_core feature {name:?}, which krab_core does \
                     not declare. Declared: {declared:?}"
                );
            }
        }
        Ok(())
    }

    /// The template version must track the workspace version; a stale pin
    /// would make generated projects request a version that does not exist.
    #[test]
    fn every_template_pins_the_current_framework_version() -> Result<()> {
        for template in ALL_TEMPLATES {
            let (_temp, project_dir) = generate_fixture("demo-version", &template)?;
            let deps = generated_dependencies(&project_dir);

            for crate_name in ["krab_core", "krab_macros"] {
                let version = deps
                    .get(crate_name)
                    .unwrap_or_else(|| panic!("{template:?}: {crate_name} is not a dependency"))
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| panic!("{template:?}: {crate_name} has no version"));

                assert_eq!(
                    version, FRAMEWORK_VERSION,
                    "{template:?}: {crate_name} pinned to {version}, expected {FRAMEWORK_VERSION}"
                );
            }
        }
        Ok(())
    }

    /// `--path-deps` is what lets the `generated-project` CI gate build
    /// scaffolded output before the version is on crates.io.
    #[test]
    fn path_deps_emit_local_paths_and_no_version() -> Result<()> {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()?;
        let (_temp, project_dir) = generate_fixture_with(
            "demo-path-deps",
            &ProjectTemplate::Default,
            &DependencySource::Path(repo_root),
        )?;
        let deps = generated_dependencies(&project_dir);

        for (crate_name, expected_suffix) in [
            ("krab_core", "crates/framework/krab_core"),
            ("krab_macros", "crates/framework/krab_macros"),
        ] {
            let entry = deps
                .get(crate_name)
                .unwrap_or_else(|| panic!("{crate_name} is not a dependency"));

            let path = entry
                .get("path")
                .and_then(|p| p.as_str())
                .unwrap_or_else(|| panic!("{crate_name} has no path"));

            assert!(
                path.ends_with(expected_suffix),
                "{crate_name} path {path:?} does not end with {expected_suffix:?}"
            );
            // Backslashes are an invalid escape inside a TOML basic string, so
            // a Windows-rendered path would make the manifest unparseable.
            assert!(
                !path.contains('\\'),
                "{crate_name} path {path:?} contains a backslash"
            );
            // `\\?\C:\...` normalises to `//?/C:/...`, which passes both checks
            // above and still makes Cargo fail with `invalid path url`.
            assert!(
                !path.starts_with("//?/"),
                "{crate_name} path {path:?} kept the Windows verbatim prefix"
            );
            assert!(
                entry.get("version").is_none(),
                "{crate_name} must not carry a version when using --path-deps; the local \
                 checkout is the point"
            );
            assert!(
                Path::new(path).join("Cargo.toml").is_file(),
                "{crate_name} path {path:?} does not point at a crate"
            );
        }

        // The assertions above are all satisfiable by a manifest Cargo still
        // refuses — `//?/C:/...` passed every one of them. Only Cargo's own
        // parser settles it.
        let output = std::process::Command::new(env!("CARGO"))
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .current_dir(&project_dir)
            .output()?;
        assert!(
            output.status.success(),
            "cargo cannot parse the generated manifest:\n{}\n---\n{}",
            String::from_utf8_lossy(&output.stderr),
            fs::read_to_string(project_dir.join("Cargo.toml"))?
        );
        Ok(())
    }

    #[test]
    fn generated_ci_uses_pinned_dependency_gate_action() {
        let workflow = generate_ci_workflow("demo", &ProjectTemplate::Default);

        assert!(workflow.contains("EmbarkStudios/cargo-deny-action@v2"));
        assert!(!workflow.contains("cargo install cargo-deny"));
        assert!(workflow.contains("cargo fmt --all --check"));
    }

    #[test]
    fn generated_deploy_manifest_includes_ready_startup_probe_and_security_context() {
        let manifest = generate_deploy_manifest("demo", &ProjectTemplate::Default);

        assert!(manifest.contains("startupProbe:"));
        assert!(manifest.contains("path: /ready"));
        assert!(manifest.contains("runAsNonRoot: true"));
        assert!(manifest.contains("allowPrivilegeEscalation: false"));
    }

    #[test]
    fn edge_ssr_template_uses_render_policy_vocabulary() {
        let main_rs = generate_edge_ssr_main("demo");

        assert!(main_rs.contains("RouteRenderPolicy"));
        assert!(main_rs.contains("CacheMode::Isr"));
        assert!(main_rs.contains("RenderMode::Server"));
        assert!(main_rs.contains("with_streaming(true)"));
        assert!(
            main_rs.contains("Render policy skeleton: server + ISR + edge eligible + streaming")
        );
    }

    #[test]
    fn default_template_smoke_generates_expected_files() -> Result<()> {
        let (_temp_dir, project_dir) = generate_fixture("demo-default", &ProjectTemplate::Default)?;
        assert_common_scaffold(&project_dir, "demo-default")?;

        let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;
        assert!(main_rs.contains(r#".route("/", get"#));
        assert!(main_rs.contains(r#""service": "demo-default""#));
        assert!(main_rs.contains(r#""status": "ready""#));
        Ok(())
    }

    #[test]
    fn saas_template_smoke_marks_scope_as_scaffold() -> Result<()> {
        let (_temp_dir, project_dir) = generate_fixture("demo-saas", &ProjectTemplate::Saas)?;
        assert_common_scaffold(&project_dir, "demo-saas")?;

        let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;
        assert!(main_rs.contains("apply_common_http_layers"));
        assert!(main_rs.contains("/api/v1/tenants"));

        let readme = fs::read_to_string(project_dir.join("README.md"))?;
        assert!(readme.contains("SaaS service skeleton"));
        assert!(readme.contains("does not include a completed auth flow"));

        let workflow = fs::read_to_string(project_dir.join(".github/workflows/ci.yaml"))?;
        assert!(workflow.contains("DATABASE_URL"));
        Ok(())
    }

    #[test]
    fn edge_ssr_template_smoke_calls_out_policy_scope() -> Result<()> {
        let (_temp_dir, project_dir) = generate_fixture("demo-edge", &ProjectTemplate::EdgeSsr)?;
        assert_common_scaffold(&project_dir, "demo-edge")?;

        let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;
        assert!(main_rs.contains("RouteRenderPolicy"));
        assert!(main_rs.contains("Render policy skeleton"));

        // The ISR cache was held in `AppState` and never read, which failed the
        // `-D warnings` clippy the generated CI runs. It is now wired into the
        // `/` handler, so the starter-scope note must not still disclaim it.
        assert!(main_rs.contains("isr_cache.serve("));
        assert!(main_rs.contains("IsrPolicy::revalidate"));

        let readme = fs::read_to_string(project_dir.join("README.md"))?;
        assert!(readme.contains("Edge SSR policy skeleton"));
        assert!(readme.contains("stale-while-revalidate ISR serving"));
        assert!(
            !readme.contains("does not ship full ISR cache serving"),
            "starter-scope note still disclaims ISR serving that the template now does"
        );
        Ok(())
    }

    #[test]
    fn event_stream_template_smoke_generates_stream_endpoints() -> Result<()> {
        let (_temp_dir, project_dir) =
            generate_fixture("demo-stream", &ProjectTemplate::EventStream)?;
        assert_common_scaffold(&project_dir, "demo-stream")?;

        let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;
        assert!(main_rs.contains("/api/events"));
        assert!(main_rs.contains("/api/ws"));
        assert!(main_rs.contains("tokio_stream"));
        Ok(())
    }
}
