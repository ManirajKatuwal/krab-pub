use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::project_model::ProjectModel;
use crate::release_ops::run_command_logged;
use crate::BuildTarget;

const PUBLIC_ASSET_MANIFEST_NAME: &str = ".public_assets_manifest.json";

/// Build the frontend server, client WASM bundle, or both depending on the selected target.
///
/// The concrete build shape is resolved from `krab.toml` when a `[project]` section is
/// present. Otherwise the CLI falls back to the historical workspace layout used by the
/// framework repo itself.
pub(super) fn build_project(release: bool, target: &BuildTarget, diagnostics: bool) -> Result<()> {
    let project = ProjectModel::load()?;

    println!("🦀 Building Krab Project...");

    if matches!(target, BuildTarget::All | BuildTarget::Frontend) {
        build_frontend_target(&project, release, diagnostics)?;
    }

    if matches!(target, BuildTarget::All | BuildTarget::Client) {
        let explicit_client_target = matches!(target, BuildTarget::Client);
        if build_client_target(&project, release, diagnostics, explicit_client_target)? {
            fingerprint_assets(&project.dist_dir, project.client_artifact_stem())?;
        }
    }

    println!(
        "✅ Build Complete! Project model: frontend=`{}` output=`{}`",
        project.frontend_bin,
        project.dist_dir.display()
    );
    Ok(())
}

pub(super) fn dev_project(release: bool) -> Result<()> {
    let project = ProjectModel::load()?;

    println!("🧪 Running Dev Workflow...");
    build_project(release, &BuildTarget::All, false)?;

    println!("   > Starting {}...", project.frontend_bin);
    let status = Command::new("cargo")
        .arg("run")
        .arg("--bin")
        .arg(&project.frontend_bin)
        .args(release.then_some("--release"))
        .status()
        .with_context(|| format!("Failed to run {}", project.frontend_bin))?;

    if !status.success() {
        anyhow::bail!("{} exited with non-zero status", project.frontend_bin);
    }

    Ok(())
}

/// Run the incremental frontend watch workflow.
///
/// The loop fingerprints project-defined source/asset roots, classifies the change set, and
/// chooses the cheapest rebuild path available:
/// - client-only rebuilds for WASM/runtime changes
/// - frontend/full rebuilds for server-side changes
/// - direct asset mirroring for public-only changes
pub(super) fn watch_project(release: bool, poll_ms: u64, settle_ms: u64) -> Result<()> {
    let project = ProjectModel::load()?;

    println!("👀 Starting watch workflow (HMR-style restart)...");
    println!("   > Poll interval: {}ms", poll_ms);
    println!("   > Debounce settle: {}ms", settle_ms);
    println!("   > Frontend bin: {}", project.frontend_bin);

    let mut baseline = collect_file_fingerprints(&project)?;
    build_project(release, &BuildTarget::All, false)?;

    // The frontend is owned by a guard from here on: every `?` below runs while
    // a `cargo run` child is alive, and the guard is what keeps those exits from
    // orphaning it.
    let mut child = FrontendChildGuard::new(spawn_frontend(&project, release)?);
    let mut pending_change_since: Option<std::time::Instant> = None;

    // Change detection is a stat poll, not an OS watch: it re-stats every file
    // under the watch roots each interval, so cost grows with project size and a
    // save is noticed up to `poll_ms` late. An event-driven watcher would need a
    // filesystem-notification dependency (`notify`), and clean Ctrl-C shutdown a
    // signal-handling one (`ctrlc`); adding either is a dependency decision under
    // this repo's governance rules, so both are deliberately deferred.
    loop {
        std::thread::sleep(Duration::from_millis(poll_ms));

        if let Some(status) = child
            .try_wait()
            .context("Failed checking frontend process state")?
        {
            eprintln!(
                "⚠️ {} exited ({status}). Restarting...",
                project.frontend_bin
            );
            child.replace(spawn_frontend(&project, release)?);
        }

        let next = collect_file_fingerprints(&project)?;
        if next == baseline {
            pending_change_since = None;
            continue;
        }

        if pending_change_since.is_none() {
            pending_change_since = Some(std::time::Instant::now());
            continue;
        }

        if pending_change_since
            .map(|t| t.elapsed() < Duration::from_millis(settle_ms))
            .unwrap_or(false)
        {
            continue;
        }

        let mut client_changed = false;
        let mut server_changed = false;
        let mut public_changed = false;

        for (path, hash) in &next {
            if baseline.get(path) != Some(hash) {
                classify_path_change(
                    path,
                    &project,
                    &mut client_changed,
                    &mut server_changed,
                    &mut public_changed,
                );
            }
        }
        for path in baseline.keys() {
            if !next.contains_key(path) {
                classify_path_change(
                    path,
                    &project,
                    &mut client_changed,
                    &mut server_changed,
                    &mut public_changed,
                );
            }
        }

        println!("   > Change detected (settled). Rebuilding...");
        if client_changed && !server_changed {
            println!("   > ⚡ Partial invalidation: Client only");
            if let Err(err) = build_project(release, &BuildTarget::Client, false) {
                eprintln!("⚠️ Client rebuild failed: {err}");
                baseline = next;
                pending_change_since = None;
                continue;
            }
        } else if server_changed {
            println!("   > ⚡ Partial invalidation: Server/Full");
            child.terminate();

            let target = if client_changed {
                BuildTarget::All
            } else {
                BuildTarget::Frontend
            };
            if let Err(err) = build_project(release, &target, false) {
                eprintln!("⚠️ Rebuild failed: {err}");
                baseline = next;
                pending_change_since = None;
                continue;
            }
            child.replace(spawn_frontend(&project, release)?);
        } else if public_changed {
            println!("   > ⚡ Partial invalidation: Public assets only (No rebuild)");
            hot_patch_assets(&project)?;
        }

        write_hmr_signal_file(&project)?;
        baseline = next;
        pending_change_since = None;
    }
}

