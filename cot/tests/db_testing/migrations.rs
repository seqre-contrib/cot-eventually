use cot::App;
use cot::auth::db::DatabaseUserApp;
use cot::db::migrations::{
    DefaultValue, Field, Migration, MigrationDependency, MigrationEngine, Operation,
    SyncDynMigration, wrap_migrations,
};
use cot::db::{Auto, Database, DatabaseField, Identifier, Model};
use cot::session::db::SessionApp;
use cot::test::TestDatabase;
use cot_macros::{model, query};

const SNAPSHOT_RELATIVE_PATH: &str = "snapshots/migrations";

// mirror the internal AppliedMigration model
#[derive(Debug)]
#[model(table_name = "cot__migrations", model_type = "internal")]
struct AppliedMigration {
    #[model(primary_key)]
    id: Auto<i32>,
    app: String,
    name: String,
    applied: chrono::DateTime<chrono::FixedOffset>,
}
struct RollbackApp1Initial;

impl Migration for RollbackApp1Initial {
    const APP_NAME: &'static str = "rollback_app1";
    const MIGRATION_NAME: &'static str = "m_0001_initial";
    const DEPENDENCIES: &'static [MigrationDependency] = &[];
    const OPERATIONS: &'static [Operation] = &[Operation::create_model()
        .table_name(Identifier::new("rollback_single__first"))
        .fields(&[
            Field::new(Identifier::new("id"), <i32 as DatabaseField>::TYPE)
                .primary_key()
                .auto(),
        ])
        .build()];
}

struct RollbackApp10002;

impl Migration for RollbackApp10002 {
    const APP_NAME: &'static str = "rollback_app1";
    const MIGRATION_NAME: &'static str = "m_0002_second";
    const DEPENDENCIES: &'static [MigrationDependency] = &[MigrationDependency::migration(
        "rollback_app1",
        "m_0001_initial",
    )];
    const OPERATIONS: &'static [Operation] = &[Operation::create_model()
        .table_name(Identifier::new("rollback_app1__second"))
        .fields(&[
            Field::new(Identifier::new("id"), <i32 as DatabaseField>::TYPE)
                .primary_key()
                .auto(),
        ])
        .build()];
}

struct RollbackApp1003;

impl Migration for RollbackApp1003 {
    const APP_NAME: &'static str = "rollback_app1";
    const MIGRATION_NAME: &'static str = "m_0003_third";
    const DEPENDENCIES: &'static [MigrationDependency] = &[MigrationDependency::migration(
        "rollback_app1",
        "m_0002_second",
    )];
    const OPERATIONS: &'static [Operation] = &[Operation::create_model()
        .table_name(Identifier::new("rollback_single__third"))
        .fields(&[
            Field::new(Identifier::new("id"), <i32 as DatabaseField>::TYPE)
                .primary_key()
                .auto(),
        ])
        .build()];
}

struct RollbackApp2Initial;

impl Migration for RollbackApp2Initial {
    const APP_NAME: &'static str = "rollback_app2";
    const MIGRATION_NAME: &'static str = "m_0001_initial";
    const DEPENDENCIES: &'static [MigrationDependency] = &[];
    const OPERATIONS: &'static [Operation] = &[Operation::create_model()
        .table_name(Identifier::new("rollback_app2__foo"))
        .fields(&[
            Field::new(Identifier::new("id"), <i32 as DatabaseField>::TYPE)
                .primary_key()
                .auto(),
        ])
        .build()];
}

struct RollbackDependentInitial;

impl Migration for RollbackDependentInitial {
    const APP_NAME: &'static str = "rollback_dependent";
    const MIGRATION_NAME: &'static str = "m_0001_initial";
    const DEPENDENCIES: &'static [MigrationDependency] = &[MigrationDependency::migration(
        "rollback_app1",
        "m_0002_second",
    )];
    const OPERATIONS: &'static [Operation] = &[Operation::create_model()
        .table_name(Identifier::new("rollback_dependent__bar"))
        .fields(&[
            Field::new(Identifier::new("id"), <i32 as DatabaseField>::TYPE)
                .primary_key()
                .auto(),
        ])
        .build()];
}

#[cot_macros::dbtest]
async fn test_migration_rollback_no_deps(test_db: &mut TestDatabase) {
    let engine = MigrationEngine::new([RollbackApp1Initial]).unwrap();
    engine.run(&test_db.database()).await.unwrap();
}

