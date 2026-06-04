use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

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

    let mut child = spawn_frontend(&project, release)?;
    let mut pending_change_since: Option<std::time::Instant> = None;

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
            child = spawn_frontend(&project, release)?;
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
            let _ = child.kill();
            let _ = child.wait();

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
            child = spawn_frontend(&project, release)?;
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

/// Validate the most common local/staging/prod environment combinations used by Krab services.
///
/// This is intentionally a lightweight policy check for developer workflows; stricter runtime
/// validation still lives in framework configuration loading.
pub(super) fn validate_environment(strict: bool) -> Result<()> {
    let mut warnings = Vec::new();

    let auth_mode = std::env::var("KRAB_AUTH_MODE").unwrap_or_else(|_| "jwt".to_string());
    if auth_mode.eq_ignore_ascii_case("jwt") || auth_mode.eq_ignore_ascii_case("oidc") {
        if std::env::var("KRAB_OIDC_ISSUER").is_err() {
            warnings.push("KRAB_OIDC_ISSUER is required when KRAB_AUTH_MODE=jwt".to_string());
        }
        if std::env::var("KRAB_OIDC_AUDIENCE").is_err() {
            warnings.push("KRAB_OIDC_AUDIENCE is required when KRAB_AUTH_MODE=jwt".to_string());
        }
    } else if auth_mode.eq_ignore_ascii_case("static") {
        let env_name = std::env::var("KRAB_ENVIRONMENT").unwrap_or_else(|_| "dev".to_string());
        if !env_name.eq_ignore_ascii_case("local") && !env_name.eq_ignore_ascii_case("dev") {
            warnings.push(
                "KRAB_AUTH_MODE=static is forbidden outside local/dev; use jwt or oidc".to_string(),
            );
        }
    } else {
        warnings.push(format!(
            "Unsupported KRAB_AUTH_MODE='{}'; expected static|jwt|oidc",
            auth_mode
        ));
    }

    let env_name = std::env::var("KRAB_ENVIRONMENT").unwrap_or_else(|_| "dev".to_string());
    if !["local", "dev", "staging", "prod"].contains(&env_name.as_str()) {
        warnings.push(format!(
            "KRAB_ENVIRONMENT should be one of local|dev|staging|prod, found: {}",
            env_name
        ));
    }

    if warnings.is_empty() {
        println!("✅ Environment validation passed");
        return Ok(());
    }

    for warning in &warnings {
        eprintln!("⚠️ {warning}");
    }

    if strict {
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
        "krab doctor [--diagnostics] [--strict]".to_string(),
        "Run aggregated workspace health checks for project model, env policy, service config, and topology".to_string(),
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
        "krab topology doctor [--diagnostics]".to_string(),
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

    let content = format!(
        "# Dev Workflow and Build Outputs\n\n## Project Model\n\n- Frontend bin: `{}`\n- Bootstrap bin: `{}`\n- Public dir: `{}`\n- Dist dir: `{}`\n- Watch roots: {:?}\n\n## CLI Commands\n\n| Command | Description |\n|---|---|\n{}\n## Asset Fingerprinting\n\nWhen a client/WASM package is configured, the CLI fingerprints browser assets and writes `{}/assets.json`.\n\n## Watch/HMR Workflow\n\n`krab dev --watch` (or `krab watch`) performs incremental change detection over the configured watch roots, rebuilds only the necessary targets, mirrors changed public assets, and writes a lightweight HMR signal file at `{}`.\n\n## Bootstrap Health Semantics\n\n`krab bootstrap` starts services in dependency order, waits on each startup readiness probe before proceeding, and applies restart policy backoff/attempt limits from `krab.toml`. Use `/ready` for readiness probes and `/health` for liveness checks. Service stdout/stderr are captured with stable `[service::stream]` prefixes and written to `audit/orchestrator/` for artifact collection.\n",
        project.frontend_bin,
        project.bootstrap_bin,
        project.public_dir.display(),
        project.dist_dir.display(),
        project.watch_roots(),
        rows,
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
        run_command_logged(
            &format!(
                "wasm-pack build --release --target web ({})",
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

        run_command_logged(
            &format!(
                "cargo build -p {} --target wasm32-unknown-unknown",
                client_package
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

/// Spawn the frontend service process used by `krab dev` and the watch loop.
fn spawn_frontend(project: &ProjectModel, release: bool) -> Result<std::process::Child> {
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
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        let digest = format!("{:016x}", hasher.finish());

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
    use super::{classify_path_change, normalize_relative_path, ProjectModel};
    use std::path::{Path, PathBuf};

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
}