pub(super) fn bootstrap_local_stack(release: bool, skip_build: bool) -> Result<()> {
    let project = ProjectModel::load()?;

    println!("🚀 Bootstrapping local Krab stack...");
    if !skip_build {
        build_project(release, &BuildTarget::All, true)?;
    }

    let status = Command::new("cargo")
        .arg("run")
        .arg("--bin")
        .arg(&project.bootstrap_bin)
        .args(release.then_some("--release"))
        .status()
        .with_context(|| format!("Failed to run {}", project.bootstrap_bin))?;

    if !status.success() {
        anyhow::bail!("{} exited with non-zero status", project.bootstrap_bin);
    }
    Ok(())
}

/// The `--json` shape of `krab env-check`.
///
/// `status` mirrors the exit status this run will produce, so a consumer does
/// not have to re-derive the `--strict` rule: `passed` with no warnings,
/// `warnings` when warnings were found but the run still exits zero, `failed`
/// when `--strict` turns them into a non-zero exit.
#[derive(Debug, Serialize)]
struct EnvCheckReport<'a> {
    command: &'static str,
    status: &'static str,
    warnings: &'a [String],
}

/// Validate the most common local/staging/prod environment combinations used by Krab services.
///
/// This is intentionally a lightweight policy check for developer workflows; stricter runtime
/// validation still lives in framework configuration loading.
///
/// The rules themselves live in [`crate::env_policy`], shared with
/// `krab doctor`. Only the presentation and the `--strict` exit rule are here.
pub(super) fn validate_environment(strict: bool, json: bool) -> Result<()> {
    let warnings = crate::env_policy::collect_environment_warnings();
    let failed = strict && !warnings.is_empty();

    if json {
        let status = match (warnings.is_empty(), failed) {
            (true, _) => "passed",
            (false, true) => "failed",
            (false, false) => "warnings",
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&EnvCheckReport {
                command: "env-check",
                status,
                warnings: &warnings,
            })?
        );
    } else if warnings.is_empty() {
        println!("✅ Environment validation passed");
    } else {
        for warning in &warnings {
            eprintln!("⚠️ {warning}");
        }
    }

    // `--json` changes only what is printed, never the exit status.
    if failed {
        anyhow::bail!("Environment validation failed in strict mode");
    }

    Ok(())
}

