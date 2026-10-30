use cot::App;
use cot::tasks::db::{DatabaseTaskBackend, TaskQueueApp, queue};
use cot::tasks::{TaskBackend, TaskRun};
use cot::test::TestDatabase;
use jiff::{Timestamp, ToSpan};

async fn make_backend(test_db: &mut TestDatabase) -> DatabaseTaskBackend {
    test_db.add_migrations(TaskQueueApp::new().migrations());
    test_db.run_migrations().await;
    DatabaseTaskBackend::new(test_db.database())
}

fn now() -> Timestamp {
    "2026-10-25T12:00:00Z".parse().unwrap()
}

#[cot_macros::dbtest]
async fn takes_the_due_runs_earliest_first(test_db: &mut TestDatabase) {
    let backend = make_backend(test_db).await;
    let later = TaskRun::new("later", now() + 1.minute());
    let first = TaskRun::new("first", now() - 1.hour())
        .with_payload(&[1, 2])
        .unwrap();
    let mut second = TaskRun::new("second", now()).with_key("second/1");
    second.attempt = 2;
    backend.enqueue(later).await.unwrap();
    backend.enqueue(second.clone()).await.unwrap();
    backend.enqueue(first.clone()).await.unwrap();

    assert_eq!(backend.take_due(now()).await.unwrap(), [first, second]);
    assert_eq!(backend.take_due(now()).await.unwrap(), []);
    assert_eq!(backend.take_due(now() + 1.minute()).await.unwrap().len(), 1);
}

#[cot_macros::dbtest]
async fn a_run_replaces_the_waiting_one_with_its_key(test_db: &mut TestDatabase) {
    let backend = make_backend(test_db).await;
    backend
        .enqueue(TaskRun::new("remind", now()).with_key("remind/1"))
        .await
        .unwrap();
    let replacement = TaskRun::new("remind", now() - 1.minute()).with_key("remind/1");
    backend.enqueue(replacement.clone()).await.unwrap();
    backend
        .enqueue(TaskRun::new("remind", now()).with_key("remind/2"))
        .await
        .unwrap();

    let due = backend.take_due(now()).await.unwrap();
    assert_eq!(due.len(), 2);
    assert_eq!(due[0], replacement);
}

#[cot_macros::dbtest]
async fn runs_queued_in_a_transaction_wait_for_it(test_db: &mut TestDatabase) {
    let backend = make_backend(test_db).await;
    let database = test_db.database();

    let mut transaction = database.begin().await.unwrap();
    queue(&mut transaction, TaskRun::new("rolled_back", now()))
        .await
        .unwrap();
    transaction.rollback().await.unwrap();

    let mut transaction = database.begin().await.unwrap();
    queue(&mut transaction, TaskRun::new("committed", now()))
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    assert_eq!(
        backend.take_due(now()).await.unwrap(),
        [TaskRun::new("committed", now())]
    );
}
