use cot::config::ProjectConfig;
use cot::html::Html;
use cot::project::{AppState, RegisterAppsContext, RegisterStateContext};
use cot::request::RequestExt;
use cot::request::extractors::State;
use cot::router::{Route, Router};
use cot::test::{Client, TestRequestBuilder};
use cot::{App, AppBuilder, Project, StatusCode};

#[cfg(feature = "cache")]
#[cot::test]
async fn request_cache() {
    let mut request_builder = TestRequestBuilder::get("/");
    let mut request = request_builder.build();
    let request_cache: cot::cache::Cache = request.extract_from_head().await.unwrap();

    // this will use the default in-memory cache
    request_cache
        .insert("user:1", serde_json::json!({"name": "Alice"}))
        .await
        .unwrap();

    let user: Option<serde_json::Value> = request_cache.get("user:1").await.unwrap();
    assert!(user.is_some());
    assert_eq!(user.unwrap()["name"], "Alice");
}

#[cfg(feature = "email")]
#[cot::test]
async fn request_email() {
    use cot::common_types::Email;
    use cot::email::EmailMessage;

    let mut request_builder = TestRequestBuilder::get("/");
    let mut request = request_builder.build();
    let email_service: cot::email::Email = request.extract_from_head().await.unwrap();

    let message = EmailMessage::builder()
        .from(Email::new("sender@example.com").unwrap())
        .to(vec![Email::new("recipient@example.com").unwrap()])
        .subject("Test Email")
        .body("Hello, this is a test email.")
        .build()
        .unwrap();

    assert!(email_service.send(message).await.is_ok());
}

#[derive(Debug, Clone)]
struct Greeting(&'static str);

#[cot::test]
#[cfg_attr(
    miri,
    ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
)]
async fn request_state() {
    async fn index(State(greeting): State<Greeting>) -> Html {
        Html::new(greeting.0)
    }

    struct TestApp;
    impl App for TestApp {
        fn name(&self) -> &'static str {
            "test_app"
        }

        fn router(&self) -> Router {
            Router::with_urls([Route::with_handler("/", index)])
        }
    }

    struct TestProject;
    impl Project for TestProject {
        fn config(&self, _config_name: &str) -> cot::Result<ProjectConfig> {
            Ok(ProjectConfig::default())
        }

        fn register_apps(&self, apps: &mut AppBuilder, _context: &RegisterAppsContext) {
            apps.register_with_views(TestApp, "");
        }

        fn register_state(&self, state: &mut AppState, _context: &RegisterStateContext) {
            state.insert(Greeting("Hello"));
        }
    }

    let mut client = Client::new(TestProject).await;
    let response = client.get("/").await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.into_body().into_bytes().await.unwrap(), "Hello");
}

#[cot::test]
async fn request_state_not_registered() {
    let mut request = TestRequestBuilder::get("/").build();

    let error = request
        .extract_from_head::<State<Greeting>>()
        .await
        .unwrap_err();

    assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.to_string().contains("Greeting"), "{error}");
}
