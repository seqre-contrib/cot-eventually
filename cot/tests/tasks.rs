use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cot::config::ProjectConfig;
use cot::project::RegisterAppsContext;
use cot::request::{Request, RequestExt};
use cot::response::Response;
use cot::router::{Route, Router};
use cot::tasks::{Clock, QueuedTask, Schedule, ScheduledTask, Task, TaskContext, TaskRun};
use cot::test::Client;
use cot::{App, AppBuilder, Body, Project, StatusCode};
use jiff::civil::time;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};

#[derive(Debug)]
struct FakeClock(Mutex<Timestamp>);

impl FakeClock {
    fn set(&self, now: &str) {
        *self.0.lock().unwrap() = now.parse().unwrap();
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        *self.0.lock().unwrap()
    }
}

/// Records the due time of each of its runs, and fails if asked to.
struct RecordingTask {
    name: &'static str,
    runs: Arc<Mutex<Vec<Timestamp>>>,
    fail: bool,
}

#[async_trait]
impl Task for RecordingTask {
    fn name(&self) -> &str {
        self.name
    }

    async fn run(&self, context: &TaskContext<'_>) -> cot::Result<()> {
        self.runs.lock().unwrap().push(context.due());
        if self.fail {
            Err(cot::Error::internal("task failed"))
        } else {
            Ok(())
        }
    }
}

struct TasksApp {
    nightly: Arc<Mutex<Vec<Timestamp>>>,
    failing: Arc<Mutex<Vec<Timestamp>>>,
}

impl App for TasksApp {
    fn name(&self) -> &'static str {
        "tasks"
    }

    fn scheduled_tasks(&self) -> Vec<ScheduledTask> {
        let at_3 = Schedule::daily(time(3, 0, 0, 0), TimeZone::UTC);
        vec![
            ScheduledTask::new(
                at_3.clone(),
                RecordingTask {
                    name: "tasks::nightly",
                    runs: Arc::clone(&self.nightly),
                    fail: false,
                },
            ),
            ScheduledTask::new(
                at_3,
                RecordingTask {
                    name: "tasks::failing",
                    runs: Arc::clone(&self.failing),
                    fail: true,
                },
            ),
        ]
    }
}

struct TestProject {
    clock: Arc<FakeClock>,
    nightly: Arc<Mutex<Vec<Timestamp>>>,
    failing: Arc<Mutex<Vec<Timestamp>>>,
}

impl Project for TestProject {
    fn config(&self, _config_name: &str) -> cot::Result<ProjectConfig> {
        Ok(ProjectConfig::default())
    }

    fn register_apps(&self, apps: &mut AppBuilder, _context: &RegisterAppsContext) {
        apps.register(TasksApp {
            nightly: Arc::clone(&self.nightly),
            failing: Arc::clone(&self.failing),
        });
    }

    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }
}

#[cot::test]
#[cfg_attr(
    miri,
    ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
)]
async fn scheduled_tasks_run_when_the_clock_passes_their_time() {
    let clock = Arc::new(FakeClock(Mutex::new(
        "2026-10-25T12:00:00Z".parse().unwrap(),
    )));
    let nightly = Arc::new(Mutex::new(Vec::new()));
    let failing = Arc::new(Mutex::new(Vec::new()));
    let mut client = Client::new(TestProject {
        clock: Arc::clone(&clock),
        nightly: Arc::clone(&nightly),
        failing: Arc::clone(&failing),
    })
    .await;

    // not due yet
    assert_eq!(client.run_due_tasks().await.unwrap(), 0);
    clock.set("2026-10-26T02:59:59Z");
    assert_eq!(client.run_due_tasks().await.unwrap(), 0);

    // due: both run, and the failing one doesn't stop the other
    clock.set("2026-10-26T03:00:00Z");
    assert_eq!(client.run_due_tasks().await.unwrap(), 2);
    let first_run: Timestamp = "2026-10-26T03:00:00Z".parse().unwrap();
    assert_eq!(*nightly.lock().unwrap(), [first_run]);
    assert_eq!(*failing.lock().unwrap(), [first_run]);

    // once per day
    assert_eq!(client.run_due_tasks().await.unwrap(), 0);

    // after the clock jumps ahead by days, the missed runs happen once, and
    // the next ones are at the next scheduled time
    clock.set("2026-10-29T12:00:00Z");
    assert_eq!(client.run_due_tasks().await.unwrap(), 2);
    assert_eq!(
        *nightly.lock().unwrap(),
        [first_run, first_run + 24.hours()]
    );
    clock.set("2026-10-30T03:00:00Z");
    assert_eq!(client.run_due_tasks().await.unwrap(), 2);
    assert_eq!(nightly.lock().unwrap().len(), 3);
    assert_eq!(failing.lock().unwrap().len(), 3);
}

