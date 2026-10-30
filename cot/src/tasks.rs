//! Background tasks.
//!
//! Background tasks are work that a project does outside of handling
//! requests. There are two kinds:
//!
//! - **Scheduled tasks** run at given times, such as a cleanup every night or a
//!   check every few minutes. Apps declare them in
//!   [`App::scheduled_tasks`](crate::App::scheduled_tasks), and a running
//!   project runs each one when it is due.
//! - **Queued tasks** run once each time they are queued, with the data they
//!   are queued with, such as sending an email after a sign-up. Apps declare
//!   them in [`App::queued_tasks`](crate::App::queued_tasks) and queue runs
//!   with [`TaskRunner::queue`]. A run that fails is tried again later, a few
//!   times.
//!
//! Whether a task is due is checked against the project's [`Clock`], which
//! the project can replace through [`Project::clock`](crate::Project::clock).
//! Tests can use a clock they control to run scheduled tasks without waiting
//! for them, with
//! [`Client::run_due_tasks`](crate::test::Client::run_due_tasks).
//!
//! The runs that are waiting to happen are kept by a [`TaskBackend`]. The
//! default one, [`MemoryTaskBackend`], keeps them in memory, so after a
//! restart, each scheduled task next runs at its first scheduled time after
//! the start, and queued runs that were waiting are lost. The database
//! backend in [`db`] keeps them in the project's database instead, and can
//! queue a run in the same transaction as the change that causes it.
//!
//! # Examples
//!
//! ```
//! use cot::App;
//! use cot::tasks::{Schedule, ScheduledTask, Task, TaskContext};
//! use jiff::civil::time;
//! use jiff::tz::TimeZone;
//!
//! struct Cleanup;
//!
//! #[async_trait::async_trait]
//! impl Task for Cleanup {
//!     fn name(&self) -> &str {
//!         "cleanup"
//!     }
//!
//!     async fn run(&self, context: &TaskContext<'_>) -> cot::Result<()> {
//!         println!("cleaning up, due at {}", context.due());
//!         Ok(())
//!     }
//! }
//!
//! struct MyApp;
//!
//! impl App for MyApp {
//!     fn name(&self) -> &str {
//!         "my_app"
//!     }
//!
//!     fn scheduled_tasks(&self) -> Vec<ScheduledTask> {
//!         vec![ScheduledTask::new(
//!             Schedule::daily(time(3, 0, 0, 0), TimeZone::UTC),
//!             Cleanup,
//!         )]
//!     }
//! }
//! ```
//!
//! A queued task reads the data it was queued with from its context:
//!
//! ```
//! use cot::App;
//! use cot::tasks::{QueuedTask, Task, TaskContext, TaskRun};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Welcome {
//!     email: String,
//! }
//!
//! struct SendWelcome;
//!
//! #[async_trait::async_trait]
//! impl Task for SendWelcome {
//!     fn name(&self) -> &str {
//!         "my_app::send_welcome"
//!     }
//!
//!     async fn run(&self, context: &TaskContext<'_>) -> cot::Result<()> {
//!         let welcome: Welcome = context.payload()?;
//!         println!("welcoming {}", welcome.email);
//!         Ok(())
//!     }
//! }
//!
//! struct MyApp;
//!
//! impl App for MyApp {
//!     fn name(&self) -> &str {
//!         "my_app"
//!     }
//!
//!     fn queued_tasks(&self) -> Vec<QueuedTask> {
//!         vec![QueuedTask::new(SendWelcome)]
//!     }
//! }
//!
//! // in a request handler, after a sign-up:
//! # async fn sign_up(context: &cot::ProjectContext) -> cot::Result<()> {
//! let now = context.tasks().clock().now();
//! let run = TaskRun::new("my_app::send_welcome", now).with_payload(&Welcome {
//!     email: "ann@example.com".to_owned(),
//! })?;
//! context.tasks().queue(run).await?;
//! # Ok(())
//! # }
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use derive_more::with_trait::Debug;
use jiff::Timestamp;
use jiff::civil::Time;
use jiff::tz::TimeZone;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::{error, warn};

use crate::{Error, ProjectContext};

