use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

use crate::ProjectTemplate;

#[derive(Clone, Copy)]
struct TemplateMetadata {
    flag: &'static str,
    description: &'static str,
    starter_scope: Option<&'static str>,
    axum_dep: &'static str,
    extra_deps: &'static str,
    extra_features: &'static str,
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
            extra_features: "db, rest",
        },
        ProjectTemplate::EdgeSsr => TemplateMetadata {
            flag: "edge-ssr",
            description: "Edge SSR policy skeleton with explicit SSR, ISR, and streaming metadata",
            starter_scope: Some(
                "This starter demonstrates route render-policy wiring and edge eligibility metadata. It does not ship full ISR cache serving or streamed SSR output out of the box.",
            ),
            axum_dep: r#"axum = "0.8"
"#,
            extra_deps: "",
            extra_features: "rest",
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
            extra_features: "rest",
        },
        ProjectTemplate::Default => TemplateMetadata {
            flag: "default",
            description: "Minimal Krab service",
            starter_scope: None,
            axum_dep: r#"axum = "0.8"
"#,
            extra_deps: "",
            extra_features: "rest",
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
krab release certify --out release-evidence
```

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

fn write_project_from_template(path: &Path, name: &str, template: &ProjectTemplate) -> Result<()> {
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
krab_core = {{ version = "0.1.0", features = ["{extra_features}"] }}
tokio = {{ version = "1.0", features = ["full"] }}
{axum_dep}serde = {{ version = "1.0", features = ["derive"] }}
serde_json = "1.0"
tracing = "0.1"
tracing-subscriber = {{ version = "0.3", features = ["json", "env-filter"] }}
{extra_deps}
"#,
        template_desc = metadata.description,
        extra_features = metadata.extra_features,
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
pub(super) fn generate_project_from_template(name: &str, template: &ProjectTemplate) -> Result<()> {
    println!(
        "🦀 Scaffolding new Krab project '{}' (template: {:?})...",
        name, template
    );
    let path = PathBuf::from(name);
    write_project_from_template(&path, name, template)
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {{
    init_tracing("{name}");
    let cfg = KrabConfig::from_env_checked("{name}", 3000)?;
    cfg.validate_all()?;
    let addr: SocketAddr = format!("{{}}:{{}}", cfg.host, cfg.port).parse()?;

    let app = Router::new()
        .route("/", get(|| async {{ Json(json!({{ "service": "{name}", "status": "ok" }})) }}))
        .route("/health", get(|| async {{ Json(json!({{ "status": "ok" }})) }}))
        .route("/ready", get(|| async {{ Json(json!({{ "status": "ready" }})) }}));

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
        r###"use axum::response::Html;
use axum::routing::get;
use axum::{{Router, Json}};
use krab_core::config::KrabConfig;
use krab_core::isr::IsrCache;
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

async fn home_handler() -> Html<String> {{
    let policy = home_render_policy();
    Html(format!(
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
    ))
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
use futures_util::{{SinkExt, StreamExt}};
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

async fn events_sse() -> Sse<impl futures_util::Stream<Item = Result<sse::Event, Infallible>>> {{
    let stream = IntervalStream::new(tokio::time::interval(Duration::from_secs(1)))
        .map(|_| {{
            let data = json!({{ "ts": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(), "event": "tick" }});
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
COPY Cargo.toml Cargo.lock ./
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
        write_project_from_template,
    };
    use crate::ProjectTemplate;
    use anyhow::Result;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn generate_fixture(name: &str, template: &ProjectTemplate) -> Result<(TempDir, PathBuf)> {
        let temp_dir = TempDir::new()?;
        let project_dir = temp_dir.path().join(name);
        write_project_from_template(&project_dir, name, template)?;
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
        assert!(readme.contains("krab release certify --out release-evidence"));
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

        let readme = fs::read_to_string(project_dir.join("README.md"))?;
        assert!(readme.contains("Edge SSR policy skeleton"));
        assert!(readme.contains("does not ship full ISR cache serving or streamed SSR output"));
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
