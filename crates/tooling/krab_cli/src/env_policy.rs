//! The environment policy shared by `krab env-check` and `krab doctor`.
//!
//! This rule set was implemented twice, near-verbatim: once in
//! `dev_workflow::validate_environment` and once in
//! `doctor::collect_environment_warnings`. Two copies meant two places to
//! drift, and neither copy was covered by a test — the policy could have
//! diverged silently in either direction. The rules now live here once; the two
//! callers keep their own presentation and exit behaviour and share nothing but
//! the verdict.
//!
//! Deliberately a *lightweight developer-workflow* check. The authoritative
//! validation still lives in `krab_core`'s configuration loading, which is what
//! actually refuses to start a service.

/// The environment variables the policy reads, captured up front.
///
/// Taking the inputs as data rather than reading `std::env` inside the rules is
/// what makes each branch testable without mutating process-global state (and
/// therefore without `#[serial]` on every test).
///
/// Presence, not emptiness, is what counts: the original implementations tested
/// `std::env::var(..).is_err()`, so `KRAB_OIDC_ISSUER=""` satisfied the rule.
/// `Option<String>` preserves that exactly.
#[derive(Debug, Default, Clone)]
pub(crate) struct EnvironmentInputs {
    pub(crate) auth_mode: Option<String>,
    pub(crate) oidc_issuer: Option<String>,
    pub(crate) oidc_audience: Option<String>,
    pub(crate) environment: Option<String>,
}

impl EnvironmentInputs {
    pub(crate) fn from_process_env() -> Self {
        Self {
            auth_mode: std::env::var("KRAB_AUTH_MODE").ok(),
            oidc_issuer: std::env::var("KRAB_OIDC_ISSUER").ok(),
            oidc_audience: std::env::var("KRAB_OIDC_AUDIENCE").ok(),
            environment: std::env::var("KRAB_ENVIRONMENT").ok(),
        }
    }
}

/// Both callers defaulted an unset `KRAB_AUTH_MODE` to `jwt` and an unset
/// `KRAB_ENVIRONMENT` to `dev`. Keeping the defaults here keeps them equal.
const DEFAULT_AUTH_MODE: &str = "jwt";
const DEFAULT_ENVIRONMENT: &str = "dev";
const KNOWN_ENVIRONMENTS: [&str; 4] = ["local", "dev", "staging", "prod"];

/// Evaluate the policy against the current process environment.
pub(crate) fn collect_environment_warnings() -> Vec<String> {
    evaluate_environment_policy(&EnvironmentInputs::from_process_env())
}

