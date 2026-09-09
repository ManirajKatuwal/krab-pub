use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

mod auth_ops;
mod dev_workflow;
mod doctor;
mod env_policy;
mod generator;
mod governance;
mod project_model;
mod project_template;
mod release_ops;
mod topology;

use crate::auth_ops::dispatch_auth_action;
use crate::dev_workflow::{
    bootstrap_local_stack, build_project, dev_project, generate_docs, validate_environment,
    watch_project,
};
use crate::doctor::dispatch_doctor_command;
use crate::generator::dispatch_gen_resource;
use crate::governance::{
    dispatch_contract_action, dispatch_db_action, dispatch_release_action, dispatch_security_action,
};
use crate::project_template::generate_project_from_template;
use crate::topology::dispatch_topology_action;

#[derive(Parser)]
#[command(name = "krab")]
#[command(about = "Krab Framework CLI", long_about = None)]
// Reports the `krab_cli` package version, which is the workspace version. Users
// installing from crates.io need this to tell which release they have, and CI
// uses `krab --version` as the smoke test that `[[bin]] name = "krab"` took.
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Emit richer command diagnostics
    //
    // Global rather than redeclared on each gate. It used to be a separate
    // `--diagnostics` field on ten subcommands, which is ten places for the
    // help text and behaviour to drift, and it meant `krab --diagnostics
    // doctor` was a parse error while `krab doctor --diagnostics` worked.
    //
    // `global = true` accepts the flag at either position, so every existing
    // invocation — CI's included — parses byte-for-byte as before. clap
    // rejects a redeclared global id, which is why the per-subcommand copies
    // are gone rather than kept "for compatibility".
    #[arg(long, global = true)]
    diagnostics: bool,

    /// Emit machine-readable JSON instead of human-readable output
    //
    // Previously only `release check|certify` had this, so the CI-facing gates
    // could not be consumed programmatically. `--json` changes only what is
    // *printed*; the exit status is identical in both modes for every command
    // that accepts it (see `release_ops::finish_release_check`).
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new krab project
    New {
        /// Name of the project
        name: String,
        /// Starter template to use
        #[arg(long, value_enum, default_value_t = ProjectTemplate::Default)]
        template: ProjectTemplate,
        /// Depend on a local Krab checkout instead of crates.io.
        ///
        /// Point this at the root of the krab repository. Used by the
        /// `generated-project` CI gate, which must build scaffolded output
        /// before that version exists on crates.io, and useful when testing a
        /// framework change against a fresh project.
        #[arg(long, value_name = "KRAB_REPO_ROOT")]
        path_deps: Option<PathBuf>,
        /// Do not run `git init` in the new project directory.
        ///
        /// The scaffold writes a `.gitignore`, so by default it also creates
        /// the repository that file is for — an empty one, with no commit.
        /// It is skipped automatically when the target is already inside a git
        /// work tree, and a missing or failing `git` is a warning, never a
        /// failed scaffold.
        #[arg(long)]
        no_git: bool,
    },
    /// Build the full stack application
    Build {
        /// Build in release mode
        #[arg(long)]
        release: bool,
        /// Selective rebuild target
        #[arg(long, value_enum, default_value_t = BuildTarget::All)]
        target: BuildTarget,
    },
    /// Run frontend dev workflow (build + run server)
    Dev {
        /// Build in release mode
        #[arg(long)]
        release: bool,
        /// Rebuild + restart frontend server automatically on file changes
        #[arg(long)]
        watch: bool,
        /// Polling interval in milliseconds while watching
        #[arg(long, default_value_t = 800)]
        poll_ms: u64,
        /// Debounce window in milliseconds before rebuild after file changes
        #[arg(long, default_value_t = 250)]
        settle_ms: u64,
    },
    /// Watch mode (build + restart frontend on changes)
    Watch {
        /// Build in release mode
        #[arg(long)]
        release: bool,
        /// Polling interval in milliseconds
        #[arg(long, default_value_t = 800)]
        poll_ms: u64,
        /// Debounce window in milliseconds before rebuild after file changes
        #[arg(long, default_value_t = 250)]
        settle_ms: u64,
    },
    /// Generate developer workflow documentation
    Docs {
        /// Output file path
        #[arg(long, default_value = "docs/guides/dev_workflow.md")]
        out: PathBuf,
    },
    /// Bootstrap full local stack in one command (build + orchestrator)
    Bootstrap {
        /// Build artifacts in release mode before starting stack
        #[arg(long)]
        release: bool,
        /// Skip build step and start orchestrator immediately
        #[arg(long)]
        skip_build: bool,
    },
    /// Validate common environment settings used by services
    EnvCheck {
        /// Fail with non-zero exit when warnings are found
        #[arg(long)]
        strict: bool,
    },
    /// Run API contract checks used by CI
    Contract {
        #[command(subcommand)]
        action: ContractAction,
    },
    /// Run database governance checks used by CI
    Db {
        #[command(subcommand)]
        action: DbAction,
    },
    /// Run security policy checks used by CI and local release gates
    Security {
        #[command(subcommand)]
        action: SecurityAction,
    },
    /// Authentication operator tooling
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Release pipeline pre-flight checks
    Release {
        #[command(subcommand)]
        action: ReleaseAction,
    },
    /// Run aggregated workspace health checks
    Doctor {
        /// Treat warnings as failures
        #[arg(long)]
        strict: bool,
    },
    /// Topology governance checks and extraction scaffolding
    Topology {
        #[command(subcommand)]
        action: TopologyAction,
    },
    /// Generate resources
    Gen {
        #[command(subcommand)]
        resource: GenResource,
    },
    /// Print a shell completion script to stdout
    ///
    /// Redirect it to wherever your shell loads completions from, for example:
    /// `krab completions bash > /etc/bash_completion.d/krab`, or
    /// `krab completions powershell | Out-String | Invoke-Expression`.
    Completions {
        /// Shell to generate a completion script for
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum ContractAction {
    /// Run contract checks and schema snapshots
    Check,
    /// Run protocol parity and resolver checks
    ProtocolCheck,
}

#[derive(Subcommand)]
enum DbAction {
    /// Run migration lifecycle checks
    Lifecycle,
    /// Run rollback simulation checks
    Rollback,
    /// Run migration drift-detection checks
    Drift,
    /// Run rollback rehearsal and capture evidence
    Rehearsal {
        /// Path for evidence output
        #[arg(
            long,
            default_value = "internal/audit/evidence/rollback-rehearsal-evidence.txt"
        )]
        out: PathBuf,
    },
}

