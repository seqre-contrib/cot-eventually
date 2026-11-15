//! List of migrations for the current app.

pub mod m_0001_initial;
/// The list of migrations for current app.
pub const MIGRATIONS: &[&::cot::db::migrations::SyncDynMigration] = &[&m_0001_initial::Migration];
