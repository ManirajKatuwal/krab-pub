//! Driver selection from `KRAB_DB_DRIVER`.
//!
//! Driver-agnostic on purpose: available under either `db-postgres` or
//! `db-sqlite`, because the whole point is choosing between them. Compiling a
//! driver in is separate from selecting one at runtime — an application may
//! enable both features and pick per environment.

use anyhow::Result;

/// Which SQL backend an application talks to.
///
/// Parsed from `KRAB_DB_DRIVER`. This lived in `service_users`, a reference
/// application, so framework consumers had no way to reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbDriver {
    /// Postgres. Carries the full migration governance surface in
    /// [`crate::db::postgres`]: checksum validation, drift detection, rollback,
    /// and promotion policy.
    Postgres,
    /// SQLite. Suitable for development, tests, and embedded deployments.
    /// Migration governance does not apply.
    Sqlite,
}

impl DbDriver {
    /// The value accepted in `KRAB_DB_DRIVER`, and what telemetry reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Sqlite => "sqlite",
        }
    }

    /// Whether this driver supports the migration governance surface.
    ///
    /// Callers running `db lifecycle`, drift detection, or rollback rehearsal
    /// should consult this rather than matching on the variant, so adding a
    /// driver later does not silently opt it into governance it cannot honour.
    pub fn supports_migration_governance(self) -> bool {
        matches!(self, Self::Postgres)
    }

    /// Case- and whitespace-insensitive parse.
    pub fn parse(input: &str) -> Result<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "postgres" => Ok(Self::Postgres),
            "sqlite" => Ok(Self::Sqlite),
            other => anyhow::bail!(
                "unsupported KRAB_DB_DRIVER='{}'; supported values are postgres|sqlite. \
                 MySQL was removed deliberately — it pulls in `rsa`, which carries \
                 RUSTSEC-2023-0071 with no upstream fix",
                other
            ),
        }
    }
}

impl std::fmt::Display for DbDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Read `KRAB_DB_DRIVER`, defaulting to Postgres.
///
/// The default is Postgres, not SQLite: it is the driver with migration
/// governance, and a production service that silently fell back to a local
/// SQLite file because an environment variable was unset would be a far worse
/// failure than one that refuses to start. `service_users` defaulted to SQLite
/// because it is a demo; that is not the right default for a framework.
pub fn resolve_db_driver() -> Result<DbDriver> {
    match std::env::var("KRAB_DB_DRIVER") {
        Ok(raw) if !raw.trim().is_empty() => DbDriver::parse(&raw),
        _ => Ok(DbDriver::Postgres),
    }
}

/// Development-only default connection string for a driver.
///
/// Never appropriate outside `local`: the Postgres form has no password and the
/// SQLite form writes a file into the working directory. Production
/// configuration comes from `DATABASE_URL` via
/// [`read_env_or_file`](crate::config::read_env_or_file).
pub fn default_db_url_for_driver(driver: DbDriver) -> &'static str {
    match driver {
        DbDriver::Postgres => "postgres://postgres@localhost:5432/krab_users",
        DbDriver::Sqlite => "sqlite://krab.sqlite?mode=rwc",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn parse_accepts_both_drivers_case_insensitively() {
        assert_eq!(DbDriver::parse("postgres").unwrap(), DbDriver::Postgres);
        assert_eq!(DbDriver::parse("  SQLite \n").unwrap(), DbDriver::Sqlite);
    }

    #[test]
    fn parse_rejects_unknown_driver_and_names_the_supported_set() {
        let err = DbDriver::parse("mysql").unwrap_err().to_string();
        assert!(err.contains("postgres|sqlite"), "unhelpful error: {err}");
    }

    #[test]
    fn only_postgres_carries_migration_governance() {
        assert!(DbDriver::Postgres.supports_migration_governance());
        assert!(!DbDriver::Sqlite.supports_migration_governance());
    }

    /// An unset or blank `KRAB_DB_DRIVER` must not silently select the driver
    /// without migration governance.
    #[test]
    #[serial]
    fn unset_and_blank_driver_default_to_postgres() {
        std::env::remove_var("KRAB_DB_DRIVER");
        assert_eq!(resolve_db_driver().unwrap(), DbDriver::Postgres);

        std::env::set_var("KRAB_DB_DRIVER", "   ");
        assert_eq!(resolve_db_driver().unwrap(), DbDriver::Postgres);

        std::env::remove_var("KRAB_DB_DRIVER");
    }

    #[test]
    #[serial]
    fn explicit_driver_is_honoured() {
        std::env::set_var("KRAB_DB_DRIVER", "sqlite");
        assert_eq!(resolve_db_driver().unwrap(), DbDriver::Sqlite);
        std::env::remove_var("KRAB_DB_DRIVER");
    }

    #[test]
    fn default_urls_match_their_driver_scheme() {
        assert!(default_db_url_for_driver(DbDriver::Postgres).starts_with("postgres://"));
        assert!(default_db_url_for_driver(DbDriver::Sqlite).starts_with("sqlite://"));
    }
}
