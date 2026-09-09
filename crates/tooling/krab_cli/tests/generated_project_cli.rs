//! End-to-end tests for the `krab` binary as a user meets it: the real
//! executable, run in a real `krab new` project, in a temporary directory.
//!
//! These exist because `krab_cli` had ~88 unit tests and *no* integration
//! tests, and that gap shipped a bug. `krab doctor` read
//! `crates/framework/krab_core/src/service_contract.rs` unconditionally, so it
//! hard-failed in every project that was not the Krab framework workspace
//! itself:
//!
//! ```text
//! $ krab new demo_app --template default
//! $ cd demo_app && krab doctor --diagnostics
//! Error: Failed reading crates/framework/krab_core/src/service_contract.rs
//! EXIT=1
//! ```
//!
//! Every unit test passed the whole time, because every unit test called an
//! evaluator directly with a hand-built root. Only running the shipped binary
//! from a scaffolded project's working directory reproduces it.
//!
//! Conventions for anything added here:
//!
//! - The binary comes from `CARGO_BIN_EXE_krab`, the path Cargo hands to
//!   integration tests. It is named `krab`, not `krab_cli` — see the `[[bin]]`
//!   section in this crate's manifest.
//! - The child's working directory is set with [`Command::current_dir`]. Never
//!   `std::env::set_current_dir`: these tests run in parallel in one process,
//!   and a process-global CWD change races with every other test.
//! - Every invocation goes through [`krab_command`], which pins the environment
//!   variables the CLI reads, so a developer's shell cannot change the result.
//! - Nothing here may invoke a cargo or wasm toolchain (`krab build`, `dev`,
//!   `watch`, `release check`, `release certify`). `new` and `gen` only write
//!   files, which is what keeps this suite in the low seconds.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Project name used by the scaffolding helpers.
const PROJECT: &str = "demo_app";

/// `krab_core` feature aliases that are deprecated and scheduled for removal.
/// A generated project pinned to one of these breaks the day it goes away, so
/// no generator output may name them. See ADR 0007 for the `grpc` rename.
const DEPRECATED_KRAB_CORE_ALIASES: &[&str] = &["grpc", "db"];

/// A finished `krab` invocation.
struct Run {
    args: String,
    code: Option<i32>,
    success: bool,
    stdout: String,
    stderr: String,
}

impl Run {
    /// Panic message that carries everything needed to diagnose a failure
    /// without re-running the command by hand.
    fn report(&self) -> String {
        format!(
            "`krab {}` exited with {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.args, self.code, self.stdout, self.stderr
        )
    }
}

/// A `krab` invocation with a deterministic environment.
///
/// `krab doctor`'s environment-policy check reads `KRAB_AUTH_MODE`,
/// `KRAB_OIDC_ISSUER`, `KRAB_OIDC_AUDIENCE` and `KRAB_ENVIRONMENT`, and the
/// topology doctor validates `KRAB_RUNTIME_TOPOLOGY` /
/// `KRAB_RUNTIME_ENDPOINTS_JSON` with the strict parser. All six are inherited
/// from whatever shell invoked `cargo test`, so a developer with a half-configured
/// `.env` sourced would see different check levels than CI. They are pinned to
/// the combination that produces no warnings (`static` auth in a `dev`
/// environment, no runtime topology overrides) so the assertions below are
/// about the CLI, not about the machine.
fn krab_command(cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_krab"));
    command
        .current_dir(cwd)
        .env("KRAB_AUTH_MODE", "static")
        .env("KRAB_ENVIRONMENT", "dev")
        .env_remove("KRAB_OIDC_ISSUER")
        .env_remove("KRAB_OIDC_AUDIENCE")
        .env_remove("KRAB_RUNTIME_TOPOLOGY")
        .env_remove("KRAB_RUNTIME_ENDPOINTS_JSON");
    command
}

