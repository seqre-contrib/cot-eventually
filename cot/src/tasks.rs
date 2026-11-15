//! Background tasks.
//!
//! Background tasks are work that a project does outside of handling
//! requests. Currently, these are scheduled tasks: tasks that run at given
//! times, such as a cleanup every night or a check every few minutes. Apps
//! declare them in [`App::scheduled_tasks`](crate::App::scheduled_tasks), and a
//! running project runs each one when it is due.
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
//! the start.
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use derive_more::with_trait::Debug;
use jiff::Timestamp;
use jiff::civil::Time;
use jiff::tz::TimeZone;
use tracing::{error, warn};

use crate::ProjectContext;

/// How often a running project checks for tasks that are due.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

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
/// [`Schedule`]. See the [module documentation](self) for an example.
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
    /// Returning an error marks this run as failed. The error is logged, and
    /// a scheduled task still runs again at its next scheduled time.
    async fn run(&self, context: &TaskContext<'_>) -> crate::Result<()>;
}

/// The context a [`Task`] runs in.
#[derive(Debug)]
pub struct TaskContext<'a> {
    project: &'a ProjectContext,
    due: Timestamp,
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
        self.due
    }
}

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

/// A run of a task that is waiting to happen.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TaskRun {
    /// The name of the task, as returned by [`Task::name`].
    pub task: String,
    /// When the run is due.
    pub due: Timestamp,
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
        }
    }
}

/// Keeps the [`TaskRun`]s that are waiting to happen.
///
/// The backend of a project is set with
/// [`Project::task_backend`](crate::Project::task_backend). The default is
/// [`MemoryTaskBackend`].
#[async_trait]
pub trait TaskBackend: Send + Sync {
    /// Adds a run that should happen once it's due.
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
        self.runs().push(run);
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
}

impl TaskRunner {
    /// Creates a runner for the given scheduled tasks and adds the first run
    /// of each one, at its first scheduled time after the clock's current
    /// time.
    ///
    /// # Panics
    ///
    /// Panics if two scheduled tasks have the same name.
    pub(crate) async fn new(
        clock: Arc<dyn Clock>,
        backend: Arc<dyn TaskBackend>,
        scheduled_tasks: Vec<ScheduledTask>,
    ) -> crate::Result<Self> {
        let now = clock.now();
        let mut scheduled = HashMap::new();
        for scheduled_task in scheduled_tasks {
            let name = scheduled_task.task.name().to_owned();
            backend
                .enqueue(TaskRun::new(
                    name.clone(),
                    scheduled_task.schedule.next_after(now),
                ))
                .await?;
            let previous = scheduled.insert(name, scheduled_task);
            if let Some(previous) = previous {
                panic!(
                    "scheduled task `{}` is registered more than once",
                    previous.task.name()
                );
            }
        }

        Ok(Self {
            clock,
            backend,
            scheduled,
        })
    }

    /// Creates a runner without any tasks.
    #[cfg(feature = "test")]
    pub(crate) fn empty() -> Self {
        Self {
            clock: Arc::new(SystemClock),
            backend: Arc::new(MemoryTaskBackend::new()),
            scheduled: HashMap::new(),
        }
    }

    /// Returns the clock the runner checks due times against.
    #[must_use]
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Runs the tasks that are due at the clock's current time, one after
    /// another, and returns how many runs there were.
    ///
    /// After running a scheduled task, its next run is added at its next
    /// scheduled time after the current time, so runs that were missed while
    /// the project wasn't running, or while the clock jumped ahead, happen
    /// only once. A task that fails is logged and doesn't stop the others.
    ///
    /// # Errors
    ///
    /// Returns an error if the runs could not be retrieved from or added to
    /// the task backend.
    pub async fn run_due(&self, context: &ProjectContext) -> crate::Result<usize> {
        let now = self.clock.now();
        let due = self.backend.take_due(now).await?;

        for run in &due {
            let Some(scheduled_task) = self.scheduled.get(&run.task) else {
                warn!(task = run.task, "skipping a run of an unknown task");
                continue;
            };

            let task_context = TaskContext {
                project: context,
                due: run.due,
            };
            if let Err(error) = scheduled_task.task.run(&task_context).await {
                error!(task = run.task, %error, "task failed");
            }

            self.backend
                .enqueue(TaskRun::new(
                    run.task.clone(),
                    scheduled_task.schedule.next_after(now),
                ))
                .await?;
        }

        Ok(due.len())
    }
}

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
