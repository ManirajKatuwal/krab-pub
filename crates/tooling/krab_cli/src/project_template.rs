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
        self.render_with_defaults(crate_name, crate_dir, features, true)
    }

    /// Render one dependency line, opting out of the dependency's own default
    /// features.
    ///
    /// Only `krab_client` needs this so far: its defaults include
    /// `demo-islands`, whose bundled `Counter`/`Toggle`/`Likes` register
    /// themselves in the same `inventory` island registry a generated project
    /// uses, so a template that names its own `Counter` gets two entries and
    /// `hydrate()` may bind the demo one over the template's SSR markup.
    fn render_no_default_features(
        &self,
        crate_name: &str,
        crate_dir: &str,
        features: &[&str],
    ) -> String {
        self.render_with_defaults(crate_name, crate_dir, features, false)
    }

    fn render_with_defaults(
        &self,
        crate_name: &str,
        crate_dir: &str,
        features: &[&str],
        default_features: bool,
    ) -> String {
        let defaults = if default_features {
            String::new()
        } else {
            ", default-features = false".to_string()
        };
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
                format!(
                    "{crate_name} = {{ version = \"{FRAMEWORK_VERSION}\"{defaults}{features} }}"
                )
            }
            DependencySource::Path(root) => {
                let path = path_for_toml(&root.join(crate_dir));
                format!("{crate_name} = {{ path = \"{path}\"{defaults}{features} }}")
            }
        }
    }
}

/// Minimum toolchain the generated dependency set actually needs.
///
/// A generated project depends on `krab_core`, whose resolved graph needs
/// 1.89 (`async-graphql` 7.2 under `graphql`, `time` 0.3.47 under either
/// database driver), and the generated manifest floats its other dependencies
/// (`axum = "0.8"`, `tokio = "1.0"`, ...) to the latest compatible release, so
/// the real floor is whatever those resolve to. This matches the workspace's
/// own `rust-version`, which said `1.75` through 0.4.0 and was never true.
/// Declaring it in the generated manifest turns an MSRV mismatch into Cargo's
/// own "package requires rustc 1.89" message instead of a type error deep
/// inside a dependency, and keeps the Dockerfile's toolchain choice checkable.
const GENERATED_PROJECT_MSRV: &str = "1.89";

#[derive(Clone, Copy)]
struct TemplateMetadata {
    flag: &'static str,
    description: &'static str,
    starter_scope: Option<&'static str>,
    axum_dep: &'static str,
    extra_deps: &'static str,
    /// Whether this template ships `docs/render_policy.md`.
    ///
    /// Only `edge-ssr` configures a [`krab_core::render_policy::RouteRenderPolicy`],
    /// so it is the only template for which such a document says anything. The
    /// scaffold used to create an empty `docs/` for every template, which git
    /// dropped on the first clone.
    render_policy_doc: bool,
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
            render_policy_doc: false,
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
            render_policy_doc: true,
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
            render_policy_doc: false,
            extra_features: &["rest"],
        },
        ProjectTemplate::Fullstack => TemplateMetadata {
            flag: "fullstack",
            description: "Full-stack SSR with WASM island hydration and server functions",
            starter_scope: Some(
                "This starter provides full-stack server-side rendering (SSR), client-side WASM island hydration, static asset serving, and server functions out of the box.",
            ),
            axum_dep: "",
            extra_deps: "",
            render_policy_doc: false,
            extra_features: &[],
        },
        ProjectTemplate::Default => TemplateMetadata {
            flag: "default",
            description: "Minimal Krab service",
            starter_scope: None,
            axum_dep: r#"axum = "0.8"
"#,
            extra_deps: "",
            render_policy_doc: false,
            extra_features: &["rest"],
        },
    }
}

fn generate_readme(name: &str, metadata: TemplateMetadata) -> String {
    let starter_scope = metadata
        .starter_scope
        .map(|note| format!("## Starter Scope\n\n{}\n\n", note))
        .unwrap_or_default();

    // Without a pointer the generated document is undiscoverable.
    let render_policy_doc = if metadata.render_policy_doc {
        "## Render Policy\n\nThis starter declares one route render policy. \
         [`docs/render_policy.md`](docs/render_policy.md) documents what it \
         configures, what startup validation rejects, and how to change it.\n\n"
    } else {
        ""
    };

    format!(
        r#"# {name}

> {template_desc}

Generated with `krab new {name} --template {template_flag}`.

{starter_scope}{render_policy_doc}## Quick Start

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

/// Rust keywords (2015, 2018, 2021) plus the words reserved for future use.
///
/// Cargo refuses these as package *and* binary target names, so a project named
/// after one cannot be built at all.
const RUST_KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "try", "type",
    "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// Windows reserved device names. The project name becomes a directory name, so
/// `krab new con` cannot even create its own folder on the platform this
/// project primarily targets. Cargo rejects these outright as well.
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Names Cargo rejects because they collide with its own build layout
/// (`target/debug/deps`, `examples`, `build`, `incremental`) or with Rust's
/// built-in `test` crate. These are `cargo new` errors, not warnings, so they
/// belong in the same gate.
const CARGO_RESERVED_NAMES: &[&str] = &["deps", "examples", "build", "incremental", "test"];

/// crates.io caps package names at 64 characters.
const MAX_PROJECT_NAME_LEN: usize = 64;

fn is_reserved_project_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    RUST_KEYWORDS.contains(&name)
        || RUST_KEYWORDS.contains(&lower.as_str())
        || WINDOWS_RESERVED_NAMES.contains(&lower.as_str())
        || CARGO_RESERVED_NAMES.contains(&lower.as_str())
}

/// Derive a valid project name from whatever the user typed, for the "did you
/// mean" line of a rejection.
///
/// Always returns a name that [`validate_project_name`] accepts.
fn suggested_project_slug(raw: &str) -> String {
    let mut slug = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }

    // ASCII by construction, so byte truncation cannot split a character.
    slug.truncate(MAX_PROJECT_NAME_LEN);
    let mut slug = slug.trim_matches('-').to_string();

    if slug.is_empty() {
        return "krab-app".to_string();
    }
    if slug.starts_with(|c: char| c.is_ascii_digit()) {
        slug.insert_str(0, "app-");
        slug.truncate(MAX_PROJECT_NAME_LEN);
    }
    if is_reserved_project_name(&slug) {
        slug.push_str("-app");
    }
    slug
}

fn reject_project_name(name: &str, reason: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "invalid project name '{name}': {reason}\n\
         The name is used as the Cargo package name, the binary name, and the directory \
         name, so it has to satisfy all three.\n\
         Try: krab new {slug}",
        slug = suggested_project_slug(name)
    )
}

/// Reject a project name that would produce an unbuildable project.
///
/// Interpolating the name straight into `name = "{name}"` used to defer the
/// failure to the user's first `cargo run`, with a Cargo parse error pointing at
/// a manifest they did not write. The rules below are Cargo's own package-name
/// rules, narrowed to ASCII, plus the Windows device names — the name becomes a
/// directory too.
fn validate_project_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(reject_project_name(name, "the name is empty"));
    }
    if name.len() > MAX_PROJECT_NAME_LEN {
        return Err(reject_project_name(
            name,
            &format!(
                "it is {} characters; crates.io allows at most {MAX_PROJECT_NAME_LEN}",
                name.len()
            ),
        ));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Err(reject_project_name(
            name,
            &format!("it contains {bad:?}; only ASCII letters, digits, '-' and '_' are allowed"),
        ));
    }

    // Non-empty and ASCII-only at this point.
    let first = name.as_bytes()[0];
    if first.is_ascii_digit() {
        return Err(reject_project_name(
            name,
            "it starts with a digit; Cargo package names must start with a letter or '_'",
        ));
    }
    if first == b'-' {
        return Err(reject_project_name(name, "it starts with '-'"));
    }

    if RUST_KEYWORDS.contains(&name) {
        return Err(reject_project_name(name, "it is a Rust keyword"));
    }
    if WINDOWS_RESERVED_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(reject_project_name(
            name,
            "it is a reserved Windows device name",
        ));
    }
    if CARGO_RESERVED_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(reject_project_name(
            name,
            "Cargo reserves it — it collides with Cargo's build directory names or with \
             Rust's built-in test library",
        ));
    }
    Ok(())
}