/// Regenerate the developer workflow reference document that describes the CLI build/watch flow.
pub(super) fn generate_docs(out: &PathBuf) -> Result<()> {
    let project = ProjectModel::load()?;

    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create documentation directory {parent:?}"))?;
        }
    }

    let mut command_matrix = BTreeMap::<String, String>::new();
    command_matrix.insert(
        "krab build [--release]".to_string(),
        if project.has_client_build() {
            format!(
                "Build `{}` plus client/WASM artifacts into `{}`",
                project.frontend_bin,
                project.dist_dir.display()
            )
        } else {
            format!(
                "Build `{}` (no separate client/WASM target configured)",
                project.frontend_bin
            )
        },
    );
    command_matrix.insert(
        "krab dev [--release]".to_string(),
        format!("Build once and run `{}`", project.frontend_bin),
    );
    command_matrix.insert(
        "krab dev --watch [--release] [--poll-ms <n>] [--settle-ms <n>]".to_string(),
        format!(
            "Watch {:?}, rebuild changed targets, and restart `{}`",
            project.watch_roots(),
            project.frontend_bin
        ),
    );
    command_matrix.insert(
        "krab watch [--release] [--poll-ms <n>] [--settle-ms <n>]".to_string(),
        "Alias dedicated to watch workflow".to_string(),
    );
    command_matrix.insert(
        "krab bootstrap [--release]".to_string(),
        format!("Build and run `{}`", project.bootstrap_bin),
    );
    command_matrix.insert(
        "krab docs [--out <path>]".to_string(),
        "Regenerate this developer workflow document".to_string(),
    );
    command_matrix.insert(
        "krab doctor [--diagnostics] [--strict] [--json]".to_string(),
        "Run aggregated workspace health checks for project model, env policy, service config, and topology. Checks that do not apply to this project are reported SKIP, not OK".to_string(),
    );
    command_matrix.insert(
        // Comma-separated, not `a|b|c`: this string lands in a Markdown table
        // cell, and GFM splits cells on `|` even inside backticks.
        "krab completions <shell>  (bash, zsh, fish, powershell, elvish)".to_string(),
        "Write a shell completion script for the `krab` binary to stdout".to_string(),
    );
    command_matrix.insert(
        "krab security dependency-gate [--diagnostics]".to_string(),
        "Run local dependency governance gate with cargo-deny (CI parity)".to_string(),
    );
    command_matrix.insert(
        "krab release certify [--out <dir>] [--diagnostics] [--json]".to_string(),
        "Run release gates and write a structured evidence bundle".to_string(),
    );
    command_matrix.insert(
        "krab topology doctor [--diagnostics] [--json]".to_string(),
        "Run topology boundary checks (cross-service imports, contract payload derives, endpoint config)".to_string(),
    );
    command_matrix.insert(
        "krab topology split <domain> [--protocols rest,graphql,rpc,grpc] [--register] [--dry-run]".to_string(),
        "Generate split-service extraction skeleton for a domain with adapter stubs and optional registration".to_string(),
    );

    let mut rows = String::new();
    for (cmd, desc) in command_matrix {
        rows.push_str(&format!("| `{}` | {} |\n", cmd, desc));
    }

    // The command the CLI actually runs, feature flags included. Documenting it
    // without them is how three separate build paths spent two releases
    // shipping an inert bundle.
    let client_section = match (
        project.client_package.as_deref(),
        project.client_crate_dir.as_deref(),
    ) {
        (Some(package), Some(dir)) => format!(
            "## Client/WASM Build\n\n- Client package: `{package}`\n- Crate directory: `{}`\n\n`krab build --client --release` runs:\n\n```sh\nwasm-pack build --release --target web --out-dir {}{}\n```\n\n`#[island]` compiles its hydrating half only under `feature = \"web\"`. A bundle built without it still loads and still exports `hydrate` — it just does nothing, at roughly a tenth of the size, with no error anywhere. The CLI passes the feature whenever the client crate's manifest declares it.\n\n",
            dir.display(),
            project.dist_dir.display(),
            if client_web_feature(dir) {
                " -- --features web"
            } else {
                ""
            }
        ),
        _ => "## Client/WASM Build\n\nNo client/WASM package is configured in `[project]` of `krab.toml`, so `krab build` skips the client step.\n\n".to_string(),
    };

    let content = format!(
        "# Dev Workflow and Build Outputs\n\n## Project Model\n\n- Frontend bin: `{}`\n- Bootstrap bin: `{}`\n- Public dir: `{}`\n- Dist dir: `{}`\n- Watch roots: {:?}\n\n## CLI Commands\n\n`--diagnostics` and `--json` are global: they may be given before or after the subcommand, and are listed below only on the commands that act on them. `--json` changes what is printed, never the exit status.\n\n| Command | Description |\n|---|---|\n{}\n{}## Asset Fingerprinting\n\nWhen a client/WASM package is configured, the CLI fingerprints browser assets and writes `{}/assets.json`.\n\n## Watch/HMR Workflow\n\n`krab dev --watch` (or `krab watch`) performs incremental change detection over the configured watch roots, rebuilds only the necessary targets, mirrors changed public assets, and writes a lightweight HMR signal file at `{}`.\n\n## Bootstrap Health Semantics\n\n`krab bootstrap` starts services in dependency order, waits on each startup readiness probe before proceeding, and applies restart policy backoff/attempt limits from `krab.toml`. Use `/ready` for readiness probes and `/health` for liveness checks. Service stdout/stderr are captured with stable `[service::stream]` prefixes and written to `internal/audit/orchestrator/` for artifact collection.\n",
        project.frontend_bin,
        project.bootstrap_bin,
        project.public_dir.display(),
        project.dist_dir.display(),
        project.watch_roots(),
        rows,
        client_section,
        project.dist_dir.display(),
        project.hmr_signal_path.display()
    );

    fs::write(out, content).with_context(|| format!("Failed to write docs file {out:?}"))?;
    println!("✅ Wrote workflow docs to {}", out.display());
    Ok(())
}