#[derive(Subcommand)]
enum SecurityAction {
    /// Run dependency policy gate (cargo-deny advisories/licenses/bans/sources)
    DependencyGate,
}

#[derive(Subcommand)]
enum AuthAction {
    /// Hash a password with Argon2id for KRAB_AUTH_LOGIN_USERS_JSON
    ///
    /// Prints a PHC string to stdout. Outside `local` environments the auth
    /// service refuses to start on any credential that is not one of these.
    HashPassword {
        /// The password to hash.
        ///
        /// Prefer omitting this and piping on stdin — an argument is visible in
        /// the process list and shell history to every user on the machine.
        #[arg(long, value_name = "PASSWORD")]
        password: Option<String>,
        /// Emit a ready-to-paste JSON credential map entry for this username
        #[arg(long, value_name = "USERNAME")]
        username: Option<String>,
    },
}

#[derive(Subcommand)]
enum ReleaseAction {
    /// Run comprehensive pre-flight release checklist
    Check,
    /// Run certification gates and write an evidence bundle
    Certify {
        /// Output directory for the evidence bundle
        //
        // Defaults under `internal/audit/release-certify/`, the gitignored
        // evidence tree CLAUDE.md documents this command as writing to, and the
        // same parent CI targets (`ops-hardening.yaml` passes
        // `--out internal/audit/release-certify/run-<run_id>`). The previous
        // default, `release-evidence`, created an untracked directory in the
        // repository root that no `.gitignore` rule covered.
        #[arg(long, default_value = "internal/audit/release-certify/local")]
        out: PathBuf,
    },
}

