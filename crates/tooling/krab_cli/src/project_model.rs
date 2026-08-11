use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProjectModel {
    pub(super) frontend_bin: String,
    pub(super) client_package: Option<String>,
    pub(super) client_crate_dir: Option<PathBuf>,
    pub(super) client_artifact_stem: Option<String>,
    pub(super) public_dir: PathBuf,
    pub(super) dist_dir: PathBuf,
    pub(super) server_paths: Vec<PathBuf>,
    pub(super) client_paths: Vec<PathBuf>,
    pub(super) shared_paths: Vec<PathBuf>,
    pub(super) public_paths: Vec<PathBuf>,
    pub(super) bootstrap_bin: String,
    pub(super) hmr_signal_path: PathBuf,
}

#[derive(Debug, Deserialize, Default)]
struct KrabToml {
    #[serde(default)]
    project: Option<ProjectSection>,
}

#[derive(Debug, Deserialize, Default)]
struct ProjectSection {
    #[serde(default)]
    frontend_bin: Option<String>,
    #[serde(default)]
    client_package: Option<String>,
    #[serde(default)]
    client_crate_dir: Option<PathBuf>,
    #[serde(default)]
    client_artifact_stem: Option<String>,
    #[serde(default)]
    public_dir: Option<PathBuf>,
    #[serde(default)]
    dist_dir: Option<PathBuf>,
    #[serde(default)]
    server_paths: Vec<PathBuf>,
    #[serde(default)]
    client_paths: Vec<PathBuf>,
    #[serde(default)]
    shared_paths: Vec<PathBuf>,
    #[serde(default)]
    public_paths: Vec<PathBuf>,
    #[serde(default)]
    bootstrap_bin: Option<String>,
    #[serde(default)]
    hmr_signal_path: Option<PathBuf>,
}

impl ProjectModel {
    pub(super) fn load() -> Result<Self> {
        let config_path = PathBuf::from("krab.toml");
        if !config_path.exists() {
            return Ok(Self::workspace_default());
        }

        let contents = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;
        if let Some(project) = parse_project_model(&contents)? {
            return Ok(project);
        }

        Ok(Self::workspace_default())
    }

    pub(super) fn has_client_build(&self) -> bool {
        self.client_package.is_some() && self.client_crate_dir.is_some()
    }

    pub(super) fn client_artifact_stem(&self) -> Option<&str> {
        self.client_artifact_stem
            .as_deref()
            .or(self.client_package.as_deref())
    }

    pub(super) fn watch_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for path in self
            .shared_paths
            .iter()
            .chain(self.server_paths.iter())
            .chain(self.client_paths.iter())
            .chain(self.public_paths.iter())
        {
            if !roots.contains(path) {
                roots.push(path.clone());
            }
        }
        roots
    }

    fn workspace_default() -> Self {
        let dist_dir = PathBuf::from("dist");
        Self {
            frontend_bin: "service_frontend".to_string(),
            client_package: Some("krab_client".to_string()),
            client_crate_dir: Some(PathBuf::from("crates/framework/krab_client")),
            client_artifact_stem: Some("krab_client".to_string()),
            public_dir: PathBuf::from("services/service_frontend/public"),
            dist_dir: dist_dir.clone(),
            server_paths: vec![PathBuf::from("services/service_frontend/src")],
            client_paths: vec![PathBuf::from("crates/framework/krab_client/src")],
            shared_paths: vec![
                PathBuf::from("crates/framework/krab_core/src"),
                PathBuf::from("crates/framework/krab_macros/src"),
            ],
            public_paths: vec![PathBuf::from("services/service_frontend/public")],
            bootstrap_bin: "krab_orchestrator".to_string(),
            hmr_signal_path: dist_dir.join(".hmr_signal"),
        }
    }

    fn from_project_section(project: ProjectSection) -> Self {
        let frontend_bin = project
            .frontend_bin
            .unwrap_or_else(|| "service_frontend".to_string());
        let public_dir = project
            .public_dir
            .unwrap_or_else(|| PathBuf::from("public"));
        let dist_dir = project.dist_dir.unwrap_or_else(|| PathBuf::from("dist"));
        let bootstrap_bin = project
            .bootstrap_bin
            .unwrap_or_else(|| "krab_orchestrator".to_string());
        let hmr_signal_path = project
            .hmr_signal_path
            .unwrap_or_else(|| dist_dir.join(".hmr_signal"));

        Self {
            frontend_bin,
            client_package: project.client_package,
            client_crate_dir: project.client_crate_dir,
            client_artifact_stem: project.client_artifact_stem,
            public_dir,
            dist_dir,
            server_paths: project.server_paths,
            client_paths: project.client_paths,
            shared_paths: project.shared_paths,
            public_paths: project.public_paths,
            bootstrap_bin,
            hmr_signal_path,
        }
    }
}

fn parse_project_model(contents: &str) -> Result<Option<ProjectModel>> {
    let parsed: KrabToml =
        toml::from_str(contents).context("Failed to parse krab.toml project configuration")?;
    Ok(parsed.project.map(ProjectModel::from_project_section))
}

#[cfg(test)]
mod tests {
    use super::{parse_project_model, ProjectModel};
    use std::path::PathBuf;

    #[test]
    fn parse_project_section_overrides_workspace_defaults() {
        let model = parse_project_model(
            r#"
[project]
frontend_bin = "demo_app"
public_dir = "public"
dist_dir = "build"
server_paths = ["src"]
public_paths = ["public"]
bootstrap_bin = "demo_app"
"#,
        )
        .expect("parse should succeed")
        .expect("project section should exist");

        assert_eq!(model.frontend_bin, "demo_app");
        assert_eq!(model.public_dir, PathBuf::from("public"));
        assert_eq!(model.dist_dir, PathBuf::from("build"));
        assert_eq!(model.server_paths, vec![PathBuf::from("src")]);
        assert_eq!(model.public_paths, vec![PathBuf::from("public")]);
        assert_eq!(model.bootstrap_bin, "demo_app");
        assert_eq!(model.hmr_signal_path, PathBuf::from("build/.hmr_signal"));
    }

    #[test]
    fn parse_project_section_can_disable_client_build() {
        let model = parse_project_model(
            r#"
[project]
frontend_bin = "demo_app"
server_paths = ["src"]
"#,
        )
        .expect("parse should succeed")
        .expect("project section should exist");

        assert!(!model.has_client_build());
        assert_eq!(model.watch_roots(), vec![PathBuf::from("src")]);
    }

    #[test]
    fn missing_project_section_returns_none() {
        let parsed = parse_project_model(
            r#"
[services.frontend]
command = "cargo"
args = ["run", "--bin", "service_frontend"]
"#,
        )
        .expect("parse should succeed");

        assert_eq!(parsed, None::<ProjectModel>);
    }
}