fn build_frontend_target(project: &ProjectModel, release: bool, diagnostics: bool) -> Result<()> {
    println!("   > Building frontend bin `{}`...", project.frontend_bin);

    let mut server_cmd = Command::new("cargo");
    server_cmd
        .arg("build")
        .arg("--bin")
        .arg(&project.frontend_bin);
    if release {
        server_cmd.arg("--release");
    }

    run_command_logged(
        &format!("cargo build --bin {}", project.frontend_bin),
        &mut server_cmd,
        diagnostics,
    )
}

fn build_client_target(
    project: &ProjectModel,
    release: bool,
    diagnostics: bool,
    explicit_client_target: bool,
) -> Result<bool> {
    let Some(client_package) = project.client_package.as_deref() else {
        if explicit_client_target {
            anyhow::bail!(
                "No client/WASM package is configured in [project] of krab.toml for this project"
            );
        }
        println!("   > No separate client/WASM package configured; skipping client build");
        return Ok(false);
    };

    let Some(client_crate_dir) = project.client_crate_dir.as_deref() else {
        if explicit_client_target {
            anyhow::bail!(
                "Client package `{}` is configured without client_crate_dir in [project] of krab.toml",
                client_package
            );
        }
        println!(
            "   > Client package `{}` has no client_crate_dir; skipping client build",
            client_package
        );
        return Ok(false);
    };

    println!("   > Building client/WASM package `{}`...", client_package);
    fs::create_dir_all(&project.dist_dir).with_context(|| {
        format!(
            "Failed to create dist directory {}",
            project.dist_dir.display()
        )
    })?;

    // `#[island]` selects its hydrating half on `feature = "web"`. A client
    // build that omits it produces a bundle whose `hydrate()` logs one line and
    // returns — indistinguishable from a working one except by size, which is
    // how it went unnoticed through the whole 0.1–0.2 line.
    let web_feature = client_web_feature(client_crate_dir);

    if release {
        println!("   > Running optimized production wasm-pack pipeline...");
        let mut wasm_pack_cmd = Command::new("wasm-pack");
        wasm_pack_cmd
            .current_dir(client_crate_dir)
            .arg("build")
            .arg("--release")
            .arg("--target")
            .arg("web")
            .arg("--out-dir")
            .arg(absolute_path(&project.dist_dir)?);
        if web_feature {
            // Everything after `--` goes to cargo, not to wasm-pack.
            wasm_pack_cmd.arg("--").arg("--features").arg("web");
        }
        run_command_logged(
            &format!(
                "wasm-pack build --release --target web{} ({})",
                if web_feature {
                    " -- --features web"
                } else {
                    ""
                },
                client_package
            ),
            &mut wasm_pack_cmd,
            diagnostics,
        )?;

        let wasm_stem = project.client_artifact_stem().ok_or_else(|| {
            anyhow::anyhow!("Missing client_artifact_stem for {}", client_package)
        })?;
        let wasm_path = project.dist_dir.join(format!("{wasm_stem}_bg.wasm"));
        let require_wasm_opt = std::env::var("KRAB_REQUIRE_WASM_OPT")
            .ok()
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(true);

        if wasm_path.exists() {
            let mut wasm_opt_cmd = Command::new("wasm-opt");
            wasm_opt_cmd
                .arg("-Oz")
                .arg("--strip-debug")
                .arg("--strip-dwarf")
                .arg("-o")
                .arg(&wasm_path)
                .arg(&wasm_path);

            match wasm_opt_cmd.status() {
                Ok(status) if status.success() => {
                    if diagnostics {
                        println!("   > Finished: wasm-opt -Oz --strip-debug --strip-dwarf");
                    }
                }
                Ok(_) => {
                    anyhow::bail!(
                        "wasm-opt optimization pass failed for {:?}. Install binaryen or set KRAB_REQUIRE_WASM_OPT=0",
                        wasm_path
                    );
                }
                Err(err) => {
                    if require_wasm_opt {
                        anyhow::bail!(
                            "wasm-opt not found or failed to start ({err}). Install binaryen or set KRAB_REQUIRE_WASM_OPT=0"
                        );
                    }
                    println!("⚠️ wasm-opt unavailable; skipping extra optimization pass");
                }
            }
        }
    } else {
        let mut client_cmd = Command::new("cargo");
        client_cmd
            .arg("build")
            .arg("-p")
            .arg(client_package)
            .arg("--target")
            .arg("wasm32-unknown-unknown");
        if web_feature {
            client_cmd.arg("--features").arg("web");
        }

        run_command_logged(
            &format!(
                "cargo build -p {} --target wasm32-unknown-unknown{}",
                client_package,
                if web_feature { " --features web" } else { "" }
            ),
            &mut client_cmd,
            diagnostics,
        )?;

        println!("   > Generating JS bindings (debug-friendly dev mode)...");
        let wasm_path = PathBuf::from("target")
            .join("wasm32-unknown-unknown")
            .join("debug")
            .join(format!("{client_package}.wasm"));

        if !wasm_path.exists() {
            anyhow::bail!("WASM file not found at: {:?}", wasm_path);
        }

        let mut bindgen_cmd = Command::new("wasm-bindgen");
        bindgen_cmd
            .arg(&wasm_path)
            .arg("--out-dir")
            .arg(&project.dist_dir)
            .arg("--target")
            .arg("web")
            .arg("--debug")
            .arg("--no-typescript");

        match bindgen_cmd.status() {
            Ok(status) => {
                if !status.success() {
                    anyhow::bail!("wasm-bindgen failed. Make sure it is installed.");
                }
            }
            Err(_) => {
                println!("⚠️ wasm-bindgen not found. Skipping JS generation.");
            }
        }
    }

    Ok(true)
}