#[cfg(feature = "db")]
pub mod db;

/// How often a running project checks for tasks that are due.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How many times a queued run is tried, unless its task says otherwise.
const DEFAULT_MAX_ATTEMPTS: u32 = 5;

/// How long a queued run that failed waits before its first retry. Each
/// retry after that waits four times as long as the one before.
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(30);

/// A source of the current time.
///
/// Cot checks whether background tasks are due against the project's clock,
/// set with [`Project::clock`](crate::Project::clock). A project uses
/// [`SystemClock`] unless it sets a different one, for instance a clock that
/// tests can move forward.
///
/// # Examples
///
/// ```
/// use std::sync::Mutex;
///
/// use cot::tasks::Clock;
/// use jiff::Timestamp;
///
/// /// A clock that stays at the time it's set to.
/// struct FixedClock(Mutex<Timestamp>);
///
/// impl Clock for FixedClock {
///     fn now(&self) -> Timestamp {
///         *self.0.lock().unwrap()
///     }
/// }
/// ```
pub trait Clock: Send + Sync {
    /// Returns the current time.
    fn now(&self) -> Timestamp;
}

/// A [`Clock`] that returns the system time.
///
/// # Examples
///
/// ```
/// use cot::tasks::{Clock, SystemClock};
///
/// let now = SystemClock.now();
/// ```
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A background task.
///
/// To run a task at given times, return it from
/// [`App::scheduled_tasks`](crate::App::scheduled_tasks) together with a
/// [`Schedule`]. To run it each time it is queued, return it from
/// [`App::queued_tasks`](crate::App::queued_tasks). See the [module
/// documentation](self) for examples.
#[async_trait]
pub trait Task: Send + Sync {
    /// Returns the name of the task.
    ///
    /// The name identifies the task, so it must be unique within the
    /// project. A good choice is a name that starts with the name of the app.
    fn name(&self) -> &str;

    /// Runs the task.
    ///
    /// # Errors
    ///
    /// Returning an error marks this run as failed. The error is logged. A
    /// scheduled task still runs again at its next scheduled time, and a
    /// queued run is tried again later until it has been tried as many times
    /// as its [`QueuedTask`] allows.
    async fn run(&self, context: &TaskContext<'_>) -> crate::Result<()>;
}

/// The context a [`Task`] runs in.
#[derive(Debug)]
pub struct TaskContext<'a> {
    project: &'a ProjectContext,
    run: &'a TaskRun,
}

impl<'a> TaskContext<'a> {
    /// Returns the context of the project, which gives access to its
    /// configuration, database and so on.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::{Task, TaskContext};
    ///
    /// struct PrintConfig;
    ///
    /// #[async_trait::async_trait]
    /// impl Task for PrintConfig {
    ///     fn name(&self) -> &str {
    ///         "print_config"
    ///     }
    ///
    ///     async fn run(&self, context: &TaskContext<'_>) -> cot::Result<()> {
    ///         println!("debug mode: {}", context.project().config().debug);
    ///         Ok(())
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn project(&self) -> &'a ProjectContext {
        self.project
    }

    /// Returns the time the run was due at. The task usually runs a little
    /// later than that.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::{Task, TaskContext};
    ///
    /// struct PrintDue;
    ///
    /// #[async_trait::async_trait]
    /// impl Task for PrintDue {
    ///     fn name(&self) -> &str {
    ///         "print_due"
    ///     }
    ///
    ///     async fn run(&self, context: &TaskContext<'_>) -> cot::Result<()> {
    ///         println!("due at {}", context.due());
    ///         Ok(())
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn due(&self) -> Timestamp {
        self.run.due
    }

    /// Returns the data the run was queued with, set with
    /// [`TaskRun::with_payload`].
    ///
    /// # Errors
    ///
    /// Returns an error if the run has no data, as a scheduled run doesn't,
    /// or if the data can't be read as a `T`.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::{Task, TaskContext};
    ///
    /// struct Greet;
    ///
    /// #[async_trait::async_trait]
    /// impl Task for Greet {
    ///     fn name(&self) -> &str {
    ///         "greet"
    ///     }
    ///
    ///     async fn run(&self, context: &TaskContext<'_>) -> cot::Result<()> {
    ///         let name: String = context.payload()?;
    ///         println!("hello, {name}");
    ///         Ok(())
    ///     }
    /// }
    /// ```
    pub fn payload<T: DeserializeOwned>(&self) -> crate::Result<T> {
        let payload = self
            .run
            .payload
            .as_deref()
            .ok_or_else(|| Error::internal(NoPayload(self.run.task.clone())))?;
        serde_json::from_str(payload).map_err(Error::internal)
    }

    /// Returns how many times the run was tried before this time: 0 the
    /// first time.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::{Task, TaskContext};
    ///
    /// struct Flaky;
    ///
    /// #[async_trait::async_trait]
    /// impl Task for Flaky {
    ///     fn name(&self) -> &str {
    ///         "flaky"
    ///     }
    ///
    ///     async fn run(&self, context: &TaskContext<'_>) -> cot::Result<()> {
    ///         if context.attempt() > 0 {
    ///             println!("trying again");
    ///         }
    ///         Ok(())
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn attempt(&self) -> u32 {
        self.run.attempt
    }
}