fn write_project_from_template(
    path: &Path,
    name: &str,
    template: &ProjectTemplate,
    deps: &DependencySource,
) -> Result<()> {
    // Before anything touches the filesystem: a bad name must leave nothing
    // behind, not a half-scaffolded directory.
    validate_project_name(name)?;
    if path.exists() {
        anyhow::bail!("Directory '{}' already exists", path.display());
    }
    let metadata = template_metadata(template);

    // Only directories this function actually populates are created. Git does
    // not track empty directories, so `src/routes/`, `src/api/` and `docs/`
    // vanished the moment anyone cloned a scaffolded project — and `public/`
    // vanishing broke `docker build`, because the generated Dockerfile does
    // `COPY public/ public/`. `src/routes/` is now created on demand by
    // `krab gen route`, and `public/` is pinned by a `.gitkeep`.
    for dir in ["", "src", "public", ".github/workflows", "deploy"] {
        fs::create_dir_all(path.join(dir))?;
    }

    fs::write(
        path.join("public/.gitkeep"),
        "# Keeps public/ in git so the Dockerfile's `COPY public/ public/` survives a clone.\n",
    )?;

    let cargo_toml = if matches!(template, ProjectTemplate::Fullstack) {
        generate_fullstack_cargo_toml(name, metadata, deps)
    } else {
        generate_standard_cargo_toml(name, metadata, deps)
    };
    fs::write(path.join("Cargo.toml"), cargo_toml)?;

    if *template == ProjectTemplate::Fullstack {
        fs::write(path.join("src/lib.rs"), generate_fullstack_lib(name))?;
    }

    let main_rs = match template {
        ProjectTemplate::Saas => generate_saas_main(name),
        ProjectTemplate::EdgeSsr => generate_edge_ssr_main(name),
        ProjectTemplate::EventStream => generate_event_stream_main(name),
        ProjectTemplate::Fullstack => generate_fullstack_main(name),
        ProjectTemplate::Default => generate_default_main(name),
    };
    fs::write(path.join("src/main.rs"), main_rs)?;

    fs::write(
        path.join(".env.example"),
        generate_env_example(name, template),
    )?;

    if let Some(doc) = generate_render_policy_doc(name, metadata) {
        fs::create_dir_all(path.join("docs"))?;
        fs::write(path.join("docs/render_policy.md"), doc)?;
    }

    let project_toml = generate_project_toml(name, template);
    fs::write(path.join("krab.toml"), project_toml)?;

    let ci_yaml = generate_ci_workflow(name);
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
/// Result of the post-scaffold `git init`.
///
/// Every variant leaves a complete, working project — initialising a repository
/// is a convenience, never a precondition, so nothing here is an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GitInitOutcome {
    /// An empty repository was created. No commit is made: what to commit, and
    /// under whose identity, is the user's call.
    Initialised,
    /// `--no-git` was passed.
    Disabled,
    /// The target already sits inside a git work tree. Nesting a repository
    /// inside someone's checkout is worse than leaving it alone — the
    /// `generated-project` CI gate scaffolds into `$RUNNER_TEMP`, but a
    /// developer trying a template out will often do it inside a clone.
    AlreadyInWorkTree,
    /// `git` is not on PATH, or `git init` failed. Carries the reason.
    Unavailable(String),
}

fn run_git(path: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
}

/// Initialise a git repository in the freshly scaffolded `path`.
///
/// `krab new` writes a `.gitignore` for a repository it never created. This
/// creates it — but only when doing so is unambiguously right.
fn init_git_repository(path: &Path) -> GitInitOutcome {
    // A non-zero exit is the ordinary "not a git repository" answer, so only a
    // successful `true` counts as "already tracked".
    match run_git(path, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(probe) => {
            if probe.status.success() && String::from_utf8_lossy(&probe.stdout).trim() == "true" {
                return GitInitOutcome::AlreadyInWorkTree;
            }
        }
        Err(err) => return GitInitOutcome::Unavailable(format!("git is not on PATH: {err}")),
    }

    match run_git(path, &["init"]) {
        Ok(out) if out.status.success() => GitInitOutcome::Initialised,
        Ok(out) => {
            let reason = String::from_utf8_lossy(&out.stderr).trim().to_string();
            GitInitOutcome::Unavailable(if reason.is_empty() {
                format!("git init exited with {}", out.status)
            } else {
                reason
            })
        }
        Err(err) => GitInitOutcome::Unavailable(format!("git is not on PATH: {err}")),
    }
}

fn report_git_init(outcome: &GitInitOutcome, path: &Path) {
    match outcome {
        GitInitOutcome::Initialised => {
            println!("   Initialised an empty git repository (no commit was created).");
        }
        GitInitOutcome::Disabled => {}
        GitInitOutcome::AlreadyInWorkTree => println!(
            "ℹ️  '{}' is already inside a git work tree, so no repository was initialised. \
             The generated .gitignore applies to the enclosing repository.",
            path.display()
        ),
        GitInitOutcome::Unavailable(reason) => println!(
            "⚠️  Could not initialise a git repository ({reason}). The project is complete — \
             run `git init` in it yourself once git is available."
        ),
    }
}

pub(super) fn generate_project_from_template(
    name: &str,
    template: &ProjectTemplate,
    path_deps: Option<&Path>,
    no_git: bool,
) -> Result<()> {
    // Checked here as well as in `write_project_from_template` so a rejection
    // is not preceded by an optimistic "Scaffolding..." line.
    validate_project_name(name)?;

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
    write_project_from_template(&path, name, template, &deps)?;

    let git = if no_git {
        GitInitOutcome::Disabled
    } else {
        init_git_repository(&path)
    };
    report_git_init(&git, &path);

    println!("   Next: cd {} && cp .env.example .env && cargo run", name);
    Ok(())
}

/// The secret-sourcing preamble every generated `.env.example` carries.
///
/// The scaffold used to ship `KRAB_JWT_SECRET=change-me-in-production` next to
/// `KRAB_SECRETS_SOURCE=env` — a variable no Krab code reads and no reference
/// page documents — which taught a configuration that
/// `KrabConfig::validate_all()` refuses to start on in `staging`/`prod`. The
/// rules restated here are the ones `krab_core::config::check_secret_source`
/// and `read_env_or_file` actually implement.
const SECRET_SOURCING_PREAMBLE: &str = r#"#
# SECRET SOURCING — read this before promoting past dev.
#
# Secrets reach the process through `krab_core::config::read_env_or_file()`,
# which resolves, in order:
#
#   1. NAME            an inline value, as below
#   2. NAME_FILE       a path whose file contents are the secret
#   3. NAME_VAULT_REF  a reference an external resolver must materialise
#
# `cfg.validate_all()` applies a per-environment policy on top of that:
#
#   dev              any source; the inline values below are fine
#   staging / prod   an inline secret is REJECTED and startup fails
#
# Krab has no runtime vault client, so a NAME_VAULT_REF that is still set at
# startup is rejected outside dev as well — the reference is for a secrets
# operator to resolve into NAME_FILE before the process starts. NAME_FILE is
# therefore the promotion path: comment the inline line out and uncomment the
# _FILE line beside it.
#"#;

/// Render the `.env.example` for one template.
fn generate_env_example(name: &str, template: &ProjectTemplate) -> String {
    let header = format!(
        "# {name} environment template. Copy to .env for local development:\n\
         #\n\
         #     cp .env.example .env\n\
         #\n\
         # Every variable here is documented in the Krab reference under\n\
         # docs/reference/environment.md.\n\
         {SECRET_SOURCING_PREAMBLE}\n\
         \n\
         KRAB_ENVIRONMENT=dev\n\
         KRAB_HOST=0.0.0.0\n\
         KRAB_PORT=3000\n\
         \n\
         # Required in staging/prod: startup refuses wildcard CORS outside dev.\n\
         # KRAB_CORS_ORIGINS=https://app.example.com\n"
    );

    match template {
        ProjectTemplate::Saas => format!(
            r#"{header}
KRAB_AUTH_MODE=jwt
KRAB_OIDC_ISSUER=https://auth.example.com
KRAB_OIDC_AUDIENCE={name}

# Secret. Inline is dev-only — staging/prod reject it and require the _FILE
# form. Generate a value with: openssl rand -hex 32
KRAB_JWT_SECRET=dev-only-change-me
# KRAB_JWT_SECRET_FILE=/run/secrets/krab_jwt_secret
# KRAB_JWT_SECRET_VAULT_REF=kv/data/{name}/auth#jwt_secret

# Secret. Inline is dev-only — staging/prod reject it and require the _FILE
# form.
DATABASE_URL=postgres://localhost:5432/{name}
# DATABASE_URL_FILE=/run/secrets/{name}_database_url
# DATABASE_URL_VAULT_REF=kv/data/{name}/db#url
"#
        ),
        _ => format!(
            r#"{header}
# `static` is a dev-only auth mode: startup rejects it in staging/prod. Switch
# to KRAB_AUTH_MODE=jwt and configure a provider before promoting.
KRAB_AUTH_MODE=static

# Secret, and the one that has no promotion path: a shared bearer token must be
# unset outside dev whatever it is sourced from, so _FILE and _VAULT_REF do not
# make it acceptable in staging/prod.
# KRAB_BEARER_TOKEN=
"#
        ),
    }
}