/// Whether the client crate at `crate_dir` declares a `web` Cargo feature.
///
/// The browser half of `#[island]` — the hydrating implementation and its
/// `inventory` registration — is gated on `feature = "web"`, so a client bundle
/// built without it is inert. `krab_client` now enables `web` by default, but
/// a crate that also puts wasm32-only dependencies behind the feature keeps it
/// opt-in (`examples/reference_apps/islands_rpc` does exactly that, because
/// enabling it for a native build would not compile).
///
/// The flag is passed only when the manifest declares the feature. Passing it
/// unconditionally would turn `krab build --client` into a hard failure
/// ("does not have the feature `web`") for every client crate that gates its
/// browser half on something else, or on nothing at all.
///
/// An unreadable or unparseable manifest yields `false`: the build then runs
/// exactly as it did before, and cargo reports the real problem.
fn client_web_feature(crate_dir: &Path) -> bool {
    #[derive(Deserialize)]
    struct Manifest {
        #[serde(default)]
        features: BTreeMap<String, Vec<String>>,
    }

    let Ok(contents) = fs::read_to_string(crate_dir.join("Cargo.toml")) else {
        return false;
    };

    toml::from_str::<Manifest>(&contents)
        .map(|manifest| manifest.features.contains_key("web"))
        .unwrap_or(false)
}

/// Owns the frontend process for the lifetime of the watch loop and kills it on drop.
///
/// The watch loop uses `?` at several points while the frontend is already
/// running (fingerprint collection, the HMR signal write, `try_wait`, the
/// in-loop respawn). Before this guard, every one of those early returns left an
/// orphaned `cargo run` holding the listen port, so the next `krab dev` failed to
/// bind with an error that pointed nowhere near the cause. `Drop` covers the
/// panic path too, which no amount of explicit cleanup at the return sites would.
struct FrontendChildGuard {
    child: Child,
}

impl FrontendChildGuard {
    fn new(child: Child) -> Self {
        Self { child }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// Stop the process currently owned and take ownership of its replacement.
    fn replace(&mut self, child: Child) {
        self.terminate();
        self.child = child;
    }

    /// Best-effort kill plus reap, so the port is released and no zombie is left.
    ///
    /// Both calls are allowed to fail: the child may already have exited, which
    /// is the normal case on the restart-after-crash path.
    fn terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for FrontendChildGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Spawn the frontend service process used by `krab dev` and the watch loop.
fn spawn_frontend(project: &ProjectModel, release: bool) -> Result<Child> {
    let mut cmd = Command::new("cargo");
    cmd.arg("run").arg("--bin").arg(&project.frontend_bin);
    if release {
        cmd.arg("--release");
    }
    cmd.spawn()
        .with_context(|| format!("Failed to start {} process", project.frontend_bin))
}

/// Mirror public assets into `dist/` so asset-only changes avoid a Rust rebuild.
///
/// A small manifest is maintained to remove stale mirrored files without touching non-public
/// build artifacts that happen to live in the same output directory.
fn hot_patch_assets(project: &ProjectModel) -> Result<()> {
    if !project.public_dir.exists() {
        return Ok(());
    }

    fs::create_dir_all(&project.dist_dir)?;

    let manifest_path = project.dist_dir.join(PUBLIC_ASSET_MANIFEST_NAME);
    let previous_manifest = read_public_asset_manifest(&manifest_path)?;
    let mut current_files = Vec::new();
    let mut copied = 0usize;

    sync_public_assets_recursive(
        &project.public_dir,
        &project.public_dir,
        &project.dist_dir,
        &mut current_files,
        &mut copied,
    )?;

    let current_manifest = PublicAssetManifest {
        files: current_files,
    };
    prune_stale_public_assets(&project.dist_dir, &previous_manifest, &current_manifest)?;
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&current_manifest)?,
    )
    .with_context(|| format!("Failed to write {}", manifest_path.display()))?;

    if copied > 0 {
        println!(
            "   > 🔥 Hot-patched {} asset file(s) into {}/",
            copied,
            project.dist_dir.display()
        );
    }

    Ok(())
}

