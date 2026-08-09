//! `krab auth` — operator tooling for the authentication service.

use crate::AuthAction;
use anyhow::{Context, Result};
use krab_core::credentials::hash_password;
use std::io::Read as _;

pub(super) fn dispatch_auth_action(action: &AuthAction) -> Result<()> {
    match action {
        AuthAction::HashPassword { password, username } => {
            run_hash_password(password.as_deref(), username.as_deref())
        }
    }
}

/// Hash a password and print the result.
///
/// Exists so an operator can produce the credential format the auth service
/// requires without reaching for a side tool. Before this, the only documented
/// credential format was plaintext, so there was nothing to produce.
fn run_hash_password(password: Option<&str>, username: Option<&str>) -> Result<()> {
    let password = match password {
        Some(value) => value.to_string(),
        None => read_password_from_stdin()?,
    };

    anyhow::ensure!(
        !password.trim().is_empty(),
        "refusing to hash an empty password"
    );

    let hash = hash_password(&password)?;

    match username {
        // A single-entry map, so the output can be pasted straight into
        // KRAB_AUTH_LOGIN_USERS_JSON or the file it is sourced from.
        Some(username) => println!("{}", serde_json::json!({ username: hash })),
        None => println!("{hash}"),
    }

    Ok(())
}

/// Read a password from stdin, dropping the trailing line ending a shell adds.
fn read_password_from_stdin() -> Result<String> {
    let mut buffer = String::new();
    std::io::stdin()
        .read_to_string(&mut buffer)
        .context("failed to read password from stdin")?;

    Ok(trim_one_line_ending(&buffer).to_string())
}

/// Strip a single trailing `\n` or `\r\n`.
///
/// Only the line ending, and only one: a trailing space can legitimately be
/// part of a password, and `trim_end` would drop it silently, producing a hash
/// that never verifies against what the operator typed.
fn trim_one_line_ending(value: &str) -> &str {
    match value.strip_suffix('\n') {
        Some(rest) => rest.strip_suffix('\r').unwrap_or(rest),
        None => value,
    }
}

#[cfg(test)]
mod tests {
    use super::trim_one_line_ending;
    use krab_core::credentials::{hash_password, is_valid_password_hash, verify_password};

    #[test]
    fn only_one_line_ending_is_trimmed_and_spaces_survive() {
        assert_eq!(trim_one_line_ending("pw\n"), "pw");
        assert_eq!(trim_one_line_ending("pw\r\n"), "pw");
        assert_eq!(trim_one_line_ending("pw"), "pw");
        // A trailing space is part of the password, not whitespace to strip.
        assert_eq!(trim_one_line_ending("pw \n"), "pw ");
        // Only the last line ending goes; a deliberate blank line stays.
        assert_eq!(trim_one_line_ending("pw\n\n"), "pw\n");
    }

    #[test]
    fn hashed_output_is_a_verifiable_phc_string() {
        let hash = hash_password("operator-supplied").unwrap();

        assert!(is_valid_password_hash(&hash));
        assert!(verify_password("operator-supplied", &hash).unwrap());
        assert!(!verify_password("something-else", &hash).unwrap());
    }

    #[test]
    fn username_form_emits_a_parseable_single_entry_map() {
        let hash = hash_password("pw").unwrap();
        let rendered = serde_json::json!({ "admin": hash }).to_string();

        let parsed: std::collections::BTreeMap<String, String> =
            serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(is_valid_password_hash(&parsed["admin"]));
    }
}