#[cot::test]
#[cfg_attr(
    miri,
    ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
)]
#[should_panic(expected = "task `tasks::nightly` is registered more than once")]
async fn duplicate_scheduled_task_names() {
    struct DuplicateApp;
    impl App for DuplicateApp {
        fn name(&self) -> &'static str {
            "duplicate"
        }

        fn scheduled_tasks(&self) -> Vec<ScheduledTask> {
            let schedule = Schedule::daily(time(3, 0, 0, 0), TimeZone::UTC);
            let task = || RecordingTask {
                name: "tasks::nightly",
                runs: Arc::default(),
                fail: false,
            };
            vec![
                ScheduledTask::new(schedule.clone(), task()),
                ScheduledTask::new(schedule, task()),
            ]
        }
    }

    struct DuplicateProject;
    impl Project for DuplicateProject {
        fn config(&self, _config_name: &str) -> cot::Result<ProjectConfig> {
            Ok(ProjectConfig::default())
        }

        fn register_apps(&self, apps: &mut AppBuilder, _context: &RegisterAppsContext) {
            apps.register(DuplicateApp);
        }
    }

    let _ = Client::new(DuplicateProject).await;
}

#[cot::test]
#[cfg_attr(
    miri,
    ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
)]
async fn running_project_runs_due_tasks() {
    use std::time::Duration;

    use cot::Bootstrapper;
    use cot::project::run_at_with_shutdown;

    let clock = Arc::new(FakeClock(Mutex::new(
        "2026-10-25T12:00:00Z".parse().unwrap(),
    )));
    let nightly = Arc::new(Mutex::new(Vec::new()));
    let project = TestProject {
        clock: Arc::clone(&clock),
        nightly: Arc::clone(&nightly),
        failing: Arc::default(),
    };
    let bootstrapper = Bootstrapper::new(project)
        .with_config(ProjectConfig::builder().register_panic_hook(false).build())
        .boot()
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (shutdown, shutdown_requested) = tokio::sync::oneshot::channel::<()>();

    let server = run_at_with_shutdown(bootstrapper, listener, async {
        let _ = shutdown_requested.await;
    });
    let check = async {
        clock.set("2026-10-26T03:00:00Z");
        for _ in 0..100 {
            if !nightly.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        shutdown.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(server, check);

    result.unwrap();
    assert_eq!(nightly.lock().unwrap().len(), 1);
}

/// Records the payload and attempt of each of its runs, and fails the first
/// `failures` of them.
struct QueuedRecorder {
    name: &'static str,
    runs: Arc<Mutex<Vec<(String, u32)>>>,
    failures: usize,
}

#[async_trait]
impl Task for QueuedRecorder {
    fn name(&self) -> &str {
        self.name
    }

    async fn run(&self, context: &TaskContext<'_>) -> cot::Result<()> {
        let payload: String = context.payload()?;
        let mut runs = self.runs.lock().unwrap();
        runs.push((payload, context.attempt()));
        if runs.len() <= self.failures {
            Err(cot::Error::internal("task failed"))
        } else {
            Ok(())
        }
    }
}

struct QueueApp {
    sends: Arc<Mutex<Vec<(String, u32)>>>,
    flaky: Arc<Mutex<Vec<(String, u32)>>>,
}

impl App for QueueApp {
    fn name(&self) -> &'static str {
        "queue"
    }

    fn router(&self) -> Router {
        Router::with_urls([Route::with_handler(
            "/queue/{task}/{due}/{payload}/{key}",
            queue_run,
        )])
    }

    fn queued_tasks(&self) -> Vec<QueuedTask> {
        vec![
            QueuedTask::new(QueuedRecorder {
                name: "queue::send",
                runs: Arc::clone(&self.sends),
                failures: 0,
            }),
            QueuedTask::new(QueuedRecorder {
                name: "queue::flaky",
                runs: Arc::clone(&self.flaky),
                failures: usize::MAX,
            })
            .max_attempts(3),
        ]
    }
}

struct QueueProject {
    clock: Arc<FakeClock>,
    sends: Arc<Mutex<Vec<(String, u32)>>>,
    flaky: Arc<Mutex<Vec<(String, u32)>>>,
}

impl Project for QueueProject {
    fn config(&self, _config_name: &str) -> cot::Result<ProjectConfig> {
        Ok(ProjectConfig::default())
    }

    fn register_apps(&self, apps: &mut AppBuilder, _context: &RegisterAppsContext) {
        apps.register_with_views(
            QueueApp {
                sends: Arc::clone(&self.sends),
                flaky: Arc::clone(&self.flaky),
            },
            "",
        );
    }

    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }
}

async fn queue_client() -> (
    Client,
    Arc<FakeClock>,
    Arc<Mutex<Vec<(String, u32)>>>,
    Arc<Mutex<Vec<(String, u32)>>>,
) {
    let clock = Arc::new(FakeClock(Mutex::new(
        "2026-10-25T12:00:00Z".parse().unwrap(),
    )));
    let sends = Arc::new(Mutex::new(Vec::new()));
    let flaky = Arc::new(Mutex::new(Vec::new()));
    let client = Client::new(QueueProject {
        clock: Arc::clone(&clock),
        sends: Arc::clone(&sends),
        flaky: Arc::clone(&flaky),
    })
    .await;
    (client, clock, sends, flaky)
}