fn sync_public_assets_recursive(
    root: &Path,
    current: &Path,
    dist_root: &Path,
    manifest_files: &mut Vec<String>,
    count: &mut usize,
) -> Result<()> {
    if !current.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let src_path = entry.path();
        if src_path.is_dir() {
            sync_public_assets_recursive(root, &src_path, dist_root, manifest_files, count)?;
            continue;
        }

        if !is_safe_public_asset(&src_path) {
            continue;
        }

        let rel_path = src_path
            .strip_prefix(root)
            .with_context(|| format!("Failed to compute relative path for {:?}", src_path))?;
        let dst_path = dist_root.join(rel_path);
        if let Some(parent) = dst_path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::copy(&src_path, &dst_path)
            .with_context(|| format!("Failed to hot-patch {:?} -> {:?}", src_path, dst_path))?;

        manifest_files.push(normalize_relative_path(rel_path));
        *count += 1;
    }

    Ok(())
}

fn read_public_asset_manifest(path: &Path) -> Result<PublicAssetManifest> {
    if !path.exists() {
        return Ok(PublicAssetManifest::default());
    }

    let bytes = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let manifest: PublicAssetManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(manifest)
}

fn prune_stale_public_assets(
    dist_root: &Path,
    previous: &PublicAssetManifest,
    current: &PublicAssetManifest,
) -> Result<()> {
    let current_files = current.files.iter().cloned().collect::<HashSet<_>>();

    for rel_path in &previous.files {
        if current_files.contains(rel_path) {
            continue;
        }

        let candidate = dist_root.join(rel_path);
        if candidate.exists() {
            fs::remove_file(&candidate).with_context(|| {
                format!(
                    "Failed to remove stale public asset {}",
                    candidate.display()
                )
            })?;
            remove_empty_parent_dirs(candidate.parent(), dist_root)?;
        }
    }

    Ok(())
}

fn remove_empty_parent_dirs(mut current: Option<&Path>, stop_at: &Path) -> Result<()> {
    while let Some(dir) = current {
        if dir == stop_at || !dir.starts_with(stop_at) {
            return Ok(());
        }
        if fs::read_dir(dir)?.next().is_some() {
            return Ok(());
        }
        fs::remove_dir(dir)
            .with_context(|| format!("Failed to remove empty directory {}", dir.display()))?;
        current = dir.parent();
    }
    Ok(())
}

/// Produce a lightweight fingerprint map for files that influence frontend rebuild decisions.
///
/// `DefaultHasher` is correct *here* and deliberately not SHA-256: these values
/// are ephemeral, never leave the process, and are only ever compared against
/// other values produced by the same binary in the same run, so the instability
/// that rules `DefaultHasher` out for [`asset_content_digest`] cannot bite. Do
/// not unify the two — the constraints are opposite.
fn collect_file_fingerprints(project: &ProjectModel) -> Result<HashMap<PathBuf, u64>> {
    let mut files = Vec::new();
    for root in project.watch_roots() {
        collect_files_recursive(&root, &mut files)?;
    }

    let mut map = HashMap::new();
    for file in files {
        let mut hasher = DefaultHasher::new();
        if let Ok(meta) = fs::metadata(&file) {
            if let Ok(modified) = meta.modified() {
                let millis = modified
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                millis.hash(&mut hasher);
            }
            meta.len().hash(&mut hasher);
        }
        map.insert(file, hasher.finish());
    }
    Ok(map)
}

/// Walk a directory tree and collect source/asset files relevant to incremental rebuilds.
fn collect_files_recursive(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(path).with_context(|| format!("Failed to read directory {path:?}"))? {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            collect_files_recursive(&p, out)?;
        } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            if [
                "rs", "html", "css", "js", "json", "wasm", "svg", "png", "jpg", "jpeg", "gif",
                "webp", "ico", "woff", "woff2", "ttf", "eot",
            ]
            .contains(&ext)
            {
                out.push(p);
            }
        }
    }

    Ok(())
}

/// Fingerprint the generated browser assets and emit a small manifest used by the frontend.
fn fingerprint_assets(out_dir: &Path, artifact_stem: Option<&str>) -> Result<()> {
    let Some(artifact_stem) = artifact_stem else {
        return Ok(());
    };

    let mut manifest_entries = Vec::new();

    for file_name in [
        format!("{artifact_stem}.js"),
        format!("{artifact_stem}_bg.wasm"),
    ] {
        let input = out_dir.join(&file_name);
        if !input.exists() {
            continue;
        }

        let bytes = fs::read(&input).with_context(|| format!("Failed to read {:?}", input))?;
        let digest = asset_content_digest(&bytes);

        let ext = input
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or_default();
        let stem = input
            .file_stem()
            .and_then(|v| v.to_str())
            .unwrap_or("asset");

        let output_name = format!("{}.{}.{}", stem, &digest[..8], ext);
        let output = out_dir.join(&output_name);
        fs::copy(&input, &output)
            .with_context(|| format!("Failed to copy {:?} -> {:?}", input, output))?;

        manifest_entries.push(format!(
            "\"{}\":{{\"source\":\"{}\",\"fingerprinted\":\"{}\"}}",
            file_name, file_name, output_name
        ));
    }

    let manifest = format!("{{{}}}\n", manifest_entries.join(","));
    fs::write(out_dir.join("assets.json"), manifest).context("Failed to write assets.json")?;

    Ok(())
}