async fn assert_migration_applied(database: &Database, app: &str, name: &str, expected: bool) {
    let applied = query!(AppliedMigration, $app == app && $name == name)
        .exists(database)
        .await
        .unwrap();
    assert_eq!(applied, expected, "{app}::{name}");
}

async fn assert_migrations_applied(db: &Database, expected: &[(&str, &str, bool)]) {
    for &(app, name, applied) in expected {
        assert_migration_applied(db, app, name, applied).await;
    }
}

async fn migration_rollback_dry_run(
    engine: &MigrationEngine,
    db: &Database,
    output: &mut Vec<u8>,
    migration_name: &str,
    app_name: &str,
) -> String {
    output.clear();
    engine
        .rollback_dry_run(db, migration_name, app_name, output)
        .await
        .unwrap();
    std::str::from_utf8(output).unwrap().to_owned()
}

async fn migration_rollback(
    engine: &MigrationEngine,
    db: &Database,
    output: &mut Vec<u8>,
    migration_name: &str,
    app_name: &str,
) -> String {
    output.clear();
    engine
        .rollback(db, migration_name, app_name, output)
        .await
        .unwrap();
    std::str::from_utf8(output).unwrap().to_owned()
}

#[cot_macros::dbtest]
async fn test_migration_engine_rollback_single_app(test_db: &mut TestDatabase) {
    #[expect(trivial_casts)]
    let engine = MigrationEngine::new([
        &RollbackApp1Initial as &SyncDynMigration,
        &RollbackApp10002 as &SyncDynMigration,
        &RollbackApp1003 as &SyncDynMigration,
    ])
    .unwrap();
    let mut output = Vec::new();

    engine.run(&test_db.database()).await.unwrap();
    // migrations should be applied
    assert_migrations_applied(
        &test_db.database(),
        &[
            ("rollback_app1", "m_0001_initial", true),
            ("rollback_app1", "m_0002_second", true),
            ("rollback_app1", "m_0003_third", true),
        ],
    )
    .await;

    // rollback everything except the initial migration
    let dry_run_output = migration_rollback_dry_run(
        &engine,
        &test_db.database(),
        &mut output,
        "0001",
        "rollback_app1",
    )
    .await;

    insta::with_settings!({snapshot_path => SNAPSHOT_RELATIVE_PATH}, {
        insta::assert_snapshot!(
            "migration_engine_rollback_single_app_dry_run",
            dry_run_output,
        );
    });

    let rollback_output = migration_rollback(
        &engine,
        &test_db.database(),
        &mut output,
        "0001",
        "rollback_app1",
    )
    .await;
    insta::with_settings!({snapshot_path => SNAPSHOT_RELATIVE_PATH}, {
        insta::assert_snapshot!("migration_engine_rollback_single_app_rollback", rollback_output);
    });

    assert_migrations_applied(
        &test_db.database(),
        &[
            // the initial migration should stay applied
            ("rollback_app1", "m_0001_initial", true),
            // everything else should be unapplied
            ("rollback_app1", "m_0002_second", false),
            ("rollback_app1", "m_0003_third", false),
        ],
    )
    .await;
}