/// Render `docs/render_policy.md` for templates that configure a render policy.
///
/// Returns `None` for every template that does not, rather than writing a
/// placeholder: the scaffold used to create `docs/` empty for all four, which
/// git dropped on the first clone while two reference-app READMEs went on
/// pointing at a `docs/render_policy.md` that had never been generated.
///
/// The content tracks [`generate_edge_ssr_main`] and
/// `krab_core::render_policy`; both are the source of truth for it.
fn generate_render_policy_doc(name: &str, metadata: TemplateMetadata) -> Option<String> {
    if !metadata.render_policy_doc {
        return None;
    }

    Some(format!(
        r##"# Render policy — {name}

This project declares one route render policy, in `src/main.rs`:

```rust
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
```

`RouteRenderPolicy` is `krab_core::render_policy::RouteRenderPolicy`. It is a
declaration of intent that the runtime and your deployment target read; it does
not by itself install caching or routing behaviour.

## What this policy says

| Field | Value here | Meaning |
| --- | --- | --- |
| `route_pattern` | `/` | The route the policy describes |
| `render_mode` | `RenderMode::Server` | Rendered per request on the server |
| `cache_mode` | `CacheMode::Isr {{ revalidate_after: 30s }}` | Rendered output is cached and considered stale 30 seconds after it was stored |
| `edge_capability` | `EdgeCapability::Eligible` | The route *may* run at an edge PoP |
| `streaming` | `true` | The route *may* stream its response |

`edge_capability` and `streaming` are declarations. Nothing in this starter
routes traffic to an edge, and `home_handler` returns a complete
`Html<String>`, so no response is streamed yet. Both fields exist so the
decision is recorded in code and can be validated before anything depends on
it.

## The full vocabulary

Pick from these when you add policies for your own routes:

| Enum | Variants |
| --- | --- |
| `RenderMode` | `Static`, `Server`, `ClientOnly` |
| `CacheMode` | `None`, `Static`, `Isr {{ revalidate_after }}`, `Swr {{ stale_after }}` |
| `EdgeCapability` | `OriginOnly`, `Eligible`, `Preferred`, `Required` |

`RouteRenderPolicy::new` defaults `edge_capability` to `OriginOnly` and
`streaming` to `false`; `with_edge_capability` and `with_streaming` override
them.

## Startup validation

`main` calls `home_render_policy().validates()` before it binds the listener and
returns an error if the combination is rejected — a contradictory policy stops
the process instead of misleading a cache. `validates()` rejects exactly these:

| Combination | Error |
| --- | --- |
| `Static` + `Isr` | `static_render_cannot_use_runtime_regeneration` |
| `Static` + `streaming` | `static_render_cannot_stream` |
| `ClientOnly` + `Isr` or `Swr` | `client_only_render_cannot_use_server_cache_revalidation` |
| `ClientOnly` + `streaming` | `client_only_render_cannot_stream` |

Everything else passes, including the `Server` + `Isr` + streaming combination
above.

## How ISR is actually served here

`cache_mode` records the intent; `home_handler` implements it against
`krab_core::isr::IsrCache`:

1. `isr_cache.serve("/")` is consulted first. A fresh entry is returned as-is.
2. A stale entry is returned immediately and re-rendered into the cache behind
   the response — stale-while-revalidate, so no request pays the render cost.
3. A miss renders, stores the result under
   `IsrPolicy::revalidate(HOME_REVALIDATE)`, and returns it.

A cache error degrades to a plain render; it never fails the request.
`HOME_REVALIDATE` and the `revalidate_after` in the policy are both 30 seconds
and are meant to stay in step — the policy is what a reader and any tooling
consult, the constant is what the cache enforces.

## Changing it

- Editing the policy alone changes what is declared, not what happens. Change
  `HOME_REVALIDATE` with `revalidate_after`, and the handler with `render_mode`.
- Adding a route: give it its own `RouteRenderPolicy`, call `validates()` on it
  at startup next to this one, and fail startup on the error.
- `CacheMode::Swr` and `CacheMode::Static` report `true` from
  `uses_distributed_cache()`, meaning they are meant to be backed by a shared
  store rather than the in-process `IsrCache` this starter uses.
"##
    ))
}

/// The `#[cfg(test)]` module appended to every generated `src/main.rs`.
///
/// The CI workflow `krab new` writes runs `cargo test`, and no test was ever
/// scaffolded — so the step passed by running zero tests while looking like a
/// gate. `/health` is the one assertion the scaffold can make that stays true
/// for any project built on it: the generated Kubernetes manifest polls it as
/// the liveness probe, so a handler that stops returning `status: ok` breaks
/// the deployment this repository ships.
///
/// It is a unit test inside the binary rather than a `tests/` integration test
/// because a binary crate exposes no library target — an integration test could
/// not reach the handler without restructuring the scaffold into lib + bin.
/// `#[tokio::test]` needs no new dependency: `tokio` is already a dependency
/// with `features = ["full"]`.
fn generate_health_smoke_test(handler: &str, service: Option<&str>) -> String {
    let service_assertion = service
        .map(|name| format!("        assert_eq!(body[\"service\"], \"{name}\");\n"))
        .unwrap_or_default();

    format!(
        r#"
#[cfg(test)]
mod tests {{
    use super::*;

    /// `/health` is the generated Kubernetes liveness probe's target. Keep this
    /// in step with `{handler}` if you change the response body.
    #[tokio::test]
    async fn health_endpoint_reports_ok() {{
        let Json(body) = {handler}().await;

{service_assertion}        assert_eq!(body["status"], "ok");
    }}
}}
"#
    )
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

// `krab gen` inserts module declarations (for example `mod routes;`) directly
// below this marker — leave the line in place.
// krab:modules

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
    // krab:routes
    // `krab gen route <name>` registers generated routers at the marker above,
    // as `let app = app.merge(...);` statements. The marker sits on the router
    // that is handed to `axum::serve`, and nothing is layered onto it
    // afterwards, so generated routes are treated exactly like the ones above.

    tracing::info!(service = "{name}", %addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}}
{tests}"#,
        tests = generate_health_smoke_test("health", None)
    )
}

fn rust_crate_name(name: &str) -> String {
    name.replace('-', "_")
}

fn generate_standard_cargo_toml(
    name: &str,
    metadata: TemplateMetadata,
    deps: &DependencySource,
) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
# The floor of the dependency set below, not a preference: `axum 0.8` declares
# `rust-version = "{msrv}"`. Because the versions below float to the latest
# compatible release, a dependency raising its own MSRV raises this one — Cargo
# will say so by name. Keep the Dockerfile's toolchain at or above this.
rust-version = "{msrv}"
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
        msrv = GENERATED_PROJECT_MSRV,
        template_desc = metadata.description,
        krab_core_dep = deps.render(
            "krab_core",
            "crates/framework/krab_core",
            metadata.extra_features
        ),
        krab_macros_dep = deps.render("krab_macros", "crates/framework/krab_macros", &[]),
        axum_dep = metadata.axum_dep,
        extra_deps = metadata.extra_deps
    )
}

fn generate_fullstack_cargo_toml(
    name: &str,
    metadata: TemplateMetadata,
    deps: &DependencySource,
) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
# The floor of the dependency set below, not a preference: `axum 0.8` declares
# `rust-version = "{msrv}"`. Because the versions below float to the latest
# compatible release, a dependency raising its own MSRV raises this one — Cargo
# will say so by name. Keep the Dockerfile's toolchain at or above this.
rust-version = "{msrv}"
description = "{template_desc}"

[lib]
crate-type = ["cdylib", "rlib"]

[[bin]]
name = "{name}"
path = "src/main.rs"

[dependencies]
{krab_core_base_dep}
{krab_macros_dep}
serde = {{ version = "1.0", features = ["derive"] }}
serde_json = "1.0"

# Server-side only. `rest` pulls axum, which does not build for wasm32, so the
# native and browser dependency sets are declared separately rather than
# feature-gated in one list.
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
{krab_core_native_dep}
axum = "0.8"
tokio = {{ version = "1.0", features = ["full"] }}
tower-http = {{ version = "0.6", features = ["fs"] }}
tracing = "0.1"
tracing-subscriber = {{ version = "0.3", features = ["json", "env-filter"] }}

# Browser only. `krab_client` takes `default-features = false` on purpose: its
# defaults include the deprecated `demo-islands` bundle, whose `Counter` /
# `Toggle` / `Likes` register in the same island registry your own `#[island]`
# components do, so leaving it on shadows a component of the same name.
[target.'cfg(target_arch = "wasm32")'.dependencies]
{krab_core_wasm_dep}
{krab_client_wasm_dep}
inventory = "0.3"
wasm-bindgen = "0.2"
wasm-bindgen-futures = "0.4"

[features]
default = []
web = []

# rustc 1.97 emits bulk-memory and nontrapping-fptoint by default; the binaryen
# wasm-pack bundles (version 117) rejects them unless told they exist.
[package.metadata.wasm-pack.profile.release]
wasm-opt = ["-O", "--enable-bulk-memory", "--enable-nontrapping-float-to-int"]
"#,
        msrv = GENERATED_PROJECT_MSRV,
        template_desc = metadata.description,
        krab_core_base_dep = deps.render("krab_core", "crates/framework/krab_core", &[]),
        krab_macros_dep = deps.render("krab_macros", "crates/framework/krab_macros", &[]),
        krab_core_native_dep = deps.render("krab_core", "crates/framework/krab_core", &["rest"]),
        krab_core_wasm_dep = deps.render("krab_core", "crates/framework/krab_core", &["web"]),
        krab_client_wasm_dep = deps.render_no_default_features(
            "krab_client",
            "crates/framework/krab_client",
            &["web"]
        ),
    )
}