/// Content digest baked into a fingerprinted asset filename.
///
/// SHA-256, not `DefaultHasher`: this digest is persisted — it becomes a file
/// name on disk, an entry in `assets.json`, and a cache key in every browser and
/// CDN that has seen the asset. `DefaultHasher`'s output is documented as
/// unstable across Rust releases, so building on a newer toolchain would rename
/// every asset and invalidate every one of those caches for bytes that never
/// changed. Same reasoning as krab_core's migration checksums.
fn asset_content_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Classify a changed path into client/server/public buckets for partial invalidation.
fn classify_path_change(
    path: &Path,
    project: &ProjectModel,
    client_changed: &mut bool,
    server_changed: &mut bool,
    public_changed: &mut bool,
) {
    if path_matches_roots(path, &project.public_paths) {
        *public_changed = true;
    } else if path_matches_roots(path, &project.shared_paths) {
        *client_changed = true;
        *server_changed = true;
    } else if path_matches_roots(path, &project.client_paths) {
        *client_changed = true;
    } else if path_matches_roots(path, &project.server_paths) {
        *server_changed = true;
    }
}

fn path_matches_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Touch the lightweight HMR signal file so the browser-side polling logic can refresh.
fn write_hmr_signal_file(project: &ProjectModel) -> Result<()> {
    if let Some(parent) = project.hmr_signal_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    fs::write(
        &project.hmr_signal_path,
        format!(
            "{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ),
    )
    .with_context(|| format!("Failed to write {}", project.hmr_signal_path.display()))?;

    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("Failed to resolve current working directory")?;
    Ok(cwd.join(path))
}

fn normalize_relative_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn is_safe_public_asset(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| {
            [
                "css", "html", "js", "json", "svg", "png", "jpg", "jpeg", "gif", "webp", "ico",
                "woff", "woff2", "ttf", "eot",
            ]
            .contains(&ext)
        })
        .unwrap_or(false)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PublicAssetManifest {
    files: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        asset_content_digest, classify_path_change, client_web_feature, fingerprint_assets,
        normalize_relative_path, FrontendChildGuard, ProjectModel,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The bug this guards: a client bundle built without `web` still links and
    /// still loads, it just does nothing. Detecting the feature from the
    /// manifest is what stops `krab build --client` from shipping that.
    #[test]
    fn the_web_feature_is_detected_from_the_client_manifest() {
        let dir = tempfile::tempdir().expect("temp dir");

        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo_client\"\n\n[features]\nweb = []\n",
        )
        .expect("write manifest");
        assert!(client_web_feature(dir.path()));

        // A client crate that gates its browser half on something else must not
        // be handed a feature it does not declare — cargo would refuse to build.
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo_client\"\n\n[features]\nbrowser = []\n",
        )
        .expect("write manifest");
        assert!(!client_web_feature(dir.path()));
    }

    #[test]
    fn a_missing_or_unparseable_manifest_adds_no_features() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(!client_web_feature(dir.path()));

        std::fs::write(dir.path().join("Cargo.toml"), "this is not toml {{").expect("write");
        assert!(!client_web_feature(dir.path()));
    }

    fn demo_project() -> ProjectModel {
        ProjectModel {
            frontend_bin: "demo".to_string(),
            client_package: Some("demo_client".to_string()),
            client_crate_dir: Some(PathBuf::from("demo_client")),
            client_artifact_stem: Some("demo_client".to_string()),
            public_dir: PathBuf::from("public"),
            dist_dir: PathBuf::from("dist"),
            server_paths: vec![PathBuf::from("src")],
            client_paths: vec![PathBuf::from("client/src")],
            shared_paths: vec![PathBuf::from("shared")],
            public_paths: vec![PathBuf::from("public")],
            bootstrap_bin: "demo".to_string(),
            hmr_signal_path: PathBuf::from("dist/.hmr_signal"),
        }
    }

    #[test]
    fn classify_respects_project_roots() {
        let project = demo_project();
        let mut client_changed = false;
        let mut server_changed = false;
        let mut public_changed = false;

        classify_path_change(
            Path::new("shared/config.rs"),
            &project,
            &mut client_changed,
            &mut server_changed,
            &mut public_changed,
        );

        assert!(client_changed);
        assert!(server_changed);
        assert!(!public_changed);
    }

    #[test]
    fn normalize_relative_path_uses_forward_slashes() {
        let normalized = normalize_relative_path(Path::new("public/images/logo.svg"));
        assert_eq!(normalized, "public/images/logo.svg");
    }

    /// Pins the digest to hard-coded SHA-256 output.
    ///
    /// The whole point of moving off `DefaultHasher` is that the digest survives
    /// a toolchain bump, and a test that only checked "some hex characters"
    /// would have passed under the old hasher too. If this ever fails, the
    /// fingerprint algorithm changed and every published asset URL changed with
    /// it — that is a deliberate decision to make, not a test to relax.
    #[test]
    fn asset_digest_is_stable_sha256() {
        assert_eq!(
            asset_content_digest(b"krab-asset-fingerprint"),
            "94ccafed55efc4f6c80e5aaaa93219d640900c0da474e4eb039372d77d62acf5"
        );
        assert_eq!(
            asset_content_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// The frontend parses `assets.json` at boot, so its shape is a contract.
    ///
    /// Guards the SHA-256 swap: the digest is wider than `DefaultHasher`'s was,
    /// and truncating it to a different width or reordering the filename parts
    /// would break asset resolution at runtime rather than at build time.
    #[test]
    fn fingerprint_assets_preserves_the_manifest_contract() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("demo_client.js"), b"console.log(1);").expect("write js");
        fs::write(dir.path().join("demo_client_bg.wasm"), b"\0asm\x01").expect("write wasm");

        fingerprint_assets(dir.path(), Some("demo_client")).expect("fingerprint");

        let raw = fs::read_to_string(dir.path().join("assets.json")).expect("read manifest");
        let parsed: BTreeMap<String, BTreeMap<String, String>> =
            serde_json::from_str(&raw).expect("manifest is valid JSON");
        assert_eq!(parsed.len(), 2);

        let entry = &parsed["demo_client.js"];
        assert_eq!(entry["source"], "demo_client.js");

        // `<stem>.<8 hex>.<ext>`, with the copied file actually on disk.
        let fingerprinted = &entry["fingerprinted"];
        let digest = fingerprinted
            .strip_prefix("demo_client.")
            .and_then(|rest| rest.strip_suffix(".js"))
            .expect("fingerprinted name keeps stem and extension around the digest");
        assert_eq!(digest.len(), 8);
        assert_eq!(digest, &asset_content_digest(b"console.log(1);")[..8]);
        assert!(dir.path().join(fingerprinted).exists());

        assert_eq!(
            parsed["demo_client_bg.wasm"]["source"],
            "demo_client_bg.wasm"
        );
    }

    /// A cheap long-lived process, portable across the platforms CI runs on.
    fn spawn_sleeper() -> std::process::Child {
        if cfg!(windows) {
            // `ping` is the shortest always-present Windows sleep; the pings run
            // a second apart, so 30 outlives any plausible test run.
            Command::new("ping")
                .args(["-n", "30", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("failed to spawn ping")
        } else {
            Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("failed to spawn sleep")
        }
    }

    /// Encodes the leak fixed here: `watch_project` returned through `?` while
    /// the frontend was still running, orphaning a `cargo run` that kept the
    /// listen port and made the next `krab dev` fail to bind.
    ///
    /// This drives `terminate` directly rather than observing a dropped guard:
    /// `Drop` is a one-line delegation to it, and probing for a dead PID after
    /// the handle is reaped is both platform-specific and racy against PID
    /// reuse. The assertions below are the real signal — the child was alive,
    /// and after the guard's cleanup it is not.
    #[test]
    fn frontend_guard_kills_a_live_child() {
        let mut guard = FrontendChildGuard::new(spawn_sleeper());
        assert!(
            guard.try_wait().expect("try_wait").is_none(),
            "sleeper should still be running before cleanup"
        );

        guard.terminate();

        assert!(
            guard.try_wait().expect("try_wait").is_some(),
            "guard cleanup must leave the child terminated and reaped"
        );
    }

    /// Restarting after a rebuild must hand ownership over, not stack processes.
    ///
    /// The predecessor is unobservable once `replace` has taken it — asserting
    /// on a reaped PID would race with PID reuse — so what is checked here is
    /// that the guard now owns the replacement, which is only reachable through
    /// the `terminate` that `replace` runs first.
    #[test]
    fn frontend_guard_replace_hands_ownership_to_the_new_child() {
        let first = spawn_sleeper();
        let first_pid = first.id();
        let mut guard = FrontendChildGuard::new(first);

        let second = spawn_sleeper();
        let second_pid = second.id();
        assert_ne!(first_pid, second_pid);

        guard.replace(second);

        assert_eq!(guard.child.id(), second_pid);
        assert!(
            guard.try_wait().expect("try_wait").is_none(),
            "the replacement must still be running"
        );
        guard.terminate();
    }
}