#[cot_macros::dbtest]
async fn test_migration_rollback_unrelated_apps(test_db: &mut TestDatabase) {
    let mut migrations = DatabaseUserApp::new().migrations();
    // combine migrations from multiple apps/crates
    #[expect(trivial_casts)]
    migrations.extend(wrap_migrations(&[
        &RollbackApp1Initial as &SyncDynMigration,
        &RollbackApp10002 as &SyncDynMigration,
        &RollbackApp2Initial as &SyncDynMigration,
    ]));
    migrations.extend(SessionApp::new().migrations());
    let mut output = Vec::new();

    let engine = MigrationEngine::new(migrations).unwrap();

    engine.run(&test_db.database()).await.unwrap();
    // migrations should be applied across all apps
    assert_migrations_applied(
        &test_db.database(),
        &[
            ("cot", "m_0001_initial", true),
            ("cot_session", "m_0001_initial", true),
            ("rollback_app1", "m_0001_initial", true),
            ("rollback_app1", "m_0002_second", true),
            ("rollback_app2", "m_0001_initial", true),
        ],
    )
    .await;

    // rollback every migration in the rollback_app1 app except the initial
    let dry_run_output = migration_rollback_dry_run(
        &engine,
        &test_db.database(),
        &mut output,
        "0001",
        "rollback_app1",
    )
    .await;

    insta::with_settings!({snapshot_path => SNAPSHOT_RELATIVE_PATH}, {
        insta::assert_snapshot!(
            "migration_rollback_unrelated_apps_dry_run",
            dry_run_output,
        );
    });

    let rollback_output = migration_rollback(
        &engine,
        &test_db.database(),
        &mut output,
        "0001",
        "rollback_app1",
    )
    .await;
    insta::with_settings!({snapshot_path => SNAPSHOT_RELATIVE_PATH}, {
        insta::assert_snapshot!("migration_rollback_unrelated_apps_rollback", rollback_output);
    });

    assert_migrations_applied(
        &test_db.database(),
        &[
            // the initial migration should stay applied
            ("rollback_app1", "m_0001_initial", true),
            // everything else in the rollback_app1 app should be unapplied
            ("rollback_app1", "m_0002_second", false),
            // migrations from other apps should remain unaffected
            ("cot", "m_0001_initial", true),
            ("cot_session", "m_0001_initial", true),
            ("rollback_app2", "m_0001_initial", true),
        ],
    )
    .await;
}

#[cot_macros::dbtest]
async fn test_migration_engine_rollback_includes_dependent_apps(test_db: &mut TestDatabase) {
    #[expect(trivial_casts)]
    let engine = MigrationEngine::new([
        &RollbackApp1Initial as &SyncDynMigration,
        &RollbackApp10002 as &SyncDynMigration,
        &RollbackDependentInitial as &SyncDynMigration,
        &RollbackApp2Initial as &SyncDynMigration,
    ])
    .unwrap();
    let mut output = Vec::new();

    engine.run(&test_db.database()).await.unwrap();

    assert_migrations_applied(
        &test_db.database(),
        &[
            ("rollback_app1", "m_0001_initial", true),
            ("rollback_app1", "m_0002_second", true),
            ("rollback_dependent", "m_0001_initial", true),
            ("rollback_app2", "m_0001_initial", true),
        ],
    )
    .await;

    // rollback everything except the initial migration in the
    // source/independent app
    let dry_run_output = migration_rollback_dry_run(
        &engine,
        &test_db.database(),
        &mut output,
        "0001",
        "rollback_app1",
    )
    .await;

    insta::with_settings!({snapshot_path => SNAPSHOT_RELATIVE_PATH}, {
        insta::assert_snapshot!(
            "migration_engine_rollback_includes_dependent_apps_dry_run",
            dry_run_output,
        );
    });

    let rollback_output = migration_rollback(
        &engine,
        &test_db.database(),
        &mut output,
        "0001",
        "rollback_app1",
    )
    .await;
    insta::with_settings!({snapshot_path => SNAPSHOT_RELATIVE_PATH}, {
        insta::assert_snapshot!(
            "migration_engine_rollback_includes_dependent_apps_rollback",
            rollback_output
        );
    });

    assert_migrations_applied(
        &test_db.database(),
        &[
            ("rollback_app1", "m_0001_initial", true),
            ("rollback_app1", "m_0002_second", false),
            // the sink/dependent app should also be unapplied/rolled back
            ("rollback_dependent", "m_0001_initial", false),
            // migrations from non-dependent apps should remain unaffected
            ("rollback_app2", "m_0001_initial", true),
        ],
    )
    .await;
}