/// Import block for the generated `src/main.rs`, sorted the way rustfmt sorts.
///
/// The crate's own `use {crate_name}::..` line lands in a name-dependent
/// position — `use axum_ish::..` sorts before `krab_core`, `use my_app::..`
/// after it. Emitting it at a fixed offset made every generated project fail
/// `cargo fmt --all --check`, which `generated-project.yaml` runs on all four
/// templates. Byte order over the whole `use` line matches rustfmt here: every
/// path segment is lowercase ASCII, and `{{` (0x7B) sorting after the letters is
/// what puts `use axum::{{Json, Router}};` last among the `axum` lines.
fn fullstack_main_imports(crate_name: &str) -> String {
    let mut lines = vec![
        "use axum::response::Html;".to_string(),
        "use axum::routing::{get, post};".to_string(),
        "use axum::{Json, Router};".to_string(),
        "use krab_core::config::KrabConfig;".to_string(),
        "use krab_core::telemetry::init_tracing;".to_string(),
        format!("use {crate_name}::{{greet_server_handler, render_home_page}};"),
        "use serde_json::json;".to_string(),
        "use std::net::SocketAddr;".to_string(),
        "use tower_http::services::ServeDir;".to_string(),
    ];
    lines.sort();
    lines.join("\n")
}

fn generate_fullstack_main(name: &str) -> String {
    let crate_name = rust_crate_name(name);
    let imports = fullstack_main_imports(&crate_name);
    format!(
        r#"{imports}

// `krab gen` inserts module declarations (for example `mod routes;`) directly
// below this marker — leave the line in place.
// krab:modules

async fn index() -> Html<String> {{
    Html(render_home_page("{name}"))
}}

async fn health() -> Json<serde_json::Value> {{
    Json(json!({{ "service": "{name}", "status": "ok" }}))
}}

async fn ready() -> Json<serde_json::Value> {{
    Json(json!({{ "service": "{name}", "status": "ready" }}))
}}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {{
    init_tracing("{name}");
    let cfg = KrabConfig::from_env_checked("{name}", 3000)?;
    cfg.validate_all()?;
    let addr: SocketAddr = format!("{{}}:{{}}", cfg.host, cfg.port).parse()?;

    // Serve WASM artifacts and static assets. `fallback` ensures /pkg requests
    // resolve whether output is placed under dist/ (from `krab build`) or pkg/ (direct wasm-pack).
    let pkg_service = ServeDir::new("dist").fallback(ServeDir::new("pkg"));
    let public_service = ServeDir::new("public");

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/api/rpc/greet_server", post(greet_server_handler))
        .nest_service("/pkg", pkg_service)
        .nest_service("/public", public_service);
    // krab:routes
    // `krab gen route <name>` registers generated routers at the marker above,
    // as `let app = app.merge(...);` statements. The marker sits on the router
    // that is handed to `axum::serve`, and nothing is layered onto it
    // afterwards, so generated routes are treated exactly like the ones above.

    tracing::info!(service = "{name}", %addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}}
{tests}"#,
        tests = generate_health_smoke_test("health", Some(name))
    )
}

fn generate_fullstack_lib(name: &str) -> String {
    let clean_name = rust_crate_name(name);
    format!(
        r#"//! Full-stack SSR + Islands + Server Functions for {name}.

#![allow(non_snake_case)]

use krab_core::action::create_action;
use krab_core::server_fn::ServerFnError;
use krab_core::signal::*;
use krab_core::{{IntoNode, Node, Render}};
use krab_macros::{{island, server, view}};
use serde::{{Deserialize, Serialize}};

// ---------------------------------------------------------------------------
// Server functions
// ---------------------------------------------------------------------------

/// Server function responding to client-side RPC calls.
///
/// Mounted at `POST /api/rpc/greet_server` on the server. On wasm32, calling
/// this function automatically issues an HTTP POST request to that endpoint.
#[server]
pub async fn greet_server(name: String) -> Result<String, ServerFnError> {{
    let trimmed = name.trim();
    if trimmed.is_empty() {{
        return Err(ServerFnError::validation("name must not be empty"));
    }}
    Ok(format!("Hello, {{trimmed}}! Response from server function."))
}}

// ---------------------------------------------------------------------------
// Islands
// ---------------------------------------------------------------------------

/// Props for [`Counter`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterProps {{
    pub initial: i32,
    pub step: i32,
}}

/// Interactive counter island.
///
/// Server-renders with hydration markers and hydrates in the browser.
/// Dispatches an action calling the [`greet_server`] server function on click.
#[island]
pub fn Counter(props: CounterProps) -> Node {{
    let (count, _set_count) = create_signal(props.initial);
    let _set_count_inc = _set_count.clone();
    let _set_count_dec = _set_count;
    let step = props.step;

    let _greet = create_action(|name: String| async move {{ greet_server(name).await }});

    view! {{
        <div class="island counter-island" data-testid="counter-island">
            <p class="counter-display">
                "Count: "
                <strong data-testid="count-value">
                    {{ move || count.get().to_string().into_node() }}
                </strong>
            </p>
            <div class="counter-actions">
                <button
                    class="btn btn-inc"
                    data-action="increment"
                    on:click={{
                        move |_| {{
                            _set_count_inc.update(|c| *c += step);
                            _greet.dispatch("Krab User".to_string());
                        }}
                    }}
                >
                    {{ format!("+{{step}}") }}
                </button>
                <button
                    class="btn btn-dec"
                    data-action="decrement"
                    on:click={{ move |_| _set_count_dec.update(|c| *c -= step) }}
                >
                    {{ format!("-{{step}}") }}
                </button>
            </div>
        </div>
    }}
}}

// ---------------------------------------------------------------------------
// Client entry point (WASM hydration)
// ---------------------------------------------------------------------------

/// Boot the browser hydration runtime and client router.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn krab_boot() {{
    krab_client::hydrate();
    krab_client::router::start();
}}

// ---------------------------------------------------------------------------
// Page rendering
// ---------------------------------------------------------------------------

/// The module script that initializes WASM and boots hydration.
const BOOT_SCRIPT: &str = "import init, {{ krab_boot }} from '/pkg/{clean_name}.js';\n\
     init().then(function () {{ krab_boot(); }});";

/// Build the home page [`Node`].
pub fn home_page_node(service_name: &str) -> Node {{
    let counter = Counter(CounterProps {{
        initial: 0,
        step: 1,
    }});

    view! {{
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <title>{{ format!("{{service_name}} — Krab Fullstack") }}</title>
                <style>
                    "body {{ font-family: system-ui, sans-serif; max-width: 720px; margin: 2rem auto; padding: 0 1rem; line-height: 1.5; }}\
                     .island {{ border: 2px dashed #e2e8f0; border-radius: 8px; padding: 1.5rem; margin: 1.5rem 0; background: #f8fafc; }}\
                     .counter-actions {{ display: flex; gap: 0.5rem; margin: 1rem 0; }}\
                     .btn {{ padding: 0.5rem 1rem; font-size: 1rem; border-radius: 4px; border: 1px solid #cbd5e1; background: #fff; cursor: pointer; }}\
                     .btn:hover {{ background: #f1f5f9; }}"
                </style>
            </head>
            <body>
                <header>
                    <h1>"🦀 " {{ service_name.to_string() }}</h1>
                    <p>"Full-stack server-side rendering with WASM island hydration."</p>
                </header>
                <main data-krab-router-outlet="main" tabindex="-1">
                    <h2>"Interactive Island"</h2>
                    <p>"The counter below is rendered on the server and hydrated in WebAssembly:"</p>
                    {{ counter }}
                </main>
                <script type="module">
                    {{ BOOT_SCRIPT.to_string() }}
                </script>
            </body>
        </html>
    }}
}}

/// Render the home page document to an HTML string.
pub fn render_home_page(service_name: &str) -> String {{
    format!("<!doctype html>{{}}", home_page_node(service_name).render())
}}

#[cfg(test)]
mod tests {{
    use super::*;

    #[test]
    fn home_page_renders_hydration_markers_and_boot_script() {{
        let html = render_home_page("{name}");
        assert!(html.contains("counter-island"));
        assert!(html.contains("/pkg/{clean_name}.js"));
        assert!(html.contains("krab_boot"));
    }}

    #[tokio::test]
    async fn greet_server_validates_input() {{
        let result = greet_server("Ferris".to_string()).await;
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            "Hello, Ferris! Response from server function."
        );

        let empty = greet_server("   ".to_string()).await;
        assert!(empty.is_err());
    }}
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

// `krab gen` inserts module declarations (for example `mod routes;`) directly
// below this marker — leave the line in place.
// krab:modules

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
        runtime: RuntimeState::try_new()?, // fails closed on Redis init failure outside dev
    }};

    let app: Router<AppState> = Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/api/v1/tenants", get(tenants_handler));
    // krab:routes
    // `krab gen route <name>` registers generated routers at the marker above,
    // as `let app = app.merge(...);` statements. The marker sits here, while
    // `app` is still `Router<AppState>`, so generated routes receive the same
    // common HTTP layers and state as the routes above. Merging after
    // `apply_common_http_layers` would silently exempt them from every one.

    let app = apply_common_http_layers(app, state.clone()).with_state(state);

    tracing::info!(service = "{name}", %addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}}
{tests}"#,
        tests = generate_health_smoke_test("health_handler", Some(name))
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

