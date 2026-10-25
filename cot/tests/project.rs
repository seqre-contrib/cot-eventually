use bytes::Bytes;
use cot::config::ProjectConfig;
use cot::html::Html;
use cot::project::RegisterAppsContext;
use cot::request::Request;
use cot::router::{Route, Router};
use cot::test::Client;
use cot::{App, AppBuilder, Project, StatusCode, reverse};

#[cot::test]
#[cfg_attr(
    miri,
    ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
)]
async fn cot_project_router_sub_path() {
    async fn hello(_request: Request) -> Html {
        Html::new("OK")
    }

    struct App1;
    impl App for App1 {
        fn name(&self) -> &'static str {
            "app1"
        }

        fn router(&self) -> Router {
            Router::with_urls([Route::with_handler_and_name("/index", hello, "index")])
        }
    }

    struct App2;
    impl App for App2 {
        fn name(&self) -> &'static str {
            "app2"
        }

        fn router(&self) -> Router {
            Router::with_urls([Route::with_handler_and_name("/hello", hello, "index")])
        }
    }

    struct TestProject;
    impl Project for TestProject {
        fn config(&self, config_name: &str) -> cot::Result<ProjectConfig> {
            assert!(
                (config_name == "test"),
                "unexpected config name: {config_name}"
            );
            Ok(ProjectConfig::default())
        }

        fn register_apps(&self, apps: &mut AppBuilder, _context: &RegisterAppsContext) {
            apps.register_with_views(App1, "");
            apps.register_with_views(App2, "/app");
        }
    }

    let mut client = Client::new(TestProject).await;

    let response = client.get("/app/hello").await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[cot::test]
#[cfg_attr(
    miri,
    ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
)]
async fn cot_router_reverse_local() {
    async fn get_index(request: Request) -> cot::Result<Html> {
        let reversed = reverse!(request, "index")?;
        Ok(Html::new(reversed))
    }

    struct App1;
    impl App for App1 {
        fn name(&self) -> &'static str {
            "app1"
        }

        fn router(&self) -> Router {
            Router::with_urls([Route::with_handler_and_name("/index1", get_index, "index")])
        }
    }

    struct App2;
    impl App for App2 {
        fn name(&self) -> &'static str {
            "app2"
        }

        fn router(&self) -> Router {
            Router::with_urls([Route::with_handler_and_name("/index2", get_index, "index")])
        }
    }

    struct TestProject;
    impl Project for TestProject {
        fn config(&self, _config_name: &str) -> cot::Result<ProjectConfig> {
            Ok(ProjectConfig::default())
        }

        fn register_apps(&self, apps: &mut AppBuilder, _context: &RegisterAppsContext) {
            apps.register_with_views(App1, "");
            apps.register_with_views(App2, "");
        }
    }

    let mut client = Client::new(TestProject).await;

    let response = client.get("/index1").await.unwrap();
    assert_eq!(
        response.into_body().into_bytes().await.unwrap(),
        Bytes::from("/index1")
    );

    let response = client.get("/index2").await.unwrap();
    assert_eq!(
        response.into_body().into_bytes().await.unwrap(),
        Bytes::from("/index2")
    );
}

