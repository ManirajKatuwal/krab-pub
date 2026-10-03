//! A minimal `.env` reader for `krab doctor` and `krab env-check`.
//!
//! Both commands evaluate the environment policy against process variables
//! only, so a fresh `krab new` project failed `krab doctor --strict` even after
//! the README's own first step, `cp .env.example .env`: the file the services
//! read at runtime was invisible to the tool that checks it. Loading `./.env`
//! before the policy runs makes the check describe the environment the project
//! will actually start with.
//!
//! Deliberately small and dependency-free rather than pulling in a dotenv
//! crate for two commands. The accepted syntax is the common subset:
//!
//! - `KEY=VALUE`, one per line, with an optional leading `export `
//! - blank lines and `#` comment lines
//! - `"double"` quoted values, with `\n`, `\t`, `\"` and `\\` escapes
//! - `'single'` quoted values, taken literally
//! - unquoted values, with a trailing ` # comment` removed and surrounding
//!   whitespace trimmed
//!
//! There is no `${VAR}` interpolation and no multi-line value. A line the
//! reader does not understand is reported and skipped, never guessed at.
//!
//! Variables already set in the process environment always win. An exported
//! value is a deliberate choice for this invocation; a file on disk is a
//! default. That is also the precedence the services themselves see when a
//! shell sources the file before starting them.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// One `KEY=VALUE` assignment read from a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DotenvEntry {
    pub(crate) key: String,
    pub(crate) value: String,
}

/// The result of parsing a whole file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ParsedDotenv {
    /// Assignments in file order, with later duplicates of a key replacing
    /// earlier ones — the result `source`-ing the file in a shell produces.
    pub(crate) entries: Vec<DotenvEntry>,
    /// One message per line that could not be parsed, naming the line.
    pub(crate) warnings: Vec<String>,
}

/// What loading a file into the process did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DotenvLoad {
    pub(crate) path: PathBuf,
    /// Variables the file set because the process did not already have them.
    pub(crate) applied: usize,
    /// Variables the file declared that the process environment already set.
    pub(crate) kept_from_environment: usize,
    pub(crate) warnings: Vec<String>,
}

fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// What may follow a closing quote: nothing, or whitespace and a comment.
fn is_trailing_comment_or_blank(rest: &str) -> bool {
    let rest = rest.trim_start();
    rest.is_empty() || rest.starts_with('#')
}

/// Parse the value half of an assignment. `Err` carries the reason.
fn parse_value(raw: &str) -> std::result::Result<String, &'static str> {
    let raw = raw.trim_start();

    if let Some(body) = raw.strip_prefix('"') {
        let mut value = String::new();
        let mut chars = body.char_indices();
        while let Some((index, c)) = chars.next() {
            match c {
                '"' => {
                    return if is_trailing_comment_or_blank(&body[index + 1..]) {
                        Ok(value)
                    } else {
                        Err("unexpected text after the closing double quote")
                    };
                }
                '\\' => match chars.next() {
                    Some((_, 'n')) => value.push('\n'),
                    Some((_, 't')) => value.push('\t'),
                    Some((_, 'r')) => value.push('\r'),
                    Some((_, '"')) => value.push('"'),
                    Some((_, '\\')) => value.push('\\'),
                    // An unknown escape is kept verbatim rather than rejected:
                    // Windows paths in double quotes are common, and dropping
                    // the backslash would silently change them.
                    Some((_, other)) => {
                        value.push('\\');
                        value.push(other);
                    }
                    None => return Err("unterminated double-quoted value"),
                },
                other => value.push(other),
            }
        }
        return Err("unterminated double-quoted value");
    }

    if let Some(body) = raw.strip_prefix('\'') {
        return match body.find('\'') {
            Some(end) if is_trailing_comment_or_blank(&body[end + 1..]) => {
                Ok(body[..end].to_string())
            }
            Some(_) => Err("unexpected text after the closing single quote"),
            None => Err("unterminated single-quoted value"),
        };
    }

    // Unquoted: a `#` starts a comment only after whitespace, so a value such
    // as `https://host/#fragment` or `abc#123` survives intact.
    let mut end = raw.len();
    let bytes = raw.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'#' && (index == 0 || bytes[index - 1].is_ascii_whitespace()) {
            end = index;
            break;
        }
    }
    Ok(raw[..end].trim_end().to_string())
}

