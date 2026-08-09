//! Password credential verification.
//!
//! Available under the `auth` feature. Provides [`CredentialStore`] — the trait
//! an application implements to answer "is this username/password pair valid?" —
//! and [`EnvHashCredentialStore`], an implementation that reads Argon2id hashes
//! in [PHC string format] from the environment.
//!
//! # Why this exists
//!
//! `service_auth` previously compared passwords with `!=` against a plaintext
//! value read from a JSON map. That is two defects: the secret was stored in
//! the clear, and the comparison short-circuited on the first differing byte,
//! leaking the length of the shared prefix to anyone able to time the endpoint.
//!
//! Both are closed here, and the trait keeps the fix reusable rather than
//! local to one reference service.
//!
//! # Timing
//!
//! [`EnvHashCredentialStore::verify`] performs an Argon2 verification against a
//! fixed dummy hash when the username is unknown, so a missing user and a wrong
//! password cost the same. Without that, the endpoint enumerates valid
//! usernames regardless of how the password comparison is written.
//!
//! [PHC string format]: https://github.com/P-H-C/phc-string-format/blob/master/phc-sf-spec.md

use anyhow::{Context as _, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString};
use argon2::Argon2;
use async_trait::async_trait;
use std::collections::BTreeMap;

/// A verified principal.
///
/// Deliberately minimal: the claims a token carries (tenant, scopes, roles) are
/// the issuing service's concern, not the credential store's. Widening this is
/// a breaking change to every implementor, so it stays narrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The canonical username the credential resolved to.
    pub username: String,
}

/// Verifies a username and password.
///
/// Implementations MUST NOT distinguish "unknown user" from "wrong password" in
/// their return value or their timing — both are `Ok(None)`. Returning `Err` is
/// for genuine faults (an unreachable store, a malformed configuration), which
/// callers should surface as a 5xx rather than a 401.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// `Ok(Some(identity))` on success, `Ok(None)` when the credential is not
    /// valid, `Err` when verification could not be performed at all.
    async fn verify(&self, username: &str, password: &str) -> Result<Option<Identity>>;
}

/// A fixed Argon2id hash used to equalise timing for unknown usernames.
///
/// Hash of a value no user can present, generated with the same parameters
/// [`hash_password`] uses. Verification against it always fails; the point is
/// that it costs the same as a real verification.
const DUMMY_PHC_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$MDAwMDAwMDAwMDAwMDAwMA$pdaCoLR3xX/D3xQMKQPvvUM8YtOKLQP2VwvUqTcHmoU";

/// Hash a password with Argon2id at the crate's chosen parameters.
///
/// Returns a PHC string, which encodes the algorithm, version, parameters, and
/// salt alongside the digest — so a stored hash stays verifiable after these
/// defaults change.
///
/// Uses `Argon2::default()`: Argon2id, v19, m=19456 KiB, t=2, p=1 — the
/// [RFC 9106] second recommended configuration, and the `argon2` crate's
/// default.
///
/// [RFC 9106]: https://www.rfc-editor.org/rfc/rfc9106.html#section-4
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| anyhow::anyhow!("failed to hash password: {err}"))
}

/// Whether `value` is a PHC string this crate can actually verify against.
///
/// Used by startup guards to reject a plaintext password before it can reach a
/// production login path.
///
/// Parsing alone is not enough. `PasswordHash::new` accepts `$argon2id$nonsense`
/// — a well-formed PHC string carrying an algorithm and nothing else — so a
/// parse-only check would admit a credential with no digest, which passes
/// startup and then rejects every login attempt for that user. This requires
/// all three of: an Argon2 variant, a salt, and a digest.
pub fn is_valid_password_hash(value: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(value) else {
        return false;
    };

    let is_argon2 = matches!(
        parsed.algorithm.as_str(),
        "argon2d" | "argon2i" | "argon2id"
    );

    is_argon2 && parsed.salt.is_some() && parsed.hash.is_some()
}

/// Verify `password` against a PHC-format `hash`.
///
/// `Ok(false)` for a wrong password; `Err` only when `hash` is not a parseable
/// PHC string, which is a configuration fault rather than a failed login.
pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash)
        .map_err(|err| anyhow::anyhow!("stored credential is not a valid PHC hash: {err}"))?;

    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// A [`CredentialStore`] over an in-memory `username -> PHC hash` map.
///
/// Built from a JSON object, typically sourced through
/// [`read_env_or_file`](crate::config::read_env_or_file) so the hashes arrive
/// from a file or vault reference rather than an inline environment variable.
#[derive(Debug, Clone, Default)]
pub struct EnvHashCredentialStore {
    users: BTreeMap<String, String>,
}

impl EnvHashCredentialStore {
    /// Parse a JSON object of `username -> PHC hash`.
    ///
    /// Every value is validated at construction, so a plaintext password fails
    /// at startup rather than at the first login attempt. The error names the
    /// offending user but never prints the value.
    pub fn from_json(raw: &str) -> Result<Self> {
        let users: BTreeMap<String, String> = serde_json::from_str(raw)
            .context("credential map must be a JSON object of username -> PHC hash")?;

        for (username, hash) in &users {
            anyhow::ensure!(
                is_valid_password_hash(hash),
                "credential for user '{username}' is not a PHC-format password hash; \
                 store an Argon2id hash (see `krab auth hash-password`), not a plaintext password"
            );
        }

        Ok(Self { users })
    }