/// The error of [`TaskContext::payload`] for a run without data.
#[derive(Debug, thiserror::Error)]
#[error("the run of task `{0}` has no payload")]
struct NoPayload(String);

/// When a scheduled task runs.
///
/// # Examples
///
/// ```
/// use cot::tasks::Schedule;
/// use jiff::civil::time;
/// use jiff::tz::TimeZone;
///
/// // every day at 03:00 UTC
/// let schedule = Schedule::daily(time(3, 0, 0, 0), TimeZone::UTC);
/// ```
#[derive(Debug, Clone)]
pub struct Schedule(ScheduleKind);

#[derive(Debug, Clone)]
enum ScheduleKind {
    Daily { time: Time, time_zone: TimeZone },
    Every(Duration),
}

impl Schedule {
    /// Runs every day at the given time of day in the given time zone.
    ///
    /// On a day when that time doesn't exist, because the clocks are moved
    /// forward over it, the task runs when the time is reached after the
    /// change instead (for instance, at 03:30 instead of 02:30). On a day
    /// when it happens twice, the task runs at the first one.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::Schedule;
    /// use jiff::civil::time;
    /// use jiff::tz::TimeZone;
    ///
    /// let schedule = Schedule::daily(time(3, 0, 0, 0), TimeZone::get("Europe/Warsaw")?);
    /// # Ok::<(), jiff::Error>(())
    /// ```
    #[must_use]
    pub fn daily(time: Time, time_zone: TimeZone) -> Self {
        Self(ScheduleKind::Daily { time, time_zone })
    }

    /// Runs at a fixed interval, at every multiple of `interval` since the
    /// Unix epoch.
    ///
    /// Runs line up with the interval, not with when the project started: a
    /// task that runs every 5 minutes runs at 12:00, 12:05 and so on.
    ///
    /// # Panics
    ///
    /// Panics if `interval` is shorter than a millisecond, or longer than
    /// the range of [`Timestamp`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use cot::tasks::Schedule;
    /// use jiff::Timestamp;
    ///
    /// let schedule = Schedule::every(Duration::from_mins(5));
    /// let after: Timestamp = "2026-10-25T12:03:00Z".parse()?;
    ///
    /// assert_eq!(
    ///     schedule.next_after(after),
    ///     "2026-10-25T12:05:00Z".parse::<Timestamp>()?
    /// );
    /// # Ok::<(), jiff::Error>(())
    /// ```
    #[must_use]
    pub fn every(interval: Duration) -> Self {
        assert!(
            interval >= Duration::from_millis(1),
            "the interval of a schedule should be at least a millisecond"
        );
        Self(ScheduleKind::Every(interval))
    }