/// Queues the run the path describes, the way a request handler would; a
/// key of `-` is none.
async fn queue_run(request: Request) -> cot::Result<Response> {
    let params = request.path_params();
    let param = |name: &str| params.get(name).unwrap().to_owned();
    let due: Timestamp = param("due").parse().map_err(cot::Error::internal)?;
    let mut run = TaskRun::new(param("task"), due).with_payload(&param("payload"))?;
    if param("key") != "-" {
        run = run.with_key(param("key"));
    }
    request.context().tasks().queue(run).await?;
    Ok(Response::new(Body::empty()))
}

/// Queues a run through a request, and returns the response status.
async fn queue(client: &mut Client, task: &str, due: &str, payload: &str, key: &str) -> StatusCode {
    client
        .get(&format!("/queue/{task}/{due}/{payload}/{key}"))
        .await
        .unwrap()
        .status()
}

#[cot::test]
#[cfg_attr(
    miri,
    ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
)]
async fn queued_tasks_run_once_with_their_payload() {
    let (mut client, clock, sends, _) = queue_client().await;
    queue(
        &mut client,
        "queue::send",
        "2026-10-25T12:00:00Z",
        "now",
        "-",
    )
    .await;
    queue(
        &mut client,
        "queue::send",
        "2026-10-25T13:00:00Z",
        "later",
        "-",
    )
    .await;

    assert_eq!(client.run_due_tasks().await.unwrap(), 1);
    assert_eq!(*sends.lock().unwrap(), [("now".to_owned(), 0)]);
    assert_eq!(client.run_due_tasks().await.unwrap(), 0);

    clock.set("2026-10-25T13:00:00Z");
    assert_eq!(client.run_due_tasks().await.unwrap(), 1);
    assert_eq!(sends.lock().unwrap().len(), 2);
    assert_eq!(client.run_due_tasks().await.unwrap(), 0);
}

#[cot::test]
#[cfg_attr(
    miri,
    ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
)]
async fn a_queued_run_replaces_the_waiting_one_with_its_key() {
    let (mut client, clock, sends, _) = queue_client().await;
    let due = "2026-10-25T13:00:00Z";
    assert_eq!(
        queue(&mut client, "queue::send", due, "first", "send-1").await,
        StatusCode::OK
    );
    assert_eq!(
        queue(&mut client, "queue::send", due, "second", "send-1").await,
        StatusCode::OK
    );
    assert_eq!(
        queue(&mut client, "queue::send", due, "other", "send-2").await,
        StatusCode::OK
    );

    clock.set(due);
    assert_eq!(client.run_due_tasks().await.unwrap(), 2);
    let mut payloads: Vec<String> = sends
        .lock()
        .unwrap()
        .iter()
        .map(|(payload, _)| payload.clone())
        .collect();
    payloads.sort();
    assert_eq!(payloads, ["other", "second"]);
}

#[cot::test]
#[cfg_attr(
    miri,
    ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
)]
async fn a_failed_queued_run_is_retried_later_until_its_attempts_run_out() {
    let (mut client, clock, _, flaky) = queue_client().await;
    queue(
        &mut client,
        "queue::flaky",
        "2026-10-25T12:00:00Z",
        "flaky",
        "-",
    )
    .await;

    assert_eq!(client.run_due_tasks().await.unwrap(), 1);
    // the first retry waits 30 seconds
    clock.set("2026-10-25T12:00:29Z");
    assert_eq!(client.run_due_tasks().await.unwrap(), 0);
    clock.set("2026-10-25T12:00:30Z");
    assert_eq!(client.run_due_tasks().await.unwrap(), 1);
    // the second four times as long
    clock.set("2026-10-25T12:02:29Z");
    assert_eq!(client.run_due_tasks().await.unwrap(), 0);
    clock.set("2026-10-25T12:02:30Z");
    assert_eq!(client.run_due_tasks().await.unwrap(), 1);
    assert_eq!(
        *flaky.lock().unwrap(),
        [
            ("flaky".to_owned(), 0),
            ("flaky".to_owned(), 1),
            ("flaky".to_owned(), 2)
        ]
    );

    // after 3 attempts, it's given up
    clock.set("2026-10-26T12:00:00Z");
    assert_eq!(client.run_due_tasks().await.unwrap(), 0);
}

#[cot::test]
#[cfg_attr(
    miri,
    ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
)]
async fn only_queued_tasks_can_be_queued() {
    let (mut client, _, _, _) = queue_client().await;
    let status = queue(
        &mut client,
        "queue::missing",
        "2026-10-25T12:00:00Z",
        "x",
        "-",
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}