// `krab gen` inserts module declarations (for example `mod routes;`) directly
// below this marker — leave the line in place.
// krab:modules

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

    let app: Router<AppState> = Router::new()
        .route("/", get(home_handler))
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler));
    // krab:routes
    // `krab gen route <name>` registers generated routers at the marker above,
    // as `let app = app.merge(...);` statements. The marker sits here, before
    // `with_state`, so generated routes are given the same state as the routes
    // above rather than being merged into an already-finalised router.

    let app = app.with_state(state);

    tracing::info!(service = "{name}", %addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}}
{tests}"###,
        tests = generate_health_smoke_test("health_handler", Some(name))
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

// `krab gen` inserts module declarations (for example `mod routes;`) directly
// below this marker — leave the line in place.
// krab:modules

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
    // krab:routes
    // `krab gen route <name>` registers generated routers at the marker above,
    // as `let app = app.merge(...);` statements. The marker sits on the router
    // that is handed to `axum::serve`, and nothing is layered onto it
    // afterwards, so generated routes are treated exactly like the ones above.

    tracing::info!(service = "{name}", %addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}}
{tests}"#,
        tests = generate_health_smoke_test("health_handler", Some(name))
    )
}

/// Render the CI workflow a generated project ships.
///
/// This used to take the template as well, to append a `saas`-only step:
///
/// ```yaml
/// - name: Run database migration checks
///   run: cargo test --package <name> -- db_
///   env:
///     DATABASE_URL: postgres://localhost:5432/<name>_test
/// ```
///
/// It asserted nothing. The filter matched no test — the scaffold has no `db_`
/// test and no migrations to test — so it exited 0 by running zero tests, and
/// the `DATABASE_URL` it exported pointed at a Postgres the workflow never
/// started, so even a real test would not have connected. Making it honest
/// would mean scaffolding a migration, a `db_`-prefixed test, and a service
/// container, which would leave a starter whose `cargo test` fails on any
/// machine without a database. It is deleted instead; add it back alongside
/// your first migration.
fn generate_ci_workflow(name: &str) -> String {
    format!(
        r#"name: {name} CI

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
        run: cargo clippy --all-targets -- -D warnings

      # `src/main.rs` carries a smoke test over the `/health` handler the
      # Kubernetes liveness probe in deploy/kubernetes.yaml polls.
      - name: Run tests
        run: cargo test
        env:
          KRAB_ENVIRONMENT: dev
          KRAB_AUTH_MODE: static

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
        ProjectTemplate::Fullstack => ("2", "256Mi"),
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
#
# The `1` tag is the latest stable Rust, deliberately. The manifest floats its
# dependencies (`axum = "0.8"`, `tokio = "1.0"`, ...) to the newest compatible
# release, so a pinned old toolchain breaks this build the first time any of
# them raises its MSRV — with an error inside a dependency that looks nothing
# like the cause. Cargo.toml declares `rust-version = "{msrv}"` as the floor;
# pin this image to a specific tag at or above it once you commit a Cargo.lock
# and want byte-reproducible builds.
FROM rust:1-slim-bookworm AS builder
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
"#,
        msrv = GENERATED_PROJECT_MSRV
    )
}