    /// Returns the first time after `after` when the schedule is due.
    ///
    /// # Panics
    ///
    /// Panics if that time is outside the range of [`Timestamp`], which can
    /// only happen within a day, or an interval, of its end.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::Schedule;
    /// use jiff::Timestamp;
    /// use jiff::civil::time;
    /// use jiff::tz::TimeZone;
    ///
    /// let schedule = Schedule::daily(time(3, 0, 0, 0), TimeZone::UTC);
    /// let after: Timestamp = "2026-10-25T12:00:00Z".parse()?;
    ///
    /// assert_eq!(
    ///     schedule.next_after(after),
    ///     "2026-10-26T03:00:00Z".parse::<Timestamp>()?
    /// );
    /// # Ok::<(), jiff::Error>(())
    /// ```
    #[must_use]
    pub fn next_after(&self, after: Timestamp) -> Timestamp {
        const OUT_OF_RANGE: &str = "the next scheduled time should be within the supported range";

        match &self.0 {
            ScheduleKind::Daily { time, time_zone } => {
                let mut date = after.to_zoned(time_zone.clone()).date();
                loop {
                    let next = date
                        .to_datetime(*time)
                        .to_zoned(time_zone.clone())
                        .expect(OUT_OF_RANGE)
                        .timestamp();
                    if next > after {
                        return next;
                    }
                    date = date.tomorrow().expect(OUT_OF_RANGE);
                }
            }
            ScheduleKind::Every(interval) => {
                let interval = i64::try_from(interval.as_millis()).expect(OUT_OF_RANGE);
                let next = (after.as_millisecond().div_euclid(interval) + 1)
                    .checked_mul(interval)
                    .expect(OUT_OF_RANGE);
                Timestamp::from_millisecond(next).expect(OUT_OF_RANGE)
            }
        }
    }
}

/// A [`Task`] together with the [`Schedule`] it runs on.
///
/// See the [module documentation](self) for an example.
#[derive(Debug, Clone)]
pub struct ScheduledTask {
    schedule: Schedule,
    #[debug("{}", task.name())]
    task: Arc<dyn Task>,
}

impl ScheduledTask {
    /// The next run of the task, at its first scheduled time after `now`.
    fn run_after(&self, now: Timestamp) -> TaskRun {
        let name = self.task.name();
        TaskRun::new(name, self.schedule.next_after(now)).with_key(name)
    }

    /// Creates a task that runs on the given schedule.
    ///
    /// See the [module documentation](self) for an example.
    #[must_use]
    pub fn new<T: Task + 'static>(schedule: Schedule, task: T) -> Self {
        Self {
            schedule,
            task: Arc::new(task),
        }
    }

    /// Returns the schedule of the task.
    #[must_use]
    pub fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    /// Returns the task.
    #[must_use]
    pub fn task(&self) -> &Arc<dyn Task> {
        &self.task
    }
}

/// A [`Task`] that runs each time it is queued, with how many times a run is
/// tried.
///
/// A run that fails is tried again after a while: 30 seconds after the first
/// try, then each time four times longer than before. See the [module
/// documentation](self) for an example.
#[derive(Debug, Clone)]
pub struct QueuedTask {
    #[debug("{}", task.name())]
    task: Arc<dyn Task>,
    max_attempts: u32,
}

impl QueuedTask {
    /// Creates a queued task whose runs are tried up to 5 times.
    ///
    /// See the [module documentation](self) for an example.
    #[must_use]
    pub fn new<T: Task + 'static>(task: T) -> Self {
        Self {
            task: Arc::new(task),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }

    /// Sets how many times a run is tried before giving up on it; 1 for no
    /// retries.
    ///
    /// # Panics
    ///
    /// Panics if `max_attempts` is 0.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::{QueuedTask, Task, TaskContext};
    ///
    /// struct Notify;
    ///
    /// #[async_trait::async_trait]
    /// impl Task for Notify {
    ///     fn name(&self) -> &str {
    ///         "notify"
    ///     }
    ///
    ///     async fn run(&self, _context: &TaskContext<'_>) -> cot::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let task = QueuedTask::new(Notify).max_attempts(10);
    /// assert_eq!(task.attempts(), 10);
    /// ```
    #[must_use]
    pub fn max_attempts(mut self, max_attempts: u32) -> Self {
        assert!(max_attempts > 0, "a run should be tried at least once");
        self.max_attempts = max_attempts;
        self
    }

    /// Returns how many times a run is tried before giving up on it.
    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.max_attempts
    }

    /// Returns the task.
    #[must_use]
    pub fn task(&self) -> &Arc<dyn Task> {
        &self.task
    }
}