/// Parse `.env` content. Never fails: malformed lines become warnings.
pub(crate) fn parse_dotenv(content: &str) -> ParsedDotenv {
    let mut parsed = ParsedDotenv::default();
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);

    for (index, line) in content.lines().enumerate() {
        let line_no = index + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let assignment = trimmed
            .strip_prefix("export")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map(str::trim_start)
            .unwrap_or(trimmed);

        let Some((key, raw_value)) = assignment.split_once('=') else {
            parsed
                .warnings
                .push(format!("line {line_no}: expected KEY=VALUE, skipped"));
            continue;
        };
        let key = key.trim();
        if !is_valid_key(key) {
            parsed.warnings.push(format!(
                "line {line_no}: `{key}` is not a valid variable name, skipped"
            ));
            continue;
        }

        match parse_value(raw_value) {
            Ok(value) => {
                parsed.entries.retain(|entry| entry.key != key);
                parsed.entries.push(DotenvEntry {
                    key: key.to_string(),
                    value,
                });
            }
            Err(reason) => parsed
                .warnings
                .push(format!("line {line_no}: {reason} for `{key}`, skipped")),
        }
    }

    parsed
}

/// Split parsed entries into the ones to apply and the count left to the
/// process environment. Pure, so the precedence is testable without mutating
/// process-global state.
pub(crate) fn entries_to_apply(
    entries: &[DotenvEntry],
    is_set: impl Fn(&str) -> bool,
) -> (Vec<&DotenvEntry>, usize) {
    let mut apply = Vec::new();
    let mut kept = 0usize;
    for entry in entries {
        if is_set(&entry.key) {
            kept += 1;
        } else {
            apply.push(entry);
        }
    }
    (apply, kept)
}

/// Load `path` into the process environment without overriding anything that
/// is already set. `Ok(None)` when the file does not exist.
///
/// Must run before anything reads the environment and while the process is
/// still single-threaded — which is the case at the top of the two commands
/// that call it.
pub(crate) fn load_dotenv_file(path: &Path) -> Result<Option<DotenvLoad>> {
    if !path.is_file() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let parsed = parse_dotenv(&content);

    let (apply, kept) = entries_to_apply(&parsed.entries, |key| std::env::var_os(key).is_some());
    for entry in &apply {
        std::env::set_var(&entry.key, &entry.value);
    }

    Ok(Some(DotenvLoad {
        path: path.to_path_buf(),
        applied: apply.len(),
        kept_from_environment: kept,
        warnings: parsed.warnings,
    }))
}

/// Load `./.env` for `krab doctor` / `krab env-check` and say what happened.
///
/// The summary goes to stderr so `--json` output on stdout stays parseable.
/// A file that exists but cannot be read is a warning, not a failure: the
/// check still runs, against the process environment alone, which is what it
/// did before this loader existed.
pub(crate) fn load_project_dotenv() {
    let path = Path::new(".env");
    match load_dotenv_file(path) {
        Ok(Some(load)) => {
            for warning in &load.warnings {
                eprintln!("warning: {}: {warning}", load.path.display());
            }
            let kept = if load.kept_from_environment > 0 {
                format!(
                    "; {} already set in the environment and left unchanged",
                    load.kept_from_environment
                )
            } else {
                String::new()
            };
            eprintln!(
                "Loaded {} variable(s) from {}{kept}",
                load.applied,
                load.path.display()
            );
        }
        Ok(None) => {}
        Err(err) => eprintln!("warning: {err:#}; checking the process environment only"),
    }
}

#[cfg(test)]
mod tests {
    use super::{entries_to_apply, load_dotenv_file, parse_dotenv, DotenvEntry};

    fn pairs(content: &str) -> Vec<(String, String)> {
        parse_dotenv(content)
            .entries
            .into_iter()
            .map(|entry| (entry.key, entry.value))
            .collect()
    }

    fn pair(key: &str, value: &str) -> (String, String) {
        (key.to_string(), value.to_string())
    }

    #[test]
    fn plain_assignments_comments_and_blank_lines() {
        let parsed = pairs("# header\n\nKRAB_ENVIRONMENT=dev\n  KRAB_PORT = 3000  \n# end\n");
        assert_eq!(
            parsed,
            vec![pair("KRAB_ENVIRONMENT", "dev"), pair("KRAB_PORT", "3000")]
        );
    }

    /// `.env.example` files written for shells often carry `export`.
    #[test]
    fn an_export_prefix_is_accepted() {
        assert_eq!(
            pairs("export KRAB_AUTH_MODE=static\nexport\tKRAB_HOST=0.0.0.0\n"),
            vec![
                pair("KRAB_AUTH_MODE", "static"),
                pair("KRAB_HOST", "0.0.0.0")
            ]
        );
        // A variable that merely starts with the word is not a prefix.
        assert_eq!(pairs("exported=1\n"), vec![pair("exported", "1")]);
    }