#[cot_macros::dbtest]
async fn test_migration_engine_rollback_zero(test_db: &mut TestDatabase) {
    #[expect(trivial_casts)]
    let engine = MigrationEngine::new([
        &RollbackApp1Initial as &SyncDynMigration,
        &RollbackApp10002 as &SyncDynMigration,
        &RollbackApp1003 as &SyncDynMigration,
        &RollbackApp2Initial as &SyncDynMigration,
    ])
    .unwrap();
    let mut output = Vec::new();

    engine.run(&test_db.database()).await.unwrap();

    assert_migrations_applied(
        &test_db.database(),
        &[
            ("rollback_app1", "m_0001_initial", true),
            ("rollback_app1", "m_0002_second", true),
            ("rollback_app1", "m_0003_third", true),
            ("rollback_app2", "m_0001_initial", true),
        ],
    )
    .await;

    let dry_run_output = migration_rollback_dry_run(
        &engine,
        &test_db.database(),
        &mut output,
        "zero",
        "rollback_app1",
    )
    .await;

    insta::with_settings!({snapshot_path => SNAPSHOT_RELATIVE_PATH}, {
        insta::assert_snapshot!(
            "migration_engine_rollback_zero_dry_run",
            dry_run_output,
        );
    });

    let rollback_output = migration_rollback(
        &engine,
        &test_db.database(),
        &mut output,
        "zero",
        "rollback_app1",
    )
    .await;
    insta::with_settings!({snapshot_path => SNAPSHOT_RELATIVE_PATH}, {
        insta::assert_snapshot!("migration_engine_rollback_zero_rollback", rollback_output);
    });

    assert_migrations_applied(
        &test_db.database(),
        &[
            // everything should be unapplied
            ("rollback_app1", "m_0001_initial", false),
            ("rollback_app1", "m_0002_second", false),
            ("rollback_app1", "m_0003_third", false),
            // the non dependent apps should be unaffected
            ("rollback_app2", "m_0001_initial", true),
        ],
    )
    .await;
}

#[derive(Debug, Clone, PartialEq)]
#[model(table_name = "default_value_model")]
struct DefaultValueBefore {
    #[model(primary_key)]
    id: Auto<i32>,
    name: String,
}

#[derive(Debug, Clone, PartialEq)]
#[model(table_name = "default_value_model")]
struct DefaultValueAfter {
    #[model(primary_key)]
    id: Auto<i32>,
    name: String,
    #[model(default = true)]
    is_active: bool,
    #[model(default = -3)]
    retries: i64,
    #[model(default = 0.5)]
    ratio: f64,
    #[model(default = "none")]
    label: String,
}

const CREATE_DEFAULT_VALUE_MODEL: Operation = Operation::create_model()
    .table_name(Identifier::new("cot__default_value_model"))
    .fields(&[
        Field::new(Identifier::new("id"), <i32 as DatabaseField>::TYPE)
            .primary_key()
            .auto(),
        Field::new(Identifier::new("name"), <String as DatabaseField>::TYPE),
    ])
    .build();

const ADD_DEFAULT_VALUE_FIELDS: [Operation; 4] = [
    Operation::add_field()
        .table_name(Identifier::new("cot__default_value_model"))
        .field(
            Field::new(Identifier::new("is_active"), <bool as DatabaseField>::TYPE)
                .default_value(DefaultValue::Bool(true)),
        )
        .build(),
    Operation::add_field()
        .table_name(Identifier::new("cot__default_value_model"))
        .field(
            Field::new(Identifier::new("retries"), <i64 as DatabaseField>::TYPE)
                .default_value(DefaultValue::Int(-3)),
        )
        .build(),
    Operation::add_field()
        .table_name(Identifier::new("cot__default_value_model"))
        .field(
            Field::new(Identifier::new("ratio"), <f64 as DatabaseField>::TYPE)
                .default_value(DefaultValue::Float(0.5)),
        )
        .build(),
    Operation::add_field()
        .table_name(Identifier::new("cot__default_value_model"))
        .field(
            Field::new(Identifier::new("label"), <String as DatabaseField>::TYPE)
                .default_value(DefaultValue::Text("none")),
        )
        .build(),
];

#[cot_macros::dbtest]
async fn test_add_non_nullable_field_with_default(test_db: &mut TestDatabase) {
    let database = test_db.database();
    CREATE_DEFAULT_VALUE_MODEL
        .forwards(&database)
        .await
        .unwrap();
    let mut existing = DefaultValueBefore {
        id: Auto::auto(),
        name: "existing".to_owned(),
    };
    existing.save(&database).await.unwrap();

    for operation in &ADD_DEFAULT_VALUE_FIELDS {
        operation.forwards(&database).await.unwrap();
    }

    let rows = DefaultValueAfter::objects().all(&database).await.unwrap();
    assert_eq!(
        rows,
        [DefaultValueAfter {
            id: existing.id,
            name: "existing".to_owned(),
            is_active: true,
            retries: -3,
            ratio: 0.5,
            label: "none".to_owned(),
        }]
    );
}