/// How long a queued run waits after it failed for the `attempt`-th time,
/// counting from 1.
fn retry_delay(attempt: u32) -> Duration {
    FIRST_RETRY_DELAY.saturating_mul(4_u32.saturating_pow(attempt.saturating_sub(1)))
}

/// A run of a task that is waiting to happen.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TaskRun {
    /// The name of the task, as returned by [`Task::name`].
    pub task: String,
    /// When the run is due.
    pub due: Timestamp,
    /// The data the run is for, as JSON; `None` for a scheduled run.
    pub payload: Option<String>,
    /// Identifies the run among those that are waiting: queuing a run with
    /// the key of one that is waiting replaces it. Scheduled runs have their
    /// task's name as their key, so each scheduled task waits only once.
    pub key: Option<String>,
    /// How many times the run was tried before: 0 until it first fails.
    pub attempt: u32,
}

impl TaskRun {
    /// Creates a run of the task with the given name, due at the given time.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::TaskRun;
    /// use jiff::Timestamp;
    ///
    /// let run = TaskRun::new("cleanup", Timestamp::UNIX_EPOCH);
    /// assert_eq!(run.task, "cleanup");
    /// ```
    #[must_use]
    pub fn new<T: Into<String>>(task: T, due: Timestamp) -> Self {
        Self {
            task: task.into(),
            due,
            payload: None,
            key: None,
            attempt: 0,
        }
    }

    /// Sets the data the run is for, which the task reads with
    /// [`TaskContext::payload`].
    ///
    /// # Errors
    ///
    /// Returns an error if `payload` can't be written as JSON.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::TaskRun;
    /// use jiff::Timestamp;
    ///
    /// let run = TaskRun::new("greet", Timestamp::UNIX_EPOCH).with_payload("Ann")?;
    /// assert_eq!(run.payload.as_deref(), Some(r#""Ann""#));
    /// # Ok::<(), cot::Error>(())
    /// ```
    pub fn with_payload<T: Serialize + ?Sized>(mut self, payload: &T) -> crate::Result<Self> {
        self.payload = Some(serde_json::to_string(payload).map_err(Error::internal)?);
        Ok(self)
    }

    /// Sets the key of the run: queuing it replaces a waiting run with the
    /// same key. Starting the key with the task's name keeps it apart from
    /// the keys of other tasks.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::TaskRun;
    /// use jiff::Timestamp;
    ///
    /// // a reminder for user 5 that replaces any earlier one still waiting
    /// let run = TaskRun::new("remind", Timestamp::UNIX_EPOCH).with_key("remind/5");
    /// assert_eq!(run.key.as_deref(), Some("remind/5"));
    /// ```
    #[must_use]
    pub fn with_key<T: Into<String>>(mut self, key: T) -> Self {
        self.key = Some(key.into());
        self
    }
}

/// Keeps the [`TaskRun`]s that are waiting to happen.
///
/// The backend of a project is set with
/// [`Project::task_backend`](crate::Project::task_backend). The default is
/// [`MemoryTaskBackend`].
#[async_trait]
pub trait TaskBackend: Send + Sync {
    /// Adds a run that should happen once it's due. If a run with the same
    /// [key](TaskRun::key) is waiting, the new run replaces it.
    ///
    /// # Errors
    ///
    /// Returns an error if the run could not be stored.
    async fn enqueue(&self, run: TaskRun) -> crate::Result<()>;

    /// Removes the runs that are due at `now` (the ones due at `now` or
    /// earlier) and returns them, earliest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the runs could not be retrieved.
    async fn take_due(&self, now: Timestamp) -> crate::Result<Vec<TaskRun>>;
}

