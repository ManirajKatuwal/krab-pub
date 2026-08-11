//! Database support: driver selection, and the Postgres migration-governance
//! runtime.
//!
//! # Driver selection lives here, not in an application
//!
//! `KRAB_DB_DRIVER=postgres|sqlite` is presented as a framework-level choice,
//! but the enum that parsed it, the default connection strings, and the pool
//! wrapper all lived in `services/service_users` — a reference application. A
//! consumer depending on `krab_core` got Postgres and nothing else, and had to
//! reimplement driver selection or copy it out of an example. [`DbDriver`] and
//! [`resolve_db_driver`] are the framework's answer to that.
//!
//! # Feature layout
//!
//! | Feature | Enables |
//! |---|---|
//! | `db-postgres` | [`postgres`] — connection, migrations, drift, rollback, promotion policy |
//! | `db-sqlite` | `sqlx`'s SQLite driver, for applications building their own repositories |
//! | `db` | Deprecated alias for `db-postgres` |
//!
//! Driver selection is available whenever *either* driver feature is on, since
//! choosing between them is the point.
//!
//! [`postgres`]'s items are re-exported at this level, so `krab_core::db::connect`,
//! `krab_core::db::DbPool`, and the rest resolve unchanged under `db-postgres`.
//!
//! Migration governance — checksums, drift detection, rollback rehearsal — is
//! Postgres-only and stays that way. SQLite is supported as a development and
//! embedded target; it does not carry the same governance guarantees. See
//! [`docs/reference/database.md`](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/reference/database.md).

mod driver;
pub use driver::{default_db_url_for_driver, resolve_db_driver, DbDriver};

#[cfg(feature = "db-postgres")]
pub mod postgres;

// Flattened so every pre-existing `krab_core::db::*` path keeps resolving.
// Splitting the feature should not have been a breaking change for callers
// that only ever used Postgres.
#[cfg(feature = "db-postgres")]
pub use postgres::*;
