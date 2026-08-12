//! `krab auth` — operator tooling for the authentication service.

use crate::AuthAction;
use anyhow::{Context, Result};
use krab_core::credentials::hash_password;
use std::io::{IsTerminal as _, Read as _};

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
    let mut stdin = std::io::stdin();

    if let Some(prompt) = stdin_prompt(stdin.is_terminal()) {
        eprintln!("{prompt}");
    }

    let mut buffer = String::new();
    stdin
        .read_to_string(&mut buffer)
        .context("failed to read password from stdin")?;

    Ok(trim_one_line_ending(&buffer).to_string())
}

/// The prompt to show before blocking on stdin, if one is warranted.
///
/// The read runs to EOF, so an interactive operator sees nothing at all until
/// they guess to send EOF — the command looks hung. A prompt fixes that, but
/// only when a human is there: it is split out from the read so the piped path
/// can be asserted to stay byte-identical, since `krab auth hash-password < pw`
/// inside a script must not start emitting new output. The caller writes it to
/// stderr, keeping stdout a clean hash for `> creds.txt` and pipelines.
fn stdin_prompt(is_terminal: bool) -> Option<String> {
    if !is_terminal {
        return None;
    }

    // Windows terminals end console input with Ctrl-Z + Enter, not Ctrl-D.
    let eof_keys = if cfg!(windows) {
        "Ctrl-Z then Enter"
    } else {
        "Ctrl-D"
    };
    Some(format!(
        "Enter password (input is not hidden), then press {eof_keys}:"
    ))
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
    use super::{stdin_prompt, trim_one_line_ending};
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

    /// Piped input must stay silent.
    ///
    /// `krab auth hash-password < pw.txt` runs unattended in scripts, so a
    /// prompt there would be new, unexpected output; the interactive case is the
    /// one that previously looked hung with no indication it wanted input.
    #[test]
    fn prompt_appears_only_for_interactive_stdin() {
        assert_eq!(stdin_prompt(false), None);

        let prompt = stdin_prompt(true).expect("a terminal operator gets a prompt");
        assert!(prompt.contains("password"));
        // Without the EOF key the prompt still leaves the operator stuck.
        let eof_keys = if cfg!(windows) { "Ctrl-Z" } else { "Ctrl-D" };
        assert!(prompt.contains(eof_keys), "prompt was: {prompt}");
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