/// A [`TaskBackend`] that keeps the runs in memory.
///
/// The runs are lost when the project stops.
///
/// # Examples
///
/// ```
/// use cot::tasks::{MemoryTaskBackend, TaskBackend, TaskRun};
/// use jiff::{Timestamp, ToSpan};
///
/// # #[tokio::main]
/// # async fn main() -> cot::Result<()> {
/// let backend = MemoryTaskBackend::new();
/// let now = Timestamp::UNIX_EPOCH;
/// backend
///     .enqueue(TaskRun::new("later", now + 1.hour()))
///     .await?;
/// backend.enqueue(TaskRun::new("now", now)).await?;
///
/// assert_eq!(backend.take_due(now).await?, [TaskRun::new("now", now)]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct MemoryTaskBackend {
    runs: Mutex<Vec<TaskRun>>,
}

impl MemoryTaskBackend {
    /// Creates an empty backend.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::MemoryTaskBackend;
    ///
    /// let backend = MemoryTaskBackend::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn runs(&self) -> std::sync::MutexGuard<'_, Vec<TaskRun>> {
        // a panic while holding the lock can't leave the list in an invalid
        // state, so the poison can be ignored
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl TaskBackend for MemoryTaskBackend {
    async fn enqueue(&self, run: TaskRun) -> crate::Result<()> {
        let mut runs = self.runs();
        if run.key.is_some() {
            runs.retain(|waiting| waiting.key != run.key);
        }
        runs.push(run);
        Ok(())
    }

    async fn take_due(&self, now: Timestamp) -> crate::Result<Vec<TaskRun>> {
        let mut runs = self.runs();
        let (mut due, waiting) = runs.drain(..).partition(|run: &TaskRun| run.due <= now);
        *runs = waiting;
        due.sort_by_key(|run: &TaskRun| run.due);
        Ok(due)
    }
}

/// Runs the background tasks of a project.
///
/// A running project uses it to run its tasks when they are due. It's
/// available through [`ProjectContext::tasks`].
#[derive(Debug)]
pub struct TaskRunner {
    #[debug("..")]
    clock: Arc<dyn Clock>,
    #[debug("..")]
    backend: Arc<dyn TaskBackend>,
    scheduled: HashMap<String, ScheduledTask>,
    queued: HashMap<String, QueuedTask>,
}

impl TaskRunner {
    /// Creates a runner for the given tasks and adds the first run of each
    /// scheduled one, at its first scheduled time after the clock's current
    /// time.
    ///
    /// # Panics
    ///
    /// Panics if two tasks have the same name.
    pub(crate) async fn new(
        clock: Arc<dyn Clock>,
        backend: Arc<dyn TaskBackend>,
        scheduled_tasks: Vec<ScheduledTask>,
        queued_tasks: Vec<QueuedTask>,
    ) -> crate::Result<Self> {
        let now = clock.now();
        let mut names = HashSet::new();
        let mut register = |name: &str| {
            assert!(
                names.insert(name.to_owned()),
                "task `{name}` is registered more than once"
            );
        };
        let mut scheduled = HashMap::new();
        for scheduled_task in scheduled_tasks {
            let name = scheduled_task.task.name().to_owned();
            register(&name);
            backend.enqueue(scheduled_task.run_after(now)).await?;
            scheduled.insert(name, scheduled_task);
        }
        let mut queued = HashMap::new();
        for queued_task in queued_tasks {
            let name = queued_task.task.name().to_owned();
            register(&name);
            queued.insert(name, queued_task);
        }

        Ok(Self {
            clock,
            backend,
            scheduled,
            queued,
        })
    }

    /// Creates a runner without any tasks.
    #[cfg(feature = "test")]
    pub(crate) fn empty() -> Self {
        Self {
            clock: Arc::new(SystemClock),
            backend: Arc::new(MemoryTaskBackend::new()),
            scheduled: HashMap::new(),
            queued: HashMap::new(),
        }
    }

    /// Returns the clock the runner checks due times against.
    #[must_use]
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Queues a run of a queued task, which happens once it's due. If a run
    /// with the same [key](TaskRun::key) is waiting, the new run replaces it.
    ///
    /// To queue a run in the same database transaction as the change that
    /// causes it, use [`db::queue`] instead.
    ///
    /// # Errors
    ///
    /// Returns an error if the task isn't one of the project's queued tasks,
    /// or the run could not be added to the task backend.
    ///
    /// # Examples
    ///
    /// ```
    /// use cot::tasks::TaskRun;
    ///
    /// # async fn sign_up(context: &cot::ProjectContext) -> cot::Result<()> {
    /// let now = context.tasks().clock().now();
    /// context
    ///     .tasks()
    ///     .queue(TaskRun::new("my_app::send_welcome", now))
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn queue(&self, run: TaskRun) -> crate::Result<()> {
        if !self.queued.contains_key(&run.task) {
            return Err(Error::internal(UnknownTask(run.task)));
        }
        self.backend.enqueue(run).await
    }