fn run(cwd: &Path, args: &[&str]) -> Run {
    let output = krab_command(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn `krab {}`: {err}", args.join(" ")));

    Run {
        args: args.join(" "),
        code: output.status.code(),
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn run_ok(cwd: &Path, args: &[&str]) -> Run {
    let run = run(cwd, args);
    assert!(run.success, "{}", run.report());
    run
}

/// Scaffold a project with the real `krab new` and return its root.
///
/// The `TempDir` is returned alongside because dropping it deletes the tree.
fn scaffold(template: &str) -> (TempDir, PathBuf) {
    let temp = TempDir::new().expect("tempdir");
    let created = run_ok(temp.path(), &["new", PROJECT, "--template", template]);
    assert!(
        created.stdout.contains("created successfully"),
        "{}",
        created.report()
    );

    let root = temp.path().join(PROJECT);
    for relative in ["Cargo.toml", "krab.toml", "src/main.rs"] {
        assert!(
            root.join(relative).is_file(),
            "`krab new` did not write {relative}:\n{}",
            created.report()
        );
    }
    (temp, root)
}

fn read(root: &Path, relative: &str) -> String {
    fs::read_to_string(root.join(relative))
        .unwrap_or_else(|err| panic!("{relative} unreadable: {err}"))
}

/// How many lines of `source` are exactly the declaration `decl`.
///
/// Whole-line rather than substring: the scaffolded `src/main.rs` explains the
/// marker with the prose "`krab gen` inserts module declarations (for example
/// `mod routes;`)", so a substring count of `mod routes;` is 2 in a correctly
/// wired project and 1 in a broken one — the wrong way round.
fn declaration_lines(source: &str, decl: &str) -> usize {
    source.lines().filter(|line| line.trim() == decl).count()
}

/// One `[LEVEL] name` line of a `krab doctor` report plus its `  - detail`
/// lines.
#[derive(Debug)]
struct DoctorCheck {
    name: String,
    level: String,
    details: Vec<String>,
}

impl DoctorCheck {
    fn has_detail_containing(&self, needle: &str) -> bool {
        self.details.iter().any(|detail| detail.contains(needle))
    }
}

fn parse_doctor_report(stdout: &str) -> Vec<DoctorCheck> {
    let mut checks: Vec<DoctorCheck> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim_end();
        if let Some(rest) = trimmed.strip_prefix('[') {
            if let Some((level, name)) = rest.split_once("] ") {
                checks.push(DoctorCheck {
                    name: name.trim().to_string(),
                    level: level.to_string(),
                    details: Vec::new(),
                });
                continue;
            }
        }
        if let Some(detail) = trimmed.trim_start().strip_prefix("- ") {
            if let Some(current) = checks.last_mut() {
                current.details.push(detail.to_string());
            }
        }
    }
    checks
}

fn doctor_check<'a>(checks: &'a [DoctorCheck], name: &str) -> &'a DoctorCheck {
    checks
        .iter()
        .find(|check| check.name == name)
        .unwrap_or_else(|| panic!("no `{name}` check in the doctor report: {checks:#?}"))
}

/// The `features = [...]` list of the `krab_core` dependency in a generated
/// manifest.
///
/// Hand-rolled rather than parsed with a TOML crate so this test target needs
/// nothing beyond `std` and `tempfile`. It reads the file the generator
/// actually wrote, so it still fails on a wrong feature name.
fn krab_core_features(manifest: &str) -> Vec<String> {
    let line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("krab_core ="))
        .unwrap_or_else(|| panic!("generated manifest declares no krab_core:\n{manifest}"));
    let opened = line
        .find("features = [")
        .map(|at| at + "features = [".len())
        .unwrap_or_else(|| panic!("krab_core dependency carries no feature list: {line}"));
    let rest = &line[opened..];
    let closed = rest
        .find(']')
        .unwrap_or_else(|| panic!("unterminated krab_core feature list: {line}"));

    rest[..closed]
        .split(',')
        .map(|feature| feature.trim().trim_matches('"').to_string())
        .filter(|feature| !feature.is_empty())
        .collect()
}