/// Evaluate the policy against explicit inputs.
///
/// The returned strings are the user-visible warning text and are matched by
/// eye in `krab env-check` output and by `krab doctor`'s `environment-policy`
/// check details; treat them as part of the CLI's surface.
pub(crate) fn evaluate_environment_policy(inputs: &EnvironmentInputs) -> Vec<String> {
    let mut warnings = Vec::new();

    let auth_mode = inputs.auth_mode.as_deref().unwrap_or(DEFAULT_AUTH_MODE);
    let environment = inputs.environment.as_deref().unwrap_or(DEFAULT_ENVIRONMENT);

    if auth_mode.eq_ignore_ascii_case("jwt") || auth_mode.eq_ignore_ascii_case("oidc") {
        if inputs.oidc_issuer.is_none() {
            warnings.push("KRAB_OIDC_ISSUER is required when KRAB_AUTH_MODE=jwt".to_string());
        }
        if inputs.oidc_audience.is_none() {
            warnings.push("KRAB_OIDC_AUDIENCE is required when KRAB_AUTH_MODE=jwt".to_string());
        }
    } else if auth_mode.eq_ignore_ascii_case("static") {
        if !environment.eq_ignore_ascii_case("local") && !environment.eq_ignore_ascii_case("dev") {
            warnings.push(
                "KRAB_AUTH_MODE=static is forbidden outside local/dev; use jwt or oidc".to_string(),
            );
        }
    } else {
        warnings.push(format!(
            "Unsupported KRAB_AUTH_MODE='{auth_mode}'; expected static|jwt|oidc"
        ));
    }

    // Case-sensitive, unlike the auth-mode comparisons above. That asymmetry is
    // inherited from both original implementations, so `KRAB_ENVIRONMENT=Prod`
    // warns while `KRAB_AUTH_MODE=JWT` does not. Preserved rather than
    // "fixed": changing it would turn a passing environment into a warning for
    // existing users, which is a behaviour change, not a refactor.
    if !KNOWN_ENVIRONMENTS.contains(&environment) {
        warnings.push(format!(
            "KRAB_ENVIRONMENT should be one of local|dev|staging|prod, found: {environment}"
        ));
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::{collect_environment_warnings, evaluate_environment_policy, EnvironmentInputs};

    fn inputs(auth_mode: Option<&str>, environment: Option<&str>) -> EnvironmentInputs {
        EnvironmentInputs {
            auth_mode: auth_mode.map(str::to_string),
            oidc_issuer: None,
            oidc_audience: None,
            environment: environment.map(str::to_string),
        }
    }

    /// The default posture — nothing set at all — is `jwt` in `dev`, which
    /// wants an issuer and an audience and warns about neither environment.
    #[test]
    fn unset_environment_defaults_to_jwt_in_dev() {
        let warnings = evaluate_environment_policy(&EnvironmentInputs::default());

        assert_eq!(
            warnings,
            vec![
                "KRAB_OIDC_ISSUER is required when KRAB_AUTH_MODE=jwt".to_string(),
                "KRAB_OIDC_AUDIENCE is required when KRAB_AUTH_MODE=jwt".to_string(),
            ]
        );
    }

    #[test]
    fn jwt_without_issuer_or_audience_warns_about_each_independently() {
        let mut only_audience_set = inputs(Some("jwt"), Some("dev"));
        only_audience_set.oidc_audience = Some("krab".to_string());
        assert_eq!(
            evaluate_environment_policy(&only_audience_set),
            vec!["KRAB_OIDC_ISSUER is required when KRAB_AUTH_MODE=jwt".to_string()]
        );

        let mut only_issuer_set = inputs(Some("jwt"), Some("dev"));
        only_issuer_set.oidc_issuer = Some("https://issuer.example".to_string());
        assert_eq!(
            evaluate_environment_policy(&only_issuer_set),
            vec!["KRAB_OIDC_AUDIENCE is required when KRAB_AUTH_MODE=jwt".to_string()]
        );
    }

    /// `oidc` follows the same rule as `jwt`, and the mode comparison is
    /// case-insensitive.
    #[test]
    fn oidc_is_treated_like_jwt_and_the_mode_match_ignores_case() {
        for mode in ["oidc", "OIDC", "JWT", "Jwt"] {
            let warnings = evaluate_environment_policy(&inputs(Some(mode), Some("prod")));
            assert_eq!(
                warnings,
                vec![
                    "KRAB_OIDC_ISSUER is required when KRAB_AUTH_MODE=jwt".to_string(),
                    "KRAB_OIDC_AUDIENCE is required when KRAB_AUTH_MODE=jwt".to_string(),
                ],
                "mode {mode} should be handled as an OIDC-style mode"
            );
        }
    }

    /// A fully configured JWT setup is the clean case: no warnings at all.
    #[test]
    fn a_complete_jwt_configuration_produces_no_warnings() {
        let complete = EnvironmentInputs {
            auth_mode: Some("jwt".to_string()),
            oidc_issuer: Some("https://issuer.example".to_string()),
            oidc_audience: Some("krab".to_string()),
            environment: Some("prod".to_string()),
        };

        assert!(
            evaluate_environment_policy(&complete).is_empty(),
            "{:?}",
            evaluate_environment_policy(&complete)
        );
    }

    /// The rule that actually protects a deployment: static credentials are a
    /// local-development affordance and must not reach staging or prod.
    #[test]
    fn static_auth_is_forbidden_outside_local_and_dev() {
        for environment in ["staging", "prod"] {
            let warnings = evaluate_environment_policy(&inputs(Some("static"), Some(environment)));
            assert_eq!(
                warnings,
                vec![
                    "KRAB_AUTH_MODE=static is forbidden outside local/dev; use jwt or oidc"
                        .to_string()
                ],
                "static must be rejected in {environment}"
            );
        }
    }

    #[test]
    fn static_auth_is_allowed_in_local_and_dev_including_an_unset_environment() {
        for environment in [Some("local"), Some("dev"), None] {
            assert!(
                evaluate_environment_policy(&inputs(Some("static"), environment)).is_empty(),
                "static should be permitted in {environment:?}"
            );
        }
    }

    /// The two rules disagree about case, and the disagreement is observable:
    /// `KRAB_ENVIRONMENT=DEV` satisfies the case-insensitive static-auth rule
    /// but not the case-sensitive environment-name rule, so exactly one
    /// warning comes out. Written down because the asymmetry looks like a bug
    /// until you know it is inherited (see the rule's own comment) — and
    /// because an over-eager first draft of this test asserted no warnings at
    /// all and was wrong.
    #[test]
    fn a_miscased_dev_passes_the_static_rule_but_not_the_environment_name_rule() {
        let warnings = evaluate_environment_policy(&inputs(Some("static"), Some("DEV")));

        assert_eq!(
            warnings,
            vec![
                "KRAB_ENVIRONMENT should be one of local|dev|staging|prod, found: DEV".to_string()
            ]
        );
    }

    #[test]
    fn an_unknown_auth_mode_is_reported_with_the_offending_value() {
        let warnings = evaluate_environment_policy(&inputs(Some("basic"), Some("dev")));

        assert_eq!(
            warnings,
            vec!["Unsupported KRAB_AUTH_MODE='basic'; expected static|jwt|oidc".to_string()]
        );
    }

    #[test]
    fn an_unknown_environment_is_reported_with_the_offending_value() {
        let warnings = evaluate_environment_policy(&inputs(Some("static"), Some("production")));

        // Both rules fire: `static` outside local/dev, and an environment name
        // that is not one of the four known ones.
        assert_eq!(
            warnings,
            vec![
                "KRAB_AUTH_MODE=static is forbidden outside local/dev; use jwt or oidc".to_string(),
                "KRAB_ENVIRONMENT should be one of local|dev|staging|prod, found: production"
                    .to_string(),
            ]
        );
    }

    /// The environment name check is case-sensitive; see the comment on the
    /// rule itself. Pinned so a future "cleanup" is a deliberate decision.
    #[test]
    fn the_environment_name_check_is_case_sensitive() {
        let warnings = evaluate_environment_policy(&EnvironmentInputs {
            auth_mode: Some("jwt".to_string()),
            oidc_issuer: Some("https://issuer.example".to_string()),
            oidc_audience: Some("krab".to_string()),
            environment: Some("Prod".to_string()),
        });

        assert_eq!(
            warnings,
            vec![
                "KRAB_ENVIRONMENT should be one of local|dev|staging|prod, found: Prod".to_string()
            ]
        );
    }

    /// `from_process_env` must feed the same rules the pure evaluator runs —
    /// this is the seam where a wiring mistake would reintroduce the drift the
    /// module exists to remove.
    #[test]
    #[serial_test::serial]
    fn the_process_env_reader_agrees_with_the_pure_evaluator() {
        let restore: Vec<(&str, Option<String>)> = [
            "KRAB_AUTH_MODE",
            "KRAB_OIDC_ISSUER",
            "KRAB_OIDC_AUDIENCE",
            "KRAB_ENVIRONMENT",
        ]
        .iter()
        .map(|name| (*name, std::env::var(name).ok()))
        .collect();

        for name in ["KRAB_OIDC_ISSUER", "KRAB_OIDC_AUDIENCE"] {
            std::env::remove_var(name);
        }
        std::env::set_var("KRAB_AUTH_MODE", "static");
        std::env::set_var("KRAB_ENVIRONMENT", "prod");

        assert_eq!(
            collect_environment_warnings(),
            evaluate_environment_policy(&inputs(Some("static"), Some("prod")))
        );

        for (name, value) in restore {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}