    /// Runs the tasks that are due at the clock's current time, one after
    /// another, and returns how many runs there were.
    ///
    /// After running a scheduled task, its next run is added at its next
    /// scheduled time after the current time, so runs that were missed while
    /// the project wasn't running, or while the clock jumped ahead, happen
    /// only once. A queued run that fails is added again to be tried later,
    /// until it has been tried as many times as its task allows. A task that
    /// fails is logged and doesn't stop the others.
    ///
    /// # Errors
    ///
    /// Returns an error if the runs could not be retrieved from or added to
    /// the task backend.
    pub async fn run_due(&self, context: &ProjectContext) -> crate::Result<usize> {
        let now = self.clock.now();
        let due = self.backend.take_due(now).await?;

        for run in &due {
            let task_context = TaskContext {
                project: context,
                run,
            };
            if let Some(scheduled_task) = self.scheduled.get(&run.task) {
                if let Err(error) = scheduled_task.task.run(&task_context).await {
                    error!(task = run.task, %error, "task failed");
                }
                self.backend.enqueue(scheduled_task.run_after(now)).await?;
            } else if let Some(queued_task) = self.queued.get(&run.task) {
                let Err(error) = queued_task.task.run(&task_context).await else {
                    continue;
                };
                let attempt = run.attempt + 1;
                if attempt >= queued_task.max_attempts {
                    error!(task = run.task, attempts = attempt, %error, "task failed, giving up");
                    continue;
                }
                let delay = retry_delay(attempt);
                warn!(task = run.task, attempt, %error, retry_in = ?delay, "task failed, will retry");
                let retry = TaskRun {
                    due: now.checked_add(delay).unwrap_or(Timestamp::MAX),
                    attempt,
                    ..run.clone()
                };
                self.backend.enqueue(retry).await?;
            } else {
                warn!(task = run.task, "skipping a run of an unknown task");
            }
        }

        Ok(due.len())
    }
}

/// The error of [`TaskRunner::queue`] for a task the project doesn't have.
#[derive(Debug, thiserror::Error)]
#[error("`{0}` is not a queued task of the project")]
struct UnknownTask(String);

/// Runs the tasks of a project in the background while it is running.
#[derive(Debug)]
pub(crate) struct Worker {
    stop: Arc<tokio::sync::Notify>,
    handle: tokio::task::JoinHandle<()>,
}

impl Worker {
    /// Starts checking for due tasks every [`POLL_INTERVAL`].
    pub(crate) fn start(context: Arc<ProjectContext>) -> Self {
        let stop = Arc::new(tokio::sync::Notify::new());
        let stop_requested = Arc::clone(&stop);

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = stop_requested.notified() => break,
                    () = tokio::time::sleep(POLL_INTERVAL) => {
                        if let Err(error) = context.tasks().run_due(&context).await {
                            error!(%error, "could not run the due tasks");
                        }
                    }
                }
            }
        });

        Self { stop, handle }
    }

    /// Stops the worker, after waiting for the tasks that are running to
    /// finish.
    pub(crate) async fn stop(self) {
        self.stop.notify_one();
        if let Err(error) = self.handle.await {
            error!(%error, "the task worker panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::{date, time};
    use jiff::{ToSpan, Zoned};

    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn daily_next_after() {
        let schedule = Schedule::daily(time(3, 0, 0, 0), TimeZone::UTC);

        assert_eq!(
            schedule.next_after(ts("2026-10-25T02:59:59Z")),
            ts("2026-10-25T03:00:00Z")
        );
        // strictly after: a run due now isn't due again right away
        assert_eq!(
            schedule.next_after(ts("2026-10-25T03:00:00Z")),
            ts("2026-10-26T03:00:00Z")
        );
        assert_eq!(
            schedule.next_after(ts("2026-12-31T12:00:00Z")),
            ts("2027-01-01T03:00:00Z")
        );
    }

    #[test]
    fn daily_next_after_in_time_zone() {
        let warsaw = TimeZone::get("Europe/Warsaw").unwrap();
        let schedule = Schedule::daily(time(3, 0, 0, 0), warsaw.clone());

        // 01:30 UTC is already 03:30 in Warsaw (CEST, UTC+2)
        let next = schedule.next_after(ts("2026-07-01T01:30:00Z"));
        assert_eq!(
            next.to_zoned(warsaw),
            date(2026, 7, 2)
                .at(3, 0, 0, 0)
                .in_tz("Europe/Warsaw")
                .unwrap()
        );
    }

    #[test]
    fn daily_next_after_skipped_time() {
        // on 2026-03-29, clocks in Warsaw go from 02:00 straight to 03:00
        let schedule = Schedule::daily(time(2, 30, 0, 0), TimeZone::get("Europe/Warsaw").unwrap());

        let next = schedule.next_after(ts("2026-03-28T12:00:00Z"));
        assert_eq!(
            next.to_zoned(TimeZone::get("Europe/Warsaw").unwrap()),
            "2026-03-29T03:30:00+02:00[Europe/Warsaw]"
                .parse::<Zoned>()
                .unwrap()
        );
    }

    #[test]
    fn every_next_after() {
        let schedule = Schedule::every(Duration::from_mins(5));

        assert_eq!(
            schedule.next_after(ts("2026-10-25T12:03:00Z")),
            ts("2026-10-25T12:05:00Z")
        );
        // strictly after: a run due now isn't due again right away
        assert_eq!(
            schedule.next_after(ts("2026-10-25T12:05:00Z")),
            ts("2026-10-25T12:10:00Z")
        );
        assert_eq!(
            schedule.next_after(ts("1969-12-31T23:58:00Z")),
            ts("1970-01-01T00:00:00Z")
        );
    }

    #[test]
    #[should_panic(expected = "at least a millisecond")]
    fn every_needs_an_interval() {
        let _ = Schedule::every(Duration::ZERO);
    }

    #[test]
    fn retries_wait_longer_each_time() {
        assert_eq!(retry_delay(1), Duration::from_secs(30));
        assert_eq!(retry_delay(2), Duration::from_mins(2));
        assert_eq!(retry_delay(3), Duration::from_mins(8));
        // no overflow, however many attempts
        assert!(retry_delay(u32::MAX) > Duration::from_hours(24 * 365));
    }

    #[cot::test]
    async fn memory_backend_replaces_a_run_with_the_same_key() {
        let backend = MemoryTaskBackend::new();
        let now = ts("2026-10-25T12:00:00Z");
        backend
            .enqueue(TaskRun::new("a", now).with_key("a/1"))
            .await
            .unwrap();
        backend.enqueue(TaskRun::new("a", now)).await.unwrap();
        let replacement = TaskRun::new("a", now - 1.minute()).with_key("a/1");
        backend.enqueue(replacement.clone()).await.unwrap();

        assert_eq!(
            backend.take_due(now).await.unwrap(),
            [replacement, TaskRun::new("a", now)]
        );
    }

    #[cot::test]
    async fn memory_backend_take_due() {
        let backend = MemoryTaskBackend::new();
        let now = ts("2026-10-25T12:00:00Z");
        backend
            .enqueue(TaskRun::new("b", now - 1.minute()))
            .await
            .unwrap();
        backend
            .enqueue(TaskRun::new("later", now + 1.minute()))
            .await
            .unwrap();
        backend
            .enqueue(TaskRun::new("a", now - 1.hour()))
            .await
            .unwrap();

        let due = backend.take_due(now).await.unwrap();
        assert_eq!(
            due,
            [
                TaskRun::new("a", now - 1.hour()),
                TaskRun::new("b", now - 1.minute())
            ]
        );
        assert_eq!(backend.take_due(now).await.unwrap(), []);
        assert_eq!(backend.take_due(now + 1.minute()).await.unwrap().len(), 1);
    }
}