fn assert_no_deprecated_aliases(features: &[String], context: &str) {
    for feature in features {
        assert!(
            !DEPRECATED_KRAB_CORE_ALIASES.contains(&feature.as_str()),
            "{context} requests the deprecated krab_core alias {feature:?}; scaffold against \
             the canonical feature so removing the alias cannot break generated projects. \
             Emitted: {features:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// `krab new` -> `krab doctor` round trip.
// ---------------------------------------------------------------------------

/// `--strict` promotes warnings to failures, so anything the CLI warns about
/// in an untouched scaffold makes this exit non-zero. That happened: a
/// generated `krab.toml` declares `[project]` and no `[services.*]`, and the
/// service-config check warned "no [services.*] entries found" — the framework
/// workspace's four-service shape applied to a project that has one binary and
/// wants no orchestrator. Both the generated README and the scaffolded CI
/// workflow run `krab doctor`, so a brand-new project failed its own gate
/// before its author had written a line.
#[test]
fn doctor_strict_passes_on_an_untouched_scaffold() {
    let (_temp, root) = scaffold("default");

    let strict = run(&root, &["doctor", "--diagnostics", "--strict"]);

    assert!(
        strict.success,
        "a freshly generated project must pass its own strict gate: {}",
        strict.report()
    );
}

/// The exact regression this file exists for.
///
/// `krab new demo_app --template default` followed by `krab doctor
/// --diagnostics` inside the generated project used to print
/// `Error: Failed reading crates/framework/krab_core/src/service_contract.rs`
/// and exit 1 — a framework-repo assumption reported to the user as their bug.
/// Two things have to hold now, and the second is as important as the first:
/// the command succeeds, *and* the checks that could not run say SKIP rather
/// than quietly passing, because a green line beside a check that never
/// executed claims coverage the project does not have.
#[test]
fn doctor_in_a_generated_project_passes_and_marks_framework_only_checks_skipped() {
    let (_temp, root) = scaffold("default");

    let doctor = run_ok(&root, &["doctor", "--diagnostics"]);
    let checks = parse_doctor_report(&doctor.stdout);

    // The whole report survives. Before the fix a single evaluator's `Err`
    // discarded the checks that had already succeeded.
    let names: Vec<&str> = checks.iter().map(|check| check.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "project-model",
            "service-config",
            "environment-policy",
            "topology-boundaries",
        ],
        "{}",
        doctor.report()
    );

    let topology = doctor_check(&checks, "topology-boundaries");
    assert_eq!(topology.level, "SKIP", "{}", doctor.report());
    assert!(
        topology.has_detail_containing("contract-payload-derives")
            && topology.has_detail_containing("only exists in a Krab framework checkout"),
        "the skip must name the check and why it does not apply: {}",
        doctor.report()
    );
    assert!(
        topology.has_detail_containing("service-source-scan"),
        "a generated project has no services/ directory, so that scan is skipped too: {}",
        doctor.report()
    );

    // The framework file is reported as absent, never as a read failure.
    assert!(
        !doctor.stdout.contains("Failed reading") && !doctor.stderr.contains("Failed reading"),
        "{}",
        doctor.report()
    );
    assert!(
        !doctor.stdout.contains("check could not run"),
        "{}",
        doctor.report()
    );
    assert!(!doctor.stdout.contains("[FAIL]"), "{}", doctor.report());

    // The summary must not read as full coverage.
    assert!(
        doctor.stdout.contains("check(s) skipped as not applicable"),
        "the summary line hides that part of the suite never ran: {}",
        doctor.report()
    );

    // Checks that *do* apply to a generated project still run for real.
    let project_model = doctor_check(&checks, "project-model");
    assert_eq!(project_model.level, "OK", "{}", doctor.report());
    assert!(
        project_model.has_detail_containing(&format!("frontend_bin={PROJECT}")),
        "project-model must read the generated krab.toml, not the workspace default: {}",
        doctor.report()
    );

    // A scaffold declares `[project]` and no `[services.*]`: one binary, with
    // nothing for the orchestrator to supervise. That is reported as SKIP, not
    // WARN — warning made `krab doctor --strict` fail on every freshly
    // generated project, and both the generated README and the scaffolded CI
    // run that command. The skip must say why rather than going quiet.
    let service_config = doctor_check(&checks, "service-config");
    assert_eq!(service_config.level, "SKIP", "{}", doctor.report());
    assert!(
        service_config.has_detail_containing("single-service project"),
        "a skipped service-config must explain itself: {}",
        doctor.report()
    );

    // Pinned by `krab_command`, so this is a statement about the CLI.
    assert_eq!(
        doctor_check(&checks, "environment-policy").level,
        "OK",
        "{}",
        doctor.report()
    );
}

/// The counterpart to the test above: tolerating an absent framework tree must
/// not turn the boundary check into a blanket skip. Inside a real framework
/// checkout the contract file is there and the sub-check has to run against it.
///
/// Deliberately does not assert an exit code — the working tree may legitimately
/// carry topology violations while someone is mid-change, and this test is about
/// *which checks executed*, not about the repository being clean.
#[test]
fn doctor_in_the_framework_checkout_still_runs_the_contract_check() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let contract = repo_root.join("crates/framework/krab_core/src/service_contract.rs");
    if !contract.is_file() {
        eprintln!(
            "{} is absent; skipping the framework-checkout contrast",
            contract.display()
        );
        return;
    }

    let doctor = run(&repo_root, &["doctor", "--diagnostics"]);
    let checks = parse_doctor_report(&doctor.stdout);
    let topology = doctor_check(&checks, "topology-boundaries");

    assert!(
        topology.has_detail_containing("contract_path=")
            && topology.has_detail_containing("service_contract.rs"),
        "the contract payload check must actually run here: {}",
        doctor.report()
    );
    assert!(
        !topology.has_detail_containing("skipped contract-payload-derives"),
        "the framework checkout has the contract file; skipping it would hide real violations: {}",
        doctor.report()
    );
    assert!(
        topology.has_detail_containing("checked_rust_files="),
        "services/ exists here, so the source scan must run: {}",
        doctor.report()
    );
}

/// `krab topology doctor` failed in generated projects for the same reason
/// `krab doctor` did — it is the same report underneath. It must exit 0 and
/// name every check it did not run.
#[test]
fn topology_doctor_in_a_generated_project_passes_and_names_the_skipped_checks() {
    let (_temp, root) = scaffold("default");

    let topology = run_ok(&root, &["topology", "doctor", "--diagnostics"]);

    assert!(
        topology
            .stdout
            .contains("skipped (not applicable to this project):"),
        "{}",
        topology.report()
    );
    for check in ["service-source-scan", "contract-payload-derives"] {
        assert!(
            topology.stdout.contains(check),
            "`{check}` is not reported as skipped: {}",
            topology.report()
        );
    }
    assert!(
        topology
            .stdout
            .contains("topology doctor passed (2 check(s) skipped as not applicable)"),
        "a pass must be qualified by how much of the suite never ran: {}",
        topology.report()
    );

    // A generated project *does* have a krab.toml, so the orchestrator policy
    // check is not skipped — the skip list has to be per-artifact, not global.
    assert!(
        topology
            .stdout
            .contains("checked orchestrator service health/restart policy in krab.toml"),
        "{}",
        topology.report()
    );
    assert!(
        !topology.stdout.contains("orchestrator-service-config:"),
        "krab.toml is present, so that check must not be listed as skipped: {}",
        topology.report()
    );
}

// ---------------------------------------------------------------------------
// Generator no-clobber.
//
// `gen component`, `gen route` and `gen server-function` used a bare
// `fs::write`, so a second run silently replaced hand-edited code with
// boilerplate and still printed a success line. Data loss reported as success.
// ---------------------------------------------------------------------------

/// Re-running a generator over a file the user has edited must keep the user's
/// bytes, report that it kept them, and still succeed — the wiring steps that
/// follow are how a user repairs a `mod` line they deleted.
fn assert_second_run_keeps_hand_written_content(args: &[&str], relative: &str) {
    let (_temp, root) = scaffold("default");
    run_ok(&root, args);

    let generated = root.join(relative);
    assert!(
        generated.is_file(),
        "{relative} was not generated by `krab {}`",
        args.join(" ")
    );

    let sentinel = format!("// hand written, must survive `krab {}`\n", args.join(" "));
    fs::write(&generated, &sentinel).expect("simulate a user edit");
    let main_before = read(&root, "src/main.rs");

    let second = run_ok(&root, args);

    assert_eq!(
        fs::read_to_string(&generated).expect("file still readable"),
        sentinel,
        "the second `krab {}` overwrote hand-written content in {relative}:\n{}",
        args.join(" "),
        second.report()
    );
    assert!(
        second
            .stdout
            .contains("kept as is, nothing was overwritten"),
        "a re-run must say it kept the file, not claim it created one: {}",
        second.report()
    );
    assert!(
        !second.stdout.contains("created at"),
        "the second run reports a creation that did not happen: {}",
        second.report()
    );
    assert_eq!(
        read(&root, "src/main.rs"),
        main_before,
        "a re-run must not touch src/main.rs again: {}",
        second.report()
    );
}

#[test]
fn gen_component_run_twice_keeps_the_users_file() {
    assert_second_run_keeps_hand_written_content(
        &["gen", "component", "Counter"],
        "src/components/counter.rs",
    );
}

#[test]
fn gen_route_run_twice_keeps_the_users_file() {
    assert_second_run_keeps_hand_written_content(&["gen", "route", "About"], "src/routes/about.rs");
}

#[test]
fn gen_server_function_run_twice_keeps_the_users_file() {
    assert_second_run_keeps_hand_written_content(
        &["gen", "server-function", "load_user"],
        "src/server_functions/load_user.rs",
    );
}

/// A second route must extend the existing index rather than duplicate or
/// replace it — the module index is shared state between generator runs.
#[test]
fn a_second_generated_route_extends_the_existing_index() {
    let (_temp, root) = scaffold("default");
    run_ok(&root, &["gen", "route", "About"]);
    run_ok(&root, &["gen", "route", "Contact"]);

    let mod_rs = read(&root, "src/routes/mod.rs");
    for decl in ["pub mod about;", "pub mod contact;"] {
        assert_eq!(
            declaration_lines(&mod_rs, decl),
            1,
            "expected exactly one `{decl}`:\n{mod_rs}"
        );
    }
    assert!(
        mod_rs.contains("router = router.route(\"/contact\", get(contact::handler));"),
        "{mod_rs}"
    );

    let main_rs = read(&root, "src/main.rs");
    assert_eq!(
        main_rs.matches("routes::router()").count(),
        1,
        "the router is merged once regardless of route count:\n{main_rs}"
    );
}

// ---------------------------------------------------------------------------
// Generated code has to be reachable.
//
// `krab gen route about` wrote src/routes/about.rs and stopped. A `krab new`
// project has no build.rs and no `mod routes;`, so nothing ever declared the
// module: the file was dead, `cargo build` never saw it, and the route silently
// did not exist.
// ---------------------------------------------------------------------------

#[test]
fn generated_modules_are_declared_in_main_and_the_route_is_registered() {
    let (_temp, root) = scaffold("default");
    run_ok(&root, &["gen", "component", "Counter"]);
    run_ok(&root, &["gen", "route", "About"]);
    run_ok(&root, &["gen", "server-function", "load_user"]);

    let main_rs = read(&root, "src/main.rs");
    for decl in ["mod components;", "mod routes;", "mod server_functions;"] {
        assert_eq!(
            declaration_lines(&main_rs, decl),
            1,
            "src/main.rs must declare `{decl}` exactly once, or the generated file is never \
             compiled:\n{main_rs}"
        );
    }

    // The merge has to land where `app` is a built router and before it is
    // served, otherwise the generated project does not compile or does not
    // serve the route.
    let merge_at = main_rs
        .find("app.merge(routes::router())")
        .unwrap_or_else(|| panic!("the routes router is never merged into `app`:\n{main_rs}"));
    let router_built_at = main_rs
        .find("Router::new()")
        .unwrap_or_else(|| panic!("scaffold no longer builds a Router:\n{main_rs}"));
    let served_at = main_rs
        .find("axum::serve(listener, app)")
        .unwrap_or_else(|| panic!("scaffold never serves `app`:\n{main_rs}"));
    assert!(
        router_built_at < merge_at && merge_at < served_at,
        "the merge is outside the window where it type-checks:\n{main_rs}"
    );

    // The index has to declare the module *and* register the handler; a
    // declaration alone still leaves the route unreachable.
    let routes_mod = read(&root, "src/routes/mod.rs");
    assert!(routes_mod.contains("pub mod about;"), "{routes_mod}");
    assert!(
        routes_mod.contains("router = router.route(\"/about\", get(about::handler));"),
        "{routes_mod}"
    );

    for (relative, decl) in [
        ("src/components/mod.rs", "pub mod counter;"),
        ("src/server_functions/mod.rs", "pub mod load_user;"),
    ] {
        let index = read(&root, relative);
        assert!(index.contains(decl), "{relative}:\n{index}");
    }

    // The handler the index registers must be the item the route file exports.
    let route_rs = read(&root, "src/routes/about.rs");
    assert!(
        route_rs.contains("pub async fn handler()"),
        "the index registers `about::handler`, which the route module must export:\n{route_rs}"
    );
}

// ---------------------------------------------------------------------------
// Deprecated `krab_core` feature aliases.
// ---------------------------------------------------------------------------

/// `--type grpc` scaffolds against `grpc-semantics`, the canonical feature.
/// It emitted the deprecated `grpc` alias, which is slated for removal, so
/// every service generated with it was pinned to a feature that is going away.
/// ADR 0007 covers the rename; `grpc-semantics` is status-code and header
/// vocabulary, not a gRPC transport.
#[test]
fn gen_service_grpc_emits_the_canonical_feature_not_the_deprecated_alias() {
    let temp = TempDir::new().expect("tempdir");
    let generated = run_ok(
        temp.path(),
        &["gen", "service", "payments_api", "--type", "grpc"],
    );
    assert!(
        generated.stdout.contains("created successfully"),
        "{}",
        generated.report()
    );

    let manifest = read(temp.path(), "payments_api/Cargo.toml");
    let features = krab_core_features(&manifest);

    assert_eq!(features, ["grpc-semantics"], "{manifest}");
    assert_no_deprecated_aliases(&features, "`krab gen service --type grpc`");
}

/// The same rule for the other protocols the generator can be asked for. `rpc`
/// maps onto `rest` because Krab serves RPC over the REST surface and has no
/// separate feature.
#[test]
fn gen_service_emits_only_features_krab_core_declares() {
    for (protocol, expected) in [("rest", "rest"), ("graphql", "graphql"), ("rpc", "rest")] {
        let temp = TempDir::new().expect("tempdir");
        let name = format!("svc_{protocol}");
        run_ok(
            temp.path(),
            &["gen", "service", name.as_str(), "--type", protocol],
        );

        let manifest = read(temp.path(), &format!("{name}/Cargo.toml"));
        let features = krab_core_features(&manifest);

        assert_eq!(features, [expected], "--type {protocol}:\n{manifest}");
        assert_no_deprecated_aliases(&features, &format!("`krab gen service --type {protocol}`"));
    }
}

/// The `saas` template needs a Postgres driver, and `db` is the deprecated
/// alias for `db-postgres`. It also once emitted `features = ["db, rest"]` —
/// one feature literally named `db, rest` — so the list is checked element by
/// element, not as a substring.
#[test]
fn the_saas_template_requests_db_postgres_not_the_deprecated_db_alias() {
    let (_temp, root) = scaffold("saas");

    let manifest = read(&root, "Cargo.toml");
    let mut features = krab_core_features(&manifest);
    features.sort();

    assert_eq!(features, ["db-postgres", "rest"], "{manifest}");
    assert_no_deprecated_aliases(&features, "`krab new --template saas`");
}

#[test]
fn the_fullstack_template_configures_dual_targets_and_wasm_assets() {
    let (_temp, root) = scaffold("fullstack");

    let manifest = read(&root, "Cargo.toml");
    assert!(
        manifest.contains("crate-type = [\"cdylib\", \"rlib\"]"),
        "{manifest}"
    );
    assert!(
        manifest.contains("[target.'cfg(not(target_arch = \"wasm32\"))'.dependencies]"),
        "{manifest}"
    );
    assert!(
        manifest.contains("[target.'cfg(target_arch = \"wasm32\")'.dependencies]"),
        "{manifest}"
    );
    assert!(manifest.contains("features = [\"web\"]"), "{manifest}");

    let lib_rs = read(&root, "src/lib.rs");
    assert!(lib_rs.contains("#[island]"), "{lib_rs}");
    assert!(lib_rs.contains("#[server]"), "{lib_rs}");
    assert!(lib_rs.contains("krab_boot"), "{lib_rs}");

    let main_rs = read(&root, "src/main.rs");
    assert!(main_rs.contains("greet_server_handler"), "{main_rs}");
    assert!(main_rs.contains("ServeDir::new(\"dist\")"), "{main_rs}");

    let krab_toml = read(&root, "krab.toml");
    assert!(
        krab_toml.contains(&format!("client_package = \"{PROJECT}\"")),
        "{krab_toml}"
    );
    assert!(
        krab_toml.contains(&format!("client_artifact_stem = \"{PROJECT}\"")),
        "{krab_toml}"
    );

    let strict = run(&root, &["doctor", "--diagnostics", "--strict"]);
    assert!(
        strict.success,
        "a freshly generated fullstack project must pass its own strict gate: {}",
        strict.report()
    );
}

// ---------------------------------------------------------------------------
// Binary identity.
// ---------------------------------------------------------------------------

/// The crates.io package is `krab_cli` but the binary is `krab`, via an
/// explicit `[[bin]]` section — without it every documented command
/// (`krab doctor`, `krab new`) would be spelled `krab_cli`. CI uses
/// `krab --version` as the smoke test that the rename took, and the version it
/// prints is what a user reports in a bug.
#[test]
fn version_flag_reports_the_binary_name_and_crate_version() {
    let temp = TempDir::new().expect("tempdir");
    let version = run_ok(temp.path(), &["--version"]);

    assert_eq!(
        version.stdout.trim(),
        format!("krab {}", env!("CARGO_PKG_VERSION")),
        "{}",
        version.report()
    );
}