#[derive(Subcommand)]
enum TopologyAction {
    /// Validate topology hygiene and service-boundary contract rules
    Doctor,
    /// Scaffold a split-service extraction skeleton for a domain
    Split {
        /// Domain name to extract (example: users, billing)
        domain: String,
        /// Protocol set for generated adapter stubs (CSV)
        #[arg(long, value_delimiter = ',')]
        protocols: Option<Vec<ServiceType>>,
        /// Register generated service in workspace Cargo.toml and krab.toml
        #[arg(long)]
        register: bool,
        /// Print planned files without writing
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum GenResource {
    /// Generate a new microservice
    Service {
        /// Name of the service (e.g., service_payment)
        name: String,
        /// Type of API to generate
        #[arg(long, value_enum)]
        r#type: ServiceType,
        /// Exposure mode (single/multi)
        #[arg(long, value_enum, default_value_t = ExposureMode::Single)]
        exposure_mode: ExposureMode,
        /// Protocol set for multi mode (CSV)
        #[arg(long, value_delimiter = ',')]
        protocols: Option<Vec<ServiceType>>,
        /// Deployment topology for generated services
        #[arg(long, value_enum, default_value_t = Topology::SingleService)]
        topology: Topology,
    },
    /// Generate a new component
    Component {
        /// Name of the component
        name: String,
    },
    /// Generate a new route
    Route {
        /// Name of the route
        name: String,
    },
    /// Generate a new server function
    ServerFunction {
        /// Name of the server function
        name: String,
    },
}

#[derive(Clone, ValueEnum, Debug, PartialEq, Eq)]
enum ServiceType {
    Rest,
    Graphql,
    Rpc,
    Grpc,
}

#[derive(Clone, ValueEnum, Debug, PartialEq, Eq)]
enum ExposureMode {
    Single,
    Multi,
}

#[derive(Clone, ValueEnum, Debug, PartialEq, Eq)]
enum Topology {
    SingleService,
    SplitServices,
}

#[derive(Clone, ValueEnum, Debug, PartialEq, Eq)]
enum ProjectTemplate {
    /// Minimal Krab service
    Default,
    /// SaaS service skeleton with auth-ready layers and tenant APIs
    Saas,
    /// Edge SSR policy skeleton with explicit render-policy metadata
    EdgeSsr,
    /// Event-stream dashboard with WebSocket and SSE
    EventStream,
    /// Full-stack SSR with WASM island hydration and server functions
    Fullstack,
}

#[derive(Clone, ValueEnum, Debug)]
enum BuildTarget {
    All,
    Frontend,
    Client,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    dispatch_command(&cli.command, cli.diagnostics, cli.json)
}

fn dispatch_command(command: &Commands, diagnostics: bool, json: bool) -> Result<()> {
    match command {
        Commands::Build { release, target } => {
            build_project(*release, target, diagnostics)?;
        }
        Commands::Dev {
            release,
            watch,
            poll_ms,
            settle_ms,
        } => {
            if *watch {
                watch_project(*release, *poll_ms, *settle_ms)?;
            } else {
                dev_project(*release)?;
            }
        }
        Commands::Watch {
            release,
            poll_ms,
            settle_ms,
        } => {
            watch_project(*release, *poll_ms, *settle_ms)?;
        }
        Commands::Docs { out } => {
            generate_docs(out)?;
        }
        Commands::Bootstrap {
            release,
            skip_build,
        } => {
            bootstrap_local_stack(*release, *skip_build)?;
        }
        Commands::EnvCheck { strict } => {
            validate_environment(*strict, json)?;
        }
        Commands::Contract { action } => dispatch_contract_action(action, diagnostics, json)?,
        Commands::Db { action } => dispatch_db_action(action, diagnostics, json)?,
        Commands::Security { action } => dispatch_security_action(action, diagnostics, json)?,
        Commands::Auth { action } => dispatch_auth_action(action)?,
        Commands::Release { action } => dispatch_release_action(action, diagnostics, json)?,
        Commands::Doctor { strict } => dispatch_doctor_command(diagnostics, *strict, json)?,
        Commands::Topology { action } => dispatch_topology_action(action, diagnostics, json)?,
        Commands::Gen { resource } => dispatch_gen_resource(resource)?,
        Commands::New {
            name,
            template,
            path_deps,
            no_git,
        } => {
            generate_project_from_template(name, template, path_deps.as_deref(), *no_git)?;
        }
        Commands::Completions { shell } => {
            // The binary name is `krab`, not the package name `krab_cli`, and
            // the completion script has to use the name the user actually
            // types — see the `[[bin]]` comment in Cargo.toml.
            let mut command = Cli::command();
            clap_complete::generate(*shell, &mut command, "krab", &mut std::io::stdout());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Cli, Commands, ContractAction, DbAction, ReleaseAction, SecurityAction, Shell};
    use clap::{CommandFactory, Parser};

    /// clap's own consistency audit. It catches exactly the class of mistake
    /// this change could introduce — a global argument id redeclared on a
    /// subcommand — and it panics with the offending id rather than failing at
    /// runtime in front of a user.
    #[test]
    fn the_command_tree_passes_claps_debug_assertions() {
        Cli::command().debug_assert();
    }

    /// The regression that would hurt most: `--diagnostics` and `--json` moved
    /// to the top level, and every documented CI invocation puts them *after*
    /// the subcommand. `global = true` is what keeps those parsing; if it were
    /// dropped, or an arg re-declared locally, this is where it shows up —
    /// not in a red workflow.
    ///
    /// The invocations below are copied from `.github/workflows/*.yaml` and
    /// `CLAUDE.md`, argv-for-argv.
    #[test]
    fn documented_ci_invocations_still_parse() {
        // .github/workflows/ops-hardening.yaml
        let cli = Cli::try_parse_from(["krab", "security", "dependency-gate", "--diagnostics"])
            .expect("security dependency-gate --diagnostics");
        assert!(cli.diagnostics);
        assert!(!cli.json);
        assert!(matches!(
            cli.command,
            Commands::Security {
                action: SecurityAction::DependencyGate
            }
        ));

        // .github/workflows/api-contract.yaml
        let cli = Cli::try_parse_from(["krab", "contract", "check", "--diagnostics"])
            .expect("contract check --diagnostics");
        assert!(cli.diagnostics);
        assert!(matches!(
            cli.command,
            Commands::Contract {
                action: ContractAction::Check
            }
        ));

        let cli = Cli::try_parse_from(["krab", "contract", "protocol-check", "--diagnostics"])
            .expect("contract protocol-check --diagnostics");
        assert!(cli.diagnostics);

        // .github/workflows/db-lifecycle.yaml
        for action in ["lifecycle", "rollback", "drift"] {
            let cli = Cli::try_parse_from(["krab", "db", action, "--diagnostics"])
                .unwrap_or_else(|err| panic!("db {action} --diagnostics: {err}"));
            assert!(cli.diagnostics, "db {action}");
        }

        // A global flag must coexist with a subcommand-local option, in the
        // order CI writes them.
        let cli = Cli::try_parse_from([
            "krab",
            "db",
            "rehearsal",
            "--out",
            "internal/audit/evidence/rollback-rehearsal-evidence.txt",
            "--diagnostics",
        ])
        .expect("db rehearsal --out <path> --diagnostics");
        assert!(cli.diagnostics);
        match cli.command {
            Commands::Db {
                action: DbAction::Rehearsal { out },
            } => assert_eq!(
                out.to_string_lossy(),
                "internal/audit/evidence/rollback-rehearsal-evidence.txt"
            ),
            _ => panic!("expected db rehearsal"),
        }

        // .github/workflows/ops-hardening.yaml release certification
        let cli = Cli::try_parse_from([
            "krab",
            "release",
            "certify",
            "--out",
            "internal/audit/release-certify/run-42",
            "--json",
        ])
        .expect("release certify --out <dir> --json");
        assert!(cli.json);
        assert!(!cli.diagnostics);
        match cli.command {
            Commands::Release {
                action: ReleaseAction::Certify { out },
            } => assert_eq!(
                out.to_string_lossy(),
                "internal/audit/release-certify/run-42"
            ),
            _ => panic!("expected release certify"),
        }

        // CLAUDE.md governance command list
        let cli = Cli::try_parse_from(["krab", "release", "check", "--diagnostics", "--json"])
            .expect("release check --diagnostics --json");
        assert!(cli.diagnostics && cli.json);

        let cli = Cli::try_parse_from(["krab", "doctor", "--diagnostics", "--strict"])
            .expect("doctor --diagnostics --strict");
        assert!(cli.diagnostics);
        assert!(matches!(cli.command, Commands::Doctor { strict: true }));

        let cli =
            Cli::try_parse_from(["krab", "env-check", "--strict"]).expect("env-check --strict");
        assert!(matches!(cli.command, Commands::EnvCheck { strict: true }));

        let cli = Cli::try_parse_from(["krab", "topology", "doctor", "--diagnostics"])
            .expect("topology doctor --diagnostics");
        assert!(cli.diagnostics);

        let cli = Cli::try_parse_from(["krab", "db", "rehearsal"]).expect("db rehearsal");
        assert!(!cli.diagnostics);
    }

    /// The point of `global = true`: the flag is now accepted before the
    /// subcommand as well, which the per-subcommand declarations rejected.
    #[test]
    fn global_flags_are_accepted_before_the_subcommand_too() {
        let cli = Cli::try_parse_from(["krab", "--diagnostics", "--json", "doctor"])
            .expect("global flags should parse ahead of the subcommand");
        assert!(cli.diagnostics && cli.json);
    }

    /// `--json` is now available to the gates that previously had no
    /// machine-readable output at all. If a future change re-scopes it to a
    /// subset of commands, these stop parsing.
    #[test]
    fn json_reaches_the_ci_facing_gates_that_previously_lacked_it() {
        for argv in [
            vec!["krab", "doctor", "--json"],
            vec!["krab", "env-check", "--json"],
            vec!["krab", "topology", "doctor", "--json"],
            vec!["krab", "contract", "check", "--json"],
            vec!["krab", "db", "drift", "--json"],
            vec!["krab", "security", "dependency-gate", "--json"],
        ] {
            let cli = Cli::try_parse_from(argv.clone())
                .unwrap_or_else(|err| panic!("{argv:?} should parse: {err}"));
            assert!(cli.json, "{argv:?}");
        }
    }

    /// Every shell the subcommand advertises must actually be a value clap
    /// accepts, and the script must be non-empty and name the binary `krab`
    /// rather than the package `krab_cli`.
    #[test]
    fn completions_are_generated_for_every_advertised_shell() {
        for (name, shell) in [
            ("bash", Shell::Bash),
            ("zsh", Shell::Zsh),
            ("fish", Shell::Fish),
            ("powershell", Shell::PowerShell),
            ("elvish", Shell::Elvish),
        ] {
            let cli = Cli::try_parse_from(["krab", "completions", name])
                .unwrap_or_else(|err| panic!("completions {name} should parse: {err}"));
            assert!(matches!(cli.command, Commands::Completions { .. }));

            let mut script = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "krab", &mut script);
            let script = String::from_utf8(script).expect("completion script is UTF-8");

            assert!(!script.is_empty(), "{name} script should not be empty");
            assert!(
                script.contains("krab"),
                "{name} script should reference the `krab` binary"
            );
            assert!(
                script.contains("doctor"),
                "{name} script should list subcommands, got:\n{script}"
            );
        }
    }
}