#[cot::test]
#[cfg_attr(
    miri,
    ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
)]
async fn client_applies_migrations_and_initializes_apps() {
    use cot::auth::db::{DatabaseUser, DatabaseUserApp};
    use cot::config::DatabaseConfig;
    use cot::db::Database;

    async fn create_user(db: Database) -> cot::Result<Html> {
        let user = DatabaseUser::create_user(&db, "alice", "password").await?;
        Ok(Html::new(user.username().to_owned()))
    }

    async fn get_user(db: Database) -> cot::Result<Html> {
        let user = DatabaseUser::get_by_username(&db, "bob").await?;
        Ok(Html::new(
            user.map(|user| user.username().to_owned())
                .unwrap_or_default(),
        ))
    }

    struct UsersApp;
    #[async_trait::async_trait]
    impl App for UsersApp {
        fn name(&self) -> &'static str {
            "users"
        }

        // runs after the migrations, so the table exists
        async fn init(&self, context: &mut cot::ProjectContext) -> cot::Result<()> {
            DatabaseUser::create_user(context.database(), "bob", "password").await?;
            Ok(())
        }

        fn router(&self) -> Router {
            Router::with_urls([
                Route::with_handler("/create", create_user),
                Route::with_handler("/get", get_user),
            ])
        }
    }

    struct TestProject;
    impl Project for TestProject {
        fn config(&self, _config_name: &str) -> cot::Result<ProjectConfig> {
            Ok(ProjectConfig::builder()
                .database(DatabaseConfig::builder().url("sqlite::memory:").build())
                .build())
        }

        fn register_apps(&self, apps: &mut AppBuilder, _context: &RegisterAppsContext) {
            apps.register(DatabaseUserApp::new());
            apps.register_with_views(UsersApp, "");
        }
    }

    let mut client = Client::new(TestProject).await;

    // the table of `DatabaseUser` exists, so the user can be created
    let response = client.get("/create").await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().into_bytes().await.unwrap(),
        Bytes::from("alice")
    );

    // and the app was initialized
    let response = client.get("/get").await.unwrap();
    assert_eq!(
        response.into_body().into_bytes().await.unwrap(),
        Bytes::from("bob")
    );

    // the test can look at the database, too
    let user = DatabaseUser::get_by_username(client.context().database(), "alice")
        .await
        .unwrap();
    assert!(user.is_some());
}

#[cfg(feature = "db")]
mod before_migrations {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use cot::auth::db::DatabaseUserApp;
    use cot::config::{DatabaseConfig, ProjectConfig};
    use cot::db::migrations::SyncDynMigration;
    use cot::project::{RegisterAppsContext, run_at_with_shutdown};
    use cot::{App, AppBuilder, Bootstrapper, Project, ProjectContext};

    /// Records the pending migrations it is called with, and fails if asked to.
    struct RecordingApp {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        fail: bool,
    }

    #[async_trait]
    impl App for RecordingApp {
        fn name(&self) -> &'static str {
            "recording"
        }

        async fn before_migrations(
            &self,
            _context: &ProjectContext,
            pending: &[&SyncDynMigration],
        ) -> cot::Result<()> {
            let names = pending
                .iter()
                .map(|migration| format!("{}::{}", migration.app_name(), migration.name()))
                .collect();
            self.calls.lock().unwrap().push(names);

            if self.fail {
                Err(cot::Error::internal("backup failed"))
            } else {
                Ok(())
            }
        }
    }

    struct TestProject {
        database_url: String,
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        fail: bool,
    }

    impl Project for TestProject {
        fn config(&self, _config_name: &str) -> cot::Result<ProjectConfig> {
            Ok(ProjectConfig::builder()
                .register_panic_hook(false)
                .database(
                    DatabaseConfig::builder()
                        .url(self.database_url.as_str())
                        .build(),
                )
                .build())
        }

        fn register_apps(&self, apps: &mut AppBuilder, _context: &RegisterAppsContext) {
            apps.register(DatabaseUserApp::new());
            apps.register(RecordingApp {
                calls: Arc::clone(&self.calls),
                fail: self.fail,
            });
        }
    }

    /// Starts the project and shuts it down right away.
    async fn start(project: TestProject) -> cot::Result<()> {
        let bootstrapper = Bootstrapper::new(project)
            .with_config_name("test")?
            .boot()
            .await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        run_at_with_shutdown(bootstrapper, listener, async {}).await
    }

    #[cot::test]
    #[cfg_attr(
        miri,
        ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
    )]
    async fn called_only_with_pending_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("db.sqlite3").display()
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let project = || TestProject {
            database_url: database_url.clone(),
            calls: Arc::clone(&calls),
            fail: false,
        };

        start(project()).await.unwrap();
        start(project()).await.unwrap();

        // called on the first start only: on the second, there is nothing to
        // apply
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].contains(&"cot::m_0001_initial".to_owned()),
            "{calls:?}"
        );
    }

    #[cot::test]
    #[cfg_attr(
        miri,
        ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
    )]
    async fn error_stops_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("db.sqlite3").display()
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let project = |fail| TestProject {
            database_url: database_url.clone(),
            calls: Arc::clone(&calls),
            fail,
        };

        let error = start(project(true)).await.unwrap_err();
        assert!(error.to_string().contains("backup failed"), "{error}");

        // nothing was applied, so the next start sees the same migrations
        // pending
        start(project(false)).await.unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], calls[1]);
    }
}