fn generate_project_toml(name: &str, template: &ProjectTemplate) -> String {
    let clean_name = rust_crate_name(name);
    let client_section = if *template == ProjectTemplate::Fullstack {
        format!(
            r#"client_package = "{name}"
client_crate_dir = "."
client_artifact_stem = "{clean_name}"
"#
        )
    } else {
        String::new()
    };

    format!(
        r#"[project]
frontend_bin = "{name}"
{client_section}public_dir = "public"
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
        init_git_repository, suggested_project_slug, validate_project_name,
        write_project_from_template, DependencySource, GitInitOutcome, FRAMEWORK_VERSION,
        GENERATED_PROJECT_MSRV,
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

    /// Fail if any directory under `dir` (including `dir`) has no entries.
    ///
    /// Git cannot represent an empty directory, so one that survives generation
    /// is a directory the first `git clone` of the scaffolded project will not
    /// have.
    fn assert_no_empty_directories(dir: &Path) -> Result<()> {
        let mut entries = fs::read_dir(dir)?.peekable();
        assert!(
            entries.peek().is_some(),
            "{} is empty; git will not preserve it",
            dir.display()
        );
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                assert_no_empty_directories(&entry.path())?;
            }
        }
        Ok(())
    }

    fn assert_common_scaffold(
        project_dir: &Path,
        name: &str,
        template: &ProjectTemplate,
    ) -> Result<()> {
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
            // The Dockerfile does `COPY public/ public/`. Git does not track
            // empty directories, so without this file the directory is gone
            // after a clone and the container build fails.
            "public/.gitkeep",
        ] {
            assert!(
                project_dir.join(relative).exists(),
                "expected {relative} to be generated"
            );
        }

        if *template == ProjectTemplate::Fullstack {
            assert!(
                project_dir.join("src/lib.rs").exists(),
                "expected src/lib.rs to be generated for fullstack template"
            );
        }

        // Directories the scaffolder never populated used to be created anyway;
        // they disappeared on clone and misled users into thinking they were
        // wired up. `src/routes/` is created on demand by `krab gen route`.
        // `docs/` is created only where a document is written into it — for
        // `edge-ssr`, which has a render policy worth documenting.
        let mut never_populated = vec!["src/routes", "src/api"];
        if *template != ProjectTemplate::EdgeSsr {
            never_populated.push("docs");
        }
        for relative in never_populated {
            assert!(
                !project_dir.join(relative).exists(),
                "{relative} is created but never populated; git will not preserve it"
            );
        }

        assert_no_empty_directories(project_dir)?;

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

    const ALL_TEMPLATES: [ProjectTemplate; 5] = [
        ProjectTemplate::Default,
        ProjectTemplate::Saas,
        ProjectTemplate::EdgeSsr,
        ProjectTemplate::EventStream,
        ProjectTemplate::Fullstack,
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
        let mut table = parsed
            .get("dependencies")
            .and_then(|d| d.as_table())
            .cloned()
            .unwrap_or_default();
        if let Some(target) = parsed.get("target").and_then(|t| t.as_table()) {
            if let Some(native) = target
                .get("cfg(not(target_arch = \"wasm32\"))")
                .and_then(|n| n.get("dependencies"))
                .and_then(|d| d.as_table())
            {
                for (k, v) in native {
                    table.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        table
    }

    fn collect_all_krab_core_features(project_dir: &Path) -> Vec<String> {
        let raw = fs::read_to_string(project_dir.join("Cargo.toml"))
            .expect("generated Cargo.toml unreadable");
        let parsed: toml::Value = toml::from_str(&raw)
            .unwrap_or_else(|e| panic!("generated Cargo.toml is not valid TOML: {e}\n{raw}"));
        let mut features = Vec::new();

        if let Some(deps) = parsed.get("dependencies").and_then(|d| d.as_table()) {
            if let Some(kc) = deps.get("krab_core").and_then(|d| d.as_table()) {
                if let Some(arr) = kc.get("features").and_then(|f| f.as_array()) {
                    for f in arr {
                        if let Some(s) = f.as_str() {
                            features.push(s.to_string());
                        }
                    }
                }
            }
        }

        if let Some(target) = parsed.get("target").and_then(|t| t.as_table()) {
            for (_, target_val) in target {
                if let Some(deps) = target_val.get("dependencies").and_then(|d| d.as_table()) {
                    if let Some(kc) = deps.get("krab_core").and_then(|d| d.as_table()) {
                        if let Some(arr) = kc.get("features").and_then(|f| f.as_array()) {
                            for f in arr {
                                if let Some(s) = f.as_str() {
                                    features.push(s.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        features
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
            let requested = collect_all_krab_core_features(&project_dir);

            assert!(
                !requested.is_empty(),
                "{template:?}: krab_core has no features; it has no default features either"
            );

            for name in requested {
                assert!(
                    declared.iter().any(|d| d == &name),
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
        let workflow = generate_ci_workflow("demo");

        assert!(workflow.contains("EmbarkStudios/cargo-deny-action@v2"));
        assert!(!workflow.contains("cargo install cargo-deny"));
        assert!(workflow.contains("cargo fmt --all --check"));
        assert!(workflow.contains("cargo test"));
    }

    /// The `saas` workflow carried a step that asserted nothing: `cargo test
    /// --package <name> -- db_` matched no test (none is scaffolded, and there
    /// are no migrations), so it exited 0 having run zero tests, and its
    /// `DATABASE_URL` pointed at a Postgres no step ever started. A step that
    /// cannot fail is worse than no step — it reads as a gate.
    #[test]
    fn generated_ci_has_no_step_that_cannot_fail() {
        for name in ["demo", "demo-saas"] {
            let workflow = generate_ci_workflow(name);

            assert!(
                !workflow.contains("-- db_"),
                "{name}: the db_ test filter matches nothing the scaffold generates"
            );
            assert!(
                !workflow.contains("DATABASE_URL"),
                "{name}: DATABASE_URL is exported for a database no step starts"
            );
            assert!(
                !workflow.contains("services:"),
                "{name}: a service container is declared but nothing uses it"
            );
        }
    }

    /// Every step must be reachable from the template-independent workflow now
    /// that `generate_ci_workflow` no longer varies by template.
    #[test]
    fn generated_ci_is_identical_for_every_template() -> Result<()> {
        let mut rendered: Vec<String> = Vec::new();
        for template in ALL_TEMPLATES {
            let (_temp, project_dir) = generate_fixture("demo-ci", &template)?;
            rendered.push(fs::read_to_string(
                project_dir.join(".github/workflows/ci.yaml"),
            )?);
        }
        assert!(
            rendered.windows(2).all(|pair| pair[0] == pair[1]),
            "the generated workflow still differs per template"
        );
        Ok(())
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
        assert_common_scaffold(&project_dir, "demo-default", &ProjectTemplate::Default)?;

        let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;
        assert!(main_rs.contains(r#".route("/", get"#));
        assert!(main_rs.contains(r#""service": "demo-default""#));
        assert!(main_rs.contains(r#""status": "ready""#));
        Ok(())
    }

    #[test]
    fn saas_template_smoke_marks_scope_as_scaffold() -> Result<()> {
        let (_temp_dir, project_dir) = generate_fixture("demo-saas", &ProjectTemplate::Saas)?;
        assert_common_scaffold(&project_dir, "demo-saas", &ProjectTemplate::Saas)?;

        let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;
        assert!(main_rs.contains("apply_common_http_layers"));
        assert!(main_rs.contains("/api/v1/tenants"));

        let readme = fs::read_to_string(project_dir.join("README.md"))?;
        assert!(readme.contains("SaaS service skeleton"));
        assert!(readme.contains("does not include a completed auth flow"));

        // DATABASE_URL belongs in the environment template, where it configures
        // something — not in the CI workflow, where it used to point at a
        // database no step started. See `generated_ci_has_no_step_that_cannot_fail`.
        let env_example = fs::read_to_string(project_dir.join(".env.example"))?;
        assert!(env_example.contains("DATABASE_URL=postgres://"));
        Ok(())
    }

    #[test]
    fn edge_ssr_template_smoke_calls_out_policy_scope() -> Result<()> {
        let (_temp_dir, project_dir) = generate_fixture("demo-edge", &ProjectTemplate::EdgeSsr)?;
        assert_common_scaffold(&project_dir, "demo-edge", &ProjectTemplate::EdgeSsr)?;

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
        assert_common_scaffold(&project_dir, "demo-stream", &ProjectTemplate::EventStream)?;

        let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;
        assert!(main_rs.contains("/api/events"));
        assert!(main_rs.contains("/api/ws"));
        assert!(main_rs.contains("tokio_stream"));
        Ok(())
    }

    #[test]
    fn fullstack_template_smoke_generates_expected_files() -> Result<()> {
        let (_temp_dir, project_dir) =
            generate_fixture("demo-fullstack", &ProjectTemplate::Fullstack)?;
        assert_common_scaffold(&project_dir, "demo-fullstack", &ProjectTemplate::Fullstack)?;

        let lib_rs = fs::read_to_string(project_dir.join("src/lib.rs"))?;
        assert!(lib_rs.contains("#[island]"));
        assert!(lib_rs.contains("#[server]"));
        assert!(lib_rs.contains("krab_boot"));
        assert!(lib_rs.contains("render_home_page"));

        let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;
        assert!(main_rs.contains("demo_fullstack"));
        assert!(main_rs.contains("greet_server_handler"));
        assert!(main_rs.contains("ServeDir::new(\"dist\")"));
        assert!(main_rs.contains("/api/rpc/greet_server"));

        let krab_toml = fs::read_to_string(project_dir.join("krab.toml"))?;
        assert!(krab_toml.contains("client_package = \"demo-fullstack\""));
        assert!(krab_toml.contains("client_artifact_stem = \"demo_fullstack\""));
        Ok(())
    }

    /// `krab gen route foo` writes `src/routes/foo.rs`, which is dead weight
    /// unless something declares the module and merges the router. `krab gen`
    /// finds its insertion points by these two marker lines, so every template
    /// has to emit both, exactly, once.
    #[test]
    fn every_template_emits_module_and_route_markers() -> Result<()> {
        for template in ALL_TEMPLATES {
            let (_temp, project_dir) = generate_fixture("demo-markers", &template)?;
            let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;

            let module_marker = main_rs
                .lines()
                .filter(|line| *line == "// krab:modules")
                .count();
            assert_eq!(
                module_marker, 1,
                "{template:?}: expected exactly one `// krab:modules` line, found \
                 {module_marker}\n{main_rs}"
            );

            let route_marker = main_rs
                .lines()
                .filter(|line| *line == "    // krab:routes")
                .count();
            assert_eq!(
                route_marker, 1,
                "{template:?}: expected exactly one `    // krab:routes` line (four-space \
                 indent, inside `main`), found {route_marker}\n{main_rs}"
            );

            let lines: Vec<&str> = main_rs.lines().collect();
            let module_at = lines
                .iter()
                .position(|line| *line == "// krab:modules")
                .expect("module marker located above");
            let route_at = lines
                .iter()
                .position(|line| *line == "    // krab:routes")
                .expect("route marker located above");

            // The module marker sits at item position: after the `use` block,
            // before the first item.
            let last_use = lines
                .iter()
                .rposition(|line| line.starts_with("use "))
                .unwrap_or_else(|| panic!("{template:?}: generated main.rs has no `use` block"));
            assert!(
                module_at > last_use,
                "{template:?}: `// krab:modules` at line {module_at} precedes the end of the \
                 use block at line {last_use}"
            );
            assert!(
                module_at < route_at,
                "{template:?}: markers are out of order"
            );

            // The route marker sits where `app` is a fully built router, so an
            // inserted `let app = app.merge(...);` type-checks.
            let main_fn = lines
                .iter()
                .position(|line| line.starts_with("async fn main("))
                .unwrap_or_else(|| {
                    panic!("{template:?}: generated main.rs has no `async fn main`")
                });
            let serve_at = lines
                .iter()
                .position(|line| line.contains("axum::serve(listener, app)"))
                .unwrap_or_else(|| panic!("{template:?}: generated main.rs never serves `app`"));
            assert!(
                route_at > main_fn && route_at < serve_at,
                "{template:?}: `    // krab:routes` at line {route_at} is not between \
                 `async fn main` ({main_fn}) and `axum::serve` ({serve_at})"
            );
            assert!(
                lines[route_at - 1].trim_end().ends_with(';'),
                "{template:?}: `    // krab:routes` must follow a completed statement, but the \
                 preceding line is {:?}",
                lines[route_at - 1]
            );
        }
        Ok(())
    }

    /// `public/` is the one empty directory the scaffold genuinely needs: the
    /// generated Dockerfile copies it.
    #[test]
    fn public_directory_is_pinned_for_the_dockerfile_copy() -> Result<()> {
        let (_temp, project_dir) = generate_fixture("demo-public", &ProjectTemplate::Default)?;

        let gitkeep = fs::read_to_string(project_dir.join("public/.gitkeep"))?;
        assert!(
            !gitkeep.trim().is_empty(),
            "public/.gitkeep is empty; it must explain why it exists"
        );
        assert!(
            gitkeep.contains("COPY public/ public/"),
            "public/.gitkeep does not name the Dockerfile step it protects: {gitkeep:?}"
        );

        let dockerfile = fs::read_to_string(project_dir.join("Dockerfile"))?;
        assert!(dockerfile.contains("COPY public/ public/"));
        Ok(())
    }

    /// The `saas` template applies `apply_common_http_layers` — rate limiting,
    /// auth, tracing, the lot. The route marker sat *after* that line, so every
    /// route added by `krab gen route` merged into an already-layered router
    /// and silently bypassed all of it, while the hand-written routes beside it
    /// kept them. Nothing failed; the routes were just unprotected.
    #[test]
    fn saas_routes_marker_precedes_the_common_http_layers() -> Result<()> {
        let (_temp, project_dir) = generate_fixture("demo-layers", &ProjectTemplate::Saas)?;
        let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;
        let lines: Vec<&str> = main_rs.lines().collect();

        let marker_at = lines
            .iter()
            .position(|line| *line == "    // krab:routes")
            .expect("the route marker is generated");
        let layers_at = lines
            .iter()
            .position(|line| line.contains("apply_common_http_layers("))
            .expect("the saas template applies the common layers");

        assert!(
            marker_at < layers_at,
            "routes merged at line {marker_at} would bypass the layers applied at \
             line {layers_at}:\n{main_rs}"
        );
        // And the merge has to happen while `app` still carries the state type,
        // or `Router::merge` will not accept the generated `router::<S>()`.
        let typed_at = lines
            .iter()
            .position(|line| line.contains("let app: Router<AppState>"))
            .expect("the router binding is explicitly typed");
        assert!(typed_at < marker_at, "{main_rs}");
        Ok(())
    }

    /// Same reasoning for `edge-ssr`: `with_state` finalises the router, so a
    /// merge after it lands on a `Router<()>` that can never see `AppState`.
    #[test]
    fn edge_ssr_routes_marker_precedes_with_state() -> Result<()> {
        let (_temp, project_dir) = generate_fixture("demo-state", &ProjectTemplate::EdgeSsr)?;
        let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;
        let lines: Vec<&str> = main_rs.lines().collect();

        let marker_at = lines
            .iter()
            .position(|line| *line == "    // krab:routes")
            .expect("the route marker is generated");
        let with_state_at = lines
            .iter()
            .position(|line| line.contains("let app = app.with_state(state);"))
            .expect("edge-ssr installs its state in a separate statement");

        assert!(marker_at < with_state_at, "{main_rs}");
        Ok(())
    }

    /// The `default` and `event-stream` templates layer nothing onto `app`, so
    /// the marker only has to sit on the router `axum::serve` is handed —
    /// covered by `every_template_emits_module_and_route_markers`. What must
    /// hold everywhere is that nothing reassigns `app` between the marker and
    /// `axum::serve` without the merge having happened first.
    #[test]
    fn nothing_rebinds_app_between_the_marker_and_serve_except_declared_finalisers() -> Result<()> {
        // Statements that are allowed to follow the marker, because merged
        // routes go through them exactly as the built-in routes do.
        const FINALISERS: &[&str] = &["apply_common_http_layers(", "app.with_state("];

        for template in ALL_TEMPLATES {
            let (_temp, project_dir) = generate_fixture("demo-tail", &template)?;
            let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;
            let lines: Vec<&str> = main_rs.lines().collect();

            let marker_at = lines
                .iter()
                .position(|line| *line == "    // krab:routes")
                .expect("the route marker is generated");
            let serve_at = lines
                .iter()
                .position(|line| line.contains("axum::serve(listener, app)"))
                .expect("app is served");

            for line in &lines[marker_at + 1..serve_at] {
                if !line.trim_start().starts_with("let app") {
                    continue;
                }
                assert!(
                    FINALISERS.iter().any(|f| line.contains(f)),
                    "{template:?}: `{}` rebinds `app` after the route marker but is not a \
                     declared finaliser, so merged routes are treated differently from the \
                     built-in ones",
                    line.trim()
                );
            }
        }
        Ok(())
    }

    /// The generated CI ran `cargo test` against a project with no tests: the
    /// step passed by running zero of them.
    #[test]
    fn every_template_scaffolds_a_health_smoke_test() -> Result<()> {
        for (template, handler) in [
            (ProjectTemplate::Default, "health"),
            (ProjectTemplate::Saas, "health_handler"),
            (ProjectTemplate::EdgeSsr, "health_handler"),
            (ProjectTemplate::EventStream, "health_handler"),
            (ProjectTemplate::Fullstack, "health"),
        ] {
            let (_temp, project_dir) = generate_fixture("demo-smoke", &template)?;
            let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;

            assert!(main_rs.contains("#[cfg(test)]"), "{template:?}:\n{main_rs}");
            assert!(
                main_rs.contains("async fn health_endpoint_reports_ok()"),
                "{template:?}:\n{main_rs}"
            );
            // It must exercise the handler the route table actually registers.
            assert!(
                main_rs.contains(&format!("let Json(body) = {handler}().await;")),
                "{template:?}: the smoke test does not call `{handler}`:\n{main_rs}"
            );
            assert!(
                main_rs.contains(&format!(r#".route("/health", get({handler}))"#)),
                "{template:?}: `{handler}` is not the handler mounted on /health:\n{main_rs}"
            );
            // No new dependency: tokio is already declared with `features = ["full"]`.
            let deps = generated_dependencies(&project_dir);
            assert!(
                deps.contains_key("tokio"),
                "{template:?}: the smoke test needs #[tokio::test]"
            );
        }
        Ok(())
    }

    /// The scaffold shipped `KRAB_JWT_SECRET=change-me-in-production` beside
    /// `KRAB_SECRETS_SOURCE=env` — a variable no Krab code reads and no
    /// reference page documents — teaching a configuration that
    /// `KrabConfig::validate_all()` refuses to start on in staging/prod.
    #[test]
    fn env_example_teaches_a_promotable_secret_configuration() -> Result<()> {
        for template in ALL_TEMPLATES {
            let (_temp, project_dir) = generate_fixture("demo-env", &template)?;
            let env_example = fs::read_to_string(project_dir.join(".env.example"))?;

            assert!(
                !env_example.contains("KRAB_SECRETS_SOURCE"),
                "{template:?}: KRAB_SECRETS_SOURCE is read by nothing in krab_core and is \
                 documented nowhere:\n{env_example}"
            );
            assert!(
                !env_example.contains("change-me-in-production"),
                "{template:?}: the inline value advertises itself as the production value, \
                 but staging/prod reject inline secrets outright:\n{env_example}"
            );
            // The policy has to be stated, not implied.
            assert!(
                env_example.contains("staging / prod   an inline secret is REJECTED"),
                "{template:?}: the secret-sourcing policy is not stated:\n{env_example}"
            );
            assert!(
                env_example.contains("KRAB_ENVIRONMENT=dev"),
                "{template:?}: the inline values are only usable under dev:\n{env_example}"
            );
        }

        // Every secret the saas template sets inline must carry its promotion
        // path directly beside it.
        let (_temp, project_dir) = generate_fixture("demo-secrets", &ProjectTemplate::Saas)?;
        let env_example = fs::read_to_string(project_dir.join(".env.example"))?;
        for secret in ["KRAB_JWT_SECRET", "DATABASE_URL"] {
            assert!(
                env_example.contains(&format!("{secret}=")),
                "{secret} is not set inline for dev:\n{env_example}"
            );
            assert!(
                env_example.contains(&format!("# {secret}_FILE=")),
                "{secret} has no commented _FILE alternative:\n{env_example}"
            );
            assert!(
                env_example.contains(&format!("# {secret}_VAULT_REF=")),
                "{secret} has no commented _VAULT_REF alternative:\n{env_example}"
            );
        }
        Ok(())
    }

    /// Every variable the scaffold emits has to exist in the framework's own
    /// reference page — CLAUDE.md convention 4. Read from the file so adding an
    /// undocumented knob to a template fails here.
    #[test]
    fn every_env_example_variable_is_documented() -> Result<()> {
        let reference = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../docs/reference/environment.md")
            .canonicalize()
            .expect("docs/reference/environment.md not found");
        let documented = fs::read_to_string(reference)?;

        for template in ALL_TEMPLATES {
            let (_temp, project_dir) = generate_fixture("demo-doc", &template)?;
            let env_example = fs::read_to_string(project_dir.join(".env.example"))?;

            for line in env_example.lines() {
                // Assignments, including the commented-out alternatives.
                let candidate = line.trim().trim_start_matches("# ").trim();
                let Some((var, _)) = candidate.split_once('=') else {
                    continue;
                };
                if var.is_empty()
                    || !var
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                {
                    continue;
                }
                // `_FILE` / `_VAULT_REF` are documented as a suffix convention
                // that applies to every variable marked **secret**.
                let base = var
                    .strip_suffix("_VAULT_REF")
                    .or_else(|| var.strip_suffix("_FILE"))
                    .unwrap_or(var);
                assert!(
                    documented.contains(base),
                    "{template:?}: .env.example sets {var}, which \
                     docs/reference/environment.md does not document"
                );
            }
        }
        Ok(())
    }

    /// The Dockerfile pinned `rust:1.77-slim-bookworm` while the manifest
    /// floated every dependency to latest. `axum 0.8` already declares
    /// `rust-version = "1.78"`, so the container build broke on a toolchain
    /// error that named none of that.
    #[test]
    fn dockerfile_toolchain_is_not_older_than_the_declared_msrv() -> Result<()> {
        for template in ALL_TEMPLATES {
            let (_temp, project_dir) = generate_fixture("demo-msrv", &template)?;

            let dockerfile = fs::read_to_string(project_dir.join("Dockerfile"))?;
            let from = dockerfile
                .lines()
                .find(|line| line.starts_with("FROM rust:"))
                .unwrap_or_else(|| panic!("{template:?}: no rust builder stage:\n{dockerfile}"));
            assert!(
                !from.contains("rust:1.7"),
                "{template:?}: {from:?} pins a toolchain older than the floating \
                 dependency set needs"
            );
            assert!(
                from.starts_with("FROM rust:1-"),
                "{template:?}: {from:?} should track the latest stable 1.x"
            );

            // And the manifest states the floor, so a mismatch is Cargo's own
            // error naming the package, not a type error inside a dependency.
            let manifest: toml::Value =
                toml::from_str(&fs::read_to_string(project_dir.join("Cargo.toml"))?)?;
            assert_eq!(
                manifest
                    .get("package")
                    .and_then(|p| p.get("rust-version"))
                    .and_then(|v| v.as_str()),
                Some(GENERATED_PROJECT_MSRV),
                "{template:?}: the generated manifest declares no MSRV"
            );
        }
        Ok(())
    }

    /// Two reference-app READMEs told users to read `docs/render_policy.md` in
    /// their scaffolded project. It had never been generated — `krab new`
    /// created `docs/` empty, and once the empty directory was removed the
    /// reference dangled permanently.
    #[test]
    fn edge_ssr_documents_the_render_policy_it_configures() -> Result<()> {
        let (_temp, project_dir) = generate_fixture("demo-policy", &ProjectTemplate::EdgeSsr)?;
        let doc = fs::read_to_string(project_dir.join("docs/render_policy.md"))?;
        let main_rs = fs::read_to_string(project_dir.join("src/main.rs"))?;

        // Everything the document claims the template configures, it configures.
        for claim in [
            "RenderMode::Server",
            "CacheMode::Isr",
            "EdgeCapability::Eligible",
            "with_streaming(true)",
            "home_render_policy",
            "IsrPolicy::revalidate",
        ] {
            assert!(doc.contains(claim), "the document omits {claim}:\n{doc}");
            assert!(
                main_rs.contains(claim),
                "the document names {claim}, which the template does not use"
            );
        }
        // The startup validation table must match `RouteRenderPolicy::validates`.
        for error in [
            "static_render_cannot_use_runtime_regeneration",
            "static_render_cannot_stream",
            "client_only_render_cannot_use_server_cache_revalidation",
            "client_only_render_cannot_stream",
        ] {
            assert!(doc.contains(error), "the document omits {error}:\n{doc}");
        }
        // The edge_rendered reference README sends readers here to choose
        // between the render and cache modes, so all of them must be listed.
        for variant in ["`None`", "`Swr {{ stale_after }}`", "`OriginOnly`"] {
            assert!(
                doc.contains(variant.replace("{{", "{").replace("}}", "}").as_str()),
                "the document omits {variant}:\n{doc}"
            );
        }

        // And it is discoverable.
        let readme = fs::read_to_string(project_dir.join("README.md"))?;
        assert!(readme.contains("docs/render_policy.md"), "{readme}");
        Ok(())
    }

    /// Only `edge-ssr` configures a render policy, so only `edge-ssr` gets the
    /// document — recreating an empty `docs/` for the rest would put back the
    /// directory git drops on clone.
    #[test]
    fn other_templates_get_no_docs_directory() -> Result<()> {
        for template in [
            ProjectTemplate::Default,
            ProjectTemplate::Saas,
            ProjectTemplate::EventStream,
            ProjectTemplate::Fullstack,
        ] {
            let (_temp, project_dir) = generate_fixture("demo-nodocs", &template)?;
            assert!(
                !project_dir.join("docs").exists(),
                "{template:?} has no render policy to document"
            );
            let readme = fs::read_to_string(project_dir.join("README.md"))?;
            assert!(
                !readme.contains("docs/render_policy.md"),
                "{template:?}: the README points at a file this template does not write"
            );
        }
        Ok(())
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// `krab new` wrote a `.gitignore` for a repository it never created.
    #[test]
    fn git_init_creates_a_repository_without_committing() -> Result<()> {
        if !git_available() {
            eprintln!("git is not on PATH; skipping the git init gate");
            return Ok(());
        }
        let temp = TempDir::new()?;
        let project_dir = temp.path().join("demo-git");
        fs::create_dir(&project_dir)?;

        let outcome = init_git_repository(&project_dir);
        if outcome == GitInitOutcome::AlreadyInWorkTree {
            eprintln!("the temp directory is inside a git work tree; skipping");
            return Ok(());
        }

        assert_eq!(outcome, GitInitOutcome::Initialised);
        assert!(
            project_dir.join(".git").exists(),
            "no repository was created"
        );

        // Initialised, not committed: what to commit and under whose identity
        // is the user's decision, and a commit needs a configured identity that
        // a fresh machine may not have.
        let head = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "HEAD"])
            .current_dir(&project_dir)
            .output()?;
        assert!(
            !head.status.success(),
            "a commit was created: {}",
            String::from_utf8_lossy(&head.stdout)
        );
        Ok(())
    }

    /// Scaffolding inside an existing checkout must not nest a repository —
    /// the `generated-project` gate uses `$RUNNER_TEMP`, but a developer
    /// trying a template out will often do it inside a clone.
    #[test]
    fn git_init_refuses_to_nest_inside_an_existing_work_tree() -> Result<()> {
        if !git_available() {
            eprintln!("git is not on PATH; skipping the git init gate");
            return Ok(());
        }
        let temp = TempDir::new()?;
        let outer = temp.path().join("outer");
        fs::create_dir(&outer)?;
        let init = std::process::Command::new("git")
            .arg("init")
            .current_dir(&outer)
            .output()?;
        assert!(init.status.success(), "could not set up the fixture");

        let nested = outer.join("demo-nested");
        fs::create_dir(&nested)?;

        assert_eq!(
            init_git_repository(&nested),
            GitInitOutcome::AlreadyInWorkTree
        );
        assert!(
            !nested.join(".git").exists(),
            "a repository was nested inside an existing checkout"
        );
        Ok(())
    }

    /// A missing or failing `git` must never fail the scaffold.
    #[test]
    fn a_git_failure_is_reported_but_never_fatal() -> Result<()> {
        let temp = TempDir::new()?;
        // A path that does not exist makes `Command::current_dir` fail to
        // spawn, which is the same error shape as `git` missing from PATH.
        let missing = temp.path().join("not-created");

        match init_git_repository(&missing) {
            GitInitOutcome::Unavailable(reason) => {
                assert!(!reason.is_empty(), "the warning must say what went wrong")
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn valid_project_names_are_accepted() {
        for name in [
            "demo", "demo-app", "demo_app", "krab2", "_private", "a", "Demo-App",
            "consul",   // only exact Windows device names are reserved
            "constant", // ditto; `con` as a prefix is fine
            "testing",  // only bare `test` collides with the built-in crate
        ] {
            assert!(
                validate_project_name(name).is_ok(),
                "{name:?} should be accepted: {:?}",
                validate_project_name(name).unwrap_err().to_string()
            );
        }
    }

    /// Every rejection has to name the problem and offer something that works —
    /// the previous behaviour deferred the failure to the user's first
    /// `cargo run`, with a Cargo error pointing at a manifest they never wrote.
    #[test]
    fn invalid_project_names_are_rejected_with_a_usable_suggestion() {
        for (name, expected_reason, expected_slug) in [
            ("", "the name is empty", "krab-app"),
            ("My App", "only ASCII letters", "my-app"),
            ("my.app", "only ASCII letters", "my-app"),
            ("my/app", "only ASCII letters", "my-app"),
            ("café", "only ASCII letters", "caf"),
            ("2fast", "starts with a digit", "app-2fast"),
            ("-lead", "starts with '-'", "lead"),
            ("async", "is a Rust keyword", "async-app"),
            ("struct", "is a Rust keyword", "struct-app"),
            ("con", "reserved Windows device name", "con-app"),
            ("COM1", "reserved Windows device name", "com1-app"),
            ("nul", "reserved Windows device name", "nul-app"),
            ("lpt9", "reserved Windows device name", "lpt9-app"),
            ("test", "Cargo reserves it", "test-app"),
            ("deps", "Cargo reserves it", "deps-app"),
        ] {
            let Err(err) = validate_project_name(name) else {
                panic!("{name:?} should be rejected");
            };
            let err = err.to_string();

            assert!(
                err.contains(expected_reason),
                "{name:?}: rejection does not explain why ({expected_reason:?}): {err}"
            );
            assert!(
                err.contains(&format!("Try: krab new {expected_slug}")),
                "{name:?}: rejection does not suggest {expected_slug:?}: {err}"
            );
        }

        let long = "a".repeat(65);
        let err = validate_project_name(&long)
            .expect_err("a 65-character name should be rejected")
            .to_string();
        assert!(err.contains("crates.io allows at most 64"), "{err}");
    }

    /// The suggestion is worthless if it would itself be rejected.
    #[test]
    fn suggested_slugs_are_themselves_valid() {
        for raw in [
            "",
            "   ",
            "My App",
            "My  Awful   Name!!",
            "2fast2furious",
            "async",
            "con",
            "COM1",
            "test",
            "----",
            "café",
            &"x".repeat(200),
        ] {
            let slug = suggested_project_slug(raw);
            assert!(
                validate_project_name(&slug).is_ok(),
                "suggestion {slug:?} for {raw:?} is itself invalid: {:?}",
                validate_project_name(&slug).unwrap_err().to_string()
            );
        }
    }

    /// A rejected name must not leave a half-built project behind.
    #[test]
    fn a_rejected_name_creates_nothing_on_disk() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_dir = temp_dir.path().join("My App");

        let err = write_project_from_template(
            &project_dir,
            "My App",
            &ProjectTemplate::Default,
            &DependencySource::Registry,
        )
        .expect_err("'My App' is not a valid package name");
        assert!(err.to_string().contains("invalid project name 'My App'"));

        assert!(
            !project_dir.exists(),
            "a rejected name left {} on disk",
            project_dir.display()
        );
        assert_eq!(
            fs::read_dir(temp_dir.path())?.count(),
            0,
            "a rejected name wrote something into the parent directory"
        );
        Ok(())
    }
}
