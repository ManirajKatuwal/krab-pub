use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

mod dev_workflow;
mod doctor;
mod generator;
mod governance;
mod project_model;
mod project_template;
mod release_ops;
mod topology;

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
struct Cli {
    #[command(subcommand)]
    command: Commands,
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
    },
    /// Build the full stack application
    Build {
        /// Build in release mode
        #[arg(long)]
        release: bool,
        /// Selective rebuild target
        #[arg(long, value_enum, default_value_t = BuildTarget::All)]
        target: BuildTarget,
        /// Emit richer command diagnostics
        #[arg(long)]
        diagnostics: bool,
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
    /// Release pipeline pre-flight checks
    Release {
        #[command(subcommand)]
        action: ReleaseAction,
    },
    /// Run aggregated workspace health checks
    Doctor {
        /// Emit richer check details
        #[arg(long)]
        diagnostics: bool,
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
}

#[derive(Subcommand)]
enum ContractAction {
    /// Run contract checks and schema snapshots
    Check {
        /// Emit richer command diagnostics
        #[arg(long)]
        diagnostics: bool,
    },
    /// Run protocol parity and resolver checks
    ProtocolCheck {
        /// Emit richer command diagnostics
        #[arg(long)]
        diagnostics: bool,
    },
}

#[derive(Subcommand)]
enum DbAction {
    /// Run migration lifecycle checks
    Lifecycle {
        /// Emit richer command diagnostics
        #[arg(long)]
        diagnostics: bool,
    },
    /// Run rollback simulation checks
    Rollback {
        /// Emit richer command diagnostics
        #[arg(long)]
        diagnostics: bool,
    },
    /// Run migration drift-detection checks
    Drift {
        /// Emit richer command diagnostics
        #[arg(long)]
        diagnostics: bool,
    },
    /// Run rollback rehearsal and capture evidence
    Rehearsal {
        /// Path for evidence output
        #[arg(
            long,
            default_value = "internal/audit/evidence/rollback-rehearsal-evidence.txt"
        )]
        out: PathBuf,
        /// Emit richer command diagnostics
        #[arg(long)]
        diagnostics: bool,
    },
}

#[derive(Subcommand)]
enum SecurityAction {
    /// Run dependency policy gate (cargo-deny advisories/licenses/bans/sources)
    DependencyGate {
        /// Emit richer command diagnostics
        #[arg(long)]
        diagnostics: bool,
    },
}

#[derive(Subcommand)]
enum ReleaseAction {
    /// Run comprehensive pre-flight release checklist
    Check {
        /// Emit richer command diagnostics
        #[arg(long)]
        diagnostics: bool,
        /// Output results as machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Run certification gates and write an evidence bundle
    Certify {
        /// Output directory for the evidence bundle
        #[arg(long, default_value = "release-evidence")]
        out: PathBuf,
        /// Emit richer command diagnostics
        #[arg(long)]
        diagnostics: bool,
        /// Output summary as machine-readable JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TopologyAction {
    /// Validate topology hygiene and service-boundary contract rules
    Doctor {
        /// Emit richer command diagnostics
        #[arg(long)]
        diagnostics: bool,
    },
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
}

#[derive(Clone, ValueEnum, Debug)]
enum BuildTarget {
    All,
    Frontend,
    Client,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    dispatch_command(&cli.command)
}

fn dispatch_command(command: &Commands) -> Result<()> {
    match command {
        Commands::Build {
            release,
            target,
            diagnostics,
        } => {
            build_project(*release, target, *diagnostics)?;
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
            validate_environment(*strict)?;
        }
        Commands::Contract { action } => dispatch_contract_action(action)?,
        Commands::Db { action } => dispatch_db_action(action)?,
        Commands::Security { action } => dispatch_security_action(action)?,
        Commands::Release { action } => dispatch_release_action(action)?,
        Commands::Doctor {
            diagnostics,
            strict,
        } => dispatch_doctor_command(*diagnostics, *strict)?,
        Commands::Topology { action } => dispatch_topology_action(action)?,
        Commands::Gen { resource } => dispatch_gen_resource(resource)?,
        Commands::New { name, template } => {
            generate_project_from_template(name, template)?;
        }
    }

    Ok(())
}