    /// Build from an explicit map, validating each hash as [`Self::from_json`]
    /// does.
    pub fn from_map(users: BTreeMap<String, String>) -> Result<Self> {
        for (username, hash) in &users {
            anyhow::ensure!(
                is_valid_password_hash(hash),
                "credential for user '{username}' is not a PHC-format password hash"
            );
        }

        Ok(Self { users })
    }

    /// Number of configured users.
    pub fn len(&self) -> usize {
        self.users.len()
    }

    /// Whether no users are configured.
    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }
}

#[async_trait]
impl CredentialStore for EnvHashCredentialStore {
    async fn verify(&self, username: &str, password: &str) -> Result<Option<Identity>> {
        match self.users.get(username) {
            Some(hash) => {
                if verify_password(password, hash)? {
                    Ok(Some(Identity {
                        username: username.to_string(),
                    }))
                } else {
                    Ok(None)
                }
            }
            None => {
                // Spend the same work as a real verification so a missing user
                // and a wrong password are indistinguishable by timing. The
                // result is discarded: this hash can never match.
                let _ = verify_password(password, DUMMY_PHC_HASH);
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_of(password: &str) -> String {
        hash_password(password).expect("hashing should succeed")
    }

    #[test]
    fn hash_password_emits_argon2id_phc_and_round_trips() {
        let hash = hash_of("correct horse battery staple");

        assert!(hash.starts_with("$argon2id$"), "unexpected format: {hash}");
        assert!(is_valid_password_hash(&hash));
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
    }

    #[test]
    fn hashing_the_same_password_twice_gives_different_hashes() {
        // Distinct salts; otherwise the map leaks which users share a password.
        assert_ne!(hash_of("same"), hash_of("same"));
    }

    #[test]
    fn wrong_password_does_not_verify() {
        let hash = hash_of("right");
        assert!(!verify_password("wrong", &hash).unwrap());
    }

    #[test]
    fn plaintext_is_not_a_valid_password_hash() {
        assert!(!is_valid_password_hash("change-me"));
        assert!(!is_valid_password_hash(""));
        assert!(!is_valid_password_hash("hunter2"));
    }

    /// `PasswordHash::new` accepts these — they are structurally valid PHC
    /// strings. They are still useless as credentials, and admitting one at
    /// startup means that user can never authenticate.
    #[test]
    fn structurally_valid_phc_without_a_digest_is_rejected() {
        assert!(!is_valid_password_hash("$argon2id$nonsense"));
        assert!(!is_valid_password_hash("$argon2id$v=19$m=19456,t=2,p=1"));
    }

    #[test]
    fn a_non_argon2_algorithm_is_rejected() {
        // Parseable, salted, and digested — but not something `Argon2` verifies.
        assert!(!is_valid_password_hash(
            "$pbkdf2-sha256$i=1000$c2FsdHNhbHQ$Fh5eLnV5b25l0kQwT0hFUkVIRVJFSEU"
        ));
    }

    #[test]
    fn verify_password_errors_on_a_non_phc_hash() {
        let err = verify_password("any", "change-me").unwrap_err().to_string();
        assert!(err.contains("not a valid PHC hash"), "unhelpful: {err}");
    }

    #[test]
    fn the_dummy_hash_is_parseable_so_unknown_user_timing_is_real_work() {
        // If this constant ever stopped parsing, the unknown-user branch would
        // return early instead of spending Argon2 time, silently reopening
        // username enumeration.
        assert!(is_valid_password_hash(DUMMY_PHC_HASH));
        assert!(!verify_password("anything at all", DUMMY_PHC_HASH).unwrap());
    }

    #[test]
    fn from_json_rejects_a_plaintext_password() {
        let err = EnvHashCredentialStore::from_json(r#"{"admin":"hunter2"}"#)
            .unwrap_err()
            .to_string();

        assert!(err.contains("admin"), "error should name the user: {err}");
        assert!(err.contains("PHC"), "error should name the format: {err}");
        assert!(
            !err.contains("hunter2"),
            "error must not echo the secret: {err}"
        );
    }

    #[test]
    fn from_json_rejects_malformed_json() {
        assert!(EnvHashCredentialStore::from_json("not json").is_err());
    }

    #[tokio::test]
    async fn correct_password_verifies_and_returns_the_identity() {
        let store =
            EnvHashCredentialStore::from_json(&format!(r#"{{"admin":"{}"}}"#, hash_of("s3cret")))
                .unwrap();

        let identity = store.verify("admin", "s3cret").await.unwrap();
        assert_eq!(
            identity,
            Some(Identity {
                username: "admin".to_string()
            })
        );
    }

    #[tokio::test]
    async fn wrong_password_rejects() {
        let store =
            EnvHashCredentialStore::from_json(&format!(r#"{{"admin":"{}"}}"#, hash_of("s3cret")))
                .unwrap();

        assert_eq!(store.verify("admin", "wrong").await.unwrap(), None);
    }

    #[tokio::test]
    async fn unknown_user_rejects_without_erroring() {
        let store =
            EnvHashCredentialStore::from_json(&format!(r#"{{"admin":"{}"}}"#, hash_of("s3cret")))
                .unwrap();

        assert_eq!(store.verify("nobody", "s3cret").await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_empty_store_rejects_everything() {
        let store = EnvHashCredentialStore::from_json("{}").unwrap();

        assert!(store.is_empty());
        assert_eq!(store.verify("admin", "anything").await.unwrap(), None);
    }
}
