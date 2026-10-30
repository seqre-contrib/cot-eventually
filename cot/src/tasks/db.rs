//! A task backend that keeps the runs in the project's database.
//!
//! [`DatabaseTaskBackend`] keeps the runs that are waiting in a table of the
//! project's database, so they survive a restart, and [`queue`] adds a run
//! through any database connection, including a transaction: a change and the
//! work it causes are then committed or rolled back together. The table is
//! created by the migrations of [`TaskQueueApp`].
//!
//! # Examples
//!
//! ```
//! use std::sync::Arc;
//!
//! use cot::project::{RegisterAppsContext, TaskBackendContext};
//! use cot::tasks::TaskBackend;
//! use cot::tasks::db::{DatabaseTaskBackend, TaskQueueApp};
//! use cot::{AppBuilder, Project};
//!
//! struct MyProject;
//!
//! impl Project for MyProject {
//!     fn register_apps(&self, apps: &mut AppBuilder, _context: &RegisterAppsContext) {
//!         apps.register(TaskQueueApp::new());
//!     }
//!
//!     fn task_backend(&self, context: &TaskBackendContext) -> Arc<dyn TaskBackend> {
//!         Arc::new(DatabaseTaskBackend::new(context.database().clone()))
//!     }
//! }
//! ```

pub mod migrations;

use async_trait::async_trait;
use jiff::Timestamp;

use crate::App;
use crate::db::migrations::SyncDynMigration;
use crate::db::{Auto, Database, DatabaseBackend, Model, model, query};
use crate::tasks::{TaskBackend, TaskRun};

/// A run that is waiting, as stored in the database.
#[derive(Debug, Clone)]
#[model(table_name = "task_run")]
pub(crate) struct StoredRun {
    #[model(primary_key)]
    pub(crate) id: Auto<i64>,
    pub(crate) task: String,
    #[model(unique)]
    pub(crate) key: Option<String>,
    pub(crate) payload: Option<String>,
    /// When the run is due, in milliseconds since the Unix epoch.
    pub(crate) due: i64,
    pub(crate) attempt: u32,
}

impl StoredRun {
    fn from_run(run: TaskRun) -> Self {
        Self {
            id: Auto::auto(),
            task: run.task,
            key: run.key,
            payload: run.payload,
            due: run.due.as_millisecond(),
            attempt: run.attempt,
        }
    }

    fn into_run(self) -> TaskRun {
        TaskRun {
            task: self.task,
            // a stored time came from a `Timestamp`, so it's always in range
            due: Timestamp::from_millisecond(self.due).unwrap_or(Timestamp::MAX),
            payload: self.payload,
            key: self.key,
            attempt: self.attempt,
        }
    }
}

/// Adds a run through the database connection `db`, which can be a
/// transaction. If a run with the same [key](TaskRun::key) is waiting, the new
/// run replaces it.
///
/// Queuing in the transaction of a change makes the run part of it: the run
/// happens only if the change is committed. The project must use a
/// [`DatabaseTaskBackend`] on the same database, and the run's task must be
/// one of the project's queued tasks; a run of a task the project doesn't have
/// is logged and dropped when it's due.
///
/// # Errors
///
/// Returns an error if the run could not be stored.
///
/// # Examples
///
/// ```
/// use cot::db::Database;
/// use cot::tasks::{TaskRun, db};
///
/// # async fn sign_up(database: &Database) -> cot::Result<()> {
/// let mut transaction = database.begin().await?;
/// // ... create the account ...
/// let run = TaskRun::new("my_app::send_welcome", jiff::Timestamp::now())
///     .with_payload("ann@example.com")?;
/// db::queue(&mut transaction, run).await?;
/// transaction.commit().await?;
/// # Ok(())
/// # }
/// ```
pub async fn queue<DB: DatabaseBackend>(mut db: DB, run: TaskRun) -> crate::Result<()> {
    if let Some(key) = &run.key {
        let key = Some(key.clone());
        query!(StoredRun, $key == key).delete(&mut db).await?;
    }
    StoredRun::from_run(run).insert(&mut db).await?;
    Ok(())
}

/// A [`TaskBackend`] that keeps the runs in a table of the project's
/// database.
///
/// The table is created by the migrations of [`TaskQueueApp`], so the project
/// registers that app too. See the [module documentation](self) for an
/// example.
///
/// A run is removed from the table when it's taken to be run, so a run
/// interrupted by the project stopping doesn't happen again.
#[derive(Debug, Clone)]
pub struct DatabaseTaskBackend {
    database: Database,
}

impl DatabaseTaskBackend {
    /// Creates a backend that keeps the runs in `database`.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::db::Database;
    /// use cot::tasks::db::DatabaseTaskBackend;
    ///
    /// # #[tokio::main]
    /// # async fn main() -> cot::Result<()> {
    /// let backend = DatabaseTaskBackend::new(Database::new("sqlite::memory:").await?);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl TaskBackend for DatabaseTaskBackend {
    async fn enqueue(&self, run: TaskRun) -> crate::Result<()> {
        let mut transaction = self.database.begin().await?;
        queue(&mut transaction, run).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn take_due(&self, now: Timestamp) -> crate::Result<Vec<TaskRun>> {
        let now = now.as_millisecond();
        let mut transaction = self.database.begin().await?;
        let mut due = query!(StoredRun, $due <= now).all(&mut transaction).await?;
        query!(StoredRun, $due <= now)
            .delete(&mut transaction)
            .await?;
        transaction.commit().await?;

        due.sort_by_key(|run| (run.due, run.id));
        Ok(due.into_iter().map(StoredRun::into_run).collect())
    }
}

/// The app that creates the table of a [`DatabaseTaskBackend`].
///
/// See the [module documentation](self) for an example.
#[derive(Debug, Copy, Clone, Default)]
#[non_exhaustive]
pub struct TaskQueueApp;

impl TaskQueueApp {
    /// Creates the app.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::db::TaskQueueApp;
    ///
    /// let app = TaskQueueApp::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl App for TaskQueueApp {
    fn name(&self) -> &'static str {
        "cot_tasks"
    }

    fn migrations(&self) -> Vec<Box<SyncDynMigration>> {
        crate::db::migrations::wrap_migrations(migrations::MIGRATIONS)
    }
}