    #[test]
    fn double_quoted_values_honour_escapes_and_keep_hashes() {
        assert_eq!(
            pairs(r#"A="two words # not a comment" # a comment"#),
            vec![pair("A", "two words # not a comment")]
        );
        assert_eq!(
            pairs(r#"B="line\nbreak \"quoted\" back\\slash""#),
            vec![pair("B", "line\nbreak \"quoted\" back\\slash")]
        );
        // Unknown escapes survive, so a quoted Windows path is not mangled.
        assert_eq!(
            pairs(r#"C="C:\data\krab""#),
            vec![pair("C", r"C:\data\krab")]
        );
    }

    #[test]
    fn single_quoted_values_are_literal() {
        assert_eq!(
            pairs(r#"A='no \n escapes # here' # comment"#),
            vec![pair("A", r"no \n escapes # here")]
        );
    }

    /// No interpolation: `${HOME}` is text, not a lookup.
    #[test]
    fn values_are_never_interpolated() {
        assert_eq!(
            pairs("A=${HOME}/x\nB=\"$USER\"\n"),
            vec![pair("A", "${HOME}/x"), pair("B", "$USER")]
        );
    }

    #[test]
    fn an_unquoted_hash_starts_a_comment_only_after_whitespace() {
        assert_eq!(
            pairs("URL=https://host/#frag\nID=abc#123\nX=value # note\nEMPTY=\n"),
            vec![
                pair("URL", "https://host/#frag"),
                pair("ID", "abc#123"),
                pair("X", "value"),
                pair("EMPTY", ""),
            ]
        );
    }

    #[test]
    fn a_later_duplicate_replaces_an_earlier_one() {
        assert_eq!(
            pairs("A=1\nB=2\nA=3\n"),
            vec![pair("B", "2"), pair("A", "3")]
        );
    }

    #[test]
    fn crlf_line_endings_and_a_bom_are_tolerated() {
        assert_eq!(
            pairs("\u{feff}A=1\r\nB=\"2\"\r\n"),
            vec![pair("A", "1"), pair("B", "2")]
        );
    }

    /// Malformed lines are reported by line number and skipped; the rest of
    /// the file still loads.
    #[test]
    fn malformed_lines_become_warnings_without_losing_the_rest() {
        let parsed = parse_dotenv(
            "GOOD=1\nnot an assignment\n1BAD=x\nOPEN=\"unterminated\nTAIL='x' y\nALSO_GOOD=2\n",
        );

        assert_eq!(
            parsed
                .entries
                .iter()
                .map(|e| e.key.as_str())
                .collect::<Vec<_>>(),
            vec!["GOOD", "ALSO_GOOD"]
        );
        assert_eq!(parsed.warnings.len(), 4, "{:?}", parsed.warnings);
        assert!(
            parsed.warnings[0].starts_with("line 2:"),
            "{:?}",
            parsed.warnings
        );
        assert!(
            parsed.warnings[1].contains("`1BAD`"),
            "{:?}",
            parsed.warnings
        );
        assert!(
            parsed.warnings[2].contains("unterminated"),
            "{:?}",
            parsed.warnings
        );
        assert!(
            parsed.warnings[3].starts_with("line 5:"),
            "{:?}",
            parsed.warnings
        );
    }

    /// The process environment wins over the file, always.
    #[test]
    fn variables_already_set_are_not_overridden() {
        let entries = vec![
            DotenvEntry {
                key: "KRAB_AUTH_MODE".to_string(),
                value: "static".to_string(),
            },
            DotenvEntry {
                key: "KRAB_ENVIRONMENT".to_string(),
                value: "dev".to_string(),
            },
        ];

        let (apply, kept) = entries_to_apply(&entries, |key| key == "KRAB_AUTH_MODE");

        assert_eq!(kept, 1);
        assert_eq!(
            apply.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
            vec!["KRAB_ENVIRONMENT"]
        );
    }

    /// The same rule against the real process environment.
    #[test]
    #[serial_test::serial]
    fn loading_a_file_fills_gaps_but_never_overrides_the_process() {
        const SET: &str = "KRAB_CLI_DOTENV_TEST_ALREADY_SET";
        const UNSET: &str = "KRAB_CLI_DOTENV_TEST_UNSET";
        std::env::set_var(SET, "from-process");
        std::env::remove_var(UNSET);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".env");
        std::fs::write(&path, format!("{SET}=from-file\n{UNSET}=from-file\n")).expect("write");

        let load = load_dotenv_file(&path)
            .expect("load succeeds")
            .expect("file exists");

        let set_after = std::env::var(SET);
        let unset_after = std::env::var(UNSET);
        std::env::remove_var(SET);
        std::env::remove_var(UNSET);

        assert_eq!(load.applied, 1);
        assert_eq!(load.kept_from_environment, 1);
        assert_eq!(set_after.as_deref(), Ok("from-process"));
        assert_eq!(unset_after.as_deref(), Ok("from-file"));
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            load_dotenv_file(&dir.path().join(".env")).expect("no error"),
            None
        );
    }
}
