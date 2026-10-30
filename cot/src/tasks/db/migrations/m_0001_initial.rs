//! The table of the runs that are waiting.

#[derive(Debug, Copy, Clone)]
pub(crate) struct Migration;
impl ::cot::db::migrations::Migration for Migration {
    const APP_NAME: &'static str = "cot_tasks";
    const MIGRATION_NAME: &'static str = "m_0001_initial";
    const DEPENDENCIES: &'static [::cot::db::migrations::MigrationDependency] = &[];
    const OPERATIONS: &'static [::cot::db::migrations::Operation] =
        &[::cot::db::migrations::Operation::create_model()
            .table_name(::cot::db::Identifier::new("cot__task_run"))
            .fields(&[
                ::cot::db::migrations::Field::new(
                    ::cot::db::Identifier::new("id"),
                    <cot::db::Auto<i64> as ::cot::db::DatabaseField>::TYPE,
                )
                .auto()
                .primary_key()
                .set_null(<cot::db::Auto<i64> as ::cot::db::DatabaseField>::NULLABLE),
                ::cot::db::migrations::Field::new(
                    ::cot::db::Identifier::new("task"),
                    <String as ::cot::db::DatabaseField>::TYPE,
                )
                .set_null(<String as ::cot::db::DatabaseField>::NULLABLE),
                ::cot::db::migrations::Field::new(
                    ::cot::db::Identifier::new("key"),
                    <Option<String> as ::cot::db::DatabaseField>::TYPE,
                )
                .set_null(<Option<String> as ::cot::db::DatabaseField>::NULLABLE)
                .unique(),
                ::cot::db::migrations::Field::new(
                    ::cot::db::Identifier::new("payload"),
                    <Option<String> as ::cot::db::DatabaseField>::TYPE,
                )
                .set_null(<Option<String> as ::cot::db::DatabaseField>::NULLABLE),
                ::cot::db::migrations::Field::new(
                    ::cot::db::Identifier::new("due"),
                    <i64 as ::cot::db::DatabaseField>::TYPE,
                )
                .set_null(<i64 as ::cot::db::DatabaseField>::NULLABLE),
                ::cot::db::migrations::Field::new(
                    ::cot::db::Identifier::new("attempt"),
                    <u32 as ::cot::db::DatabaseField>::TYPE,
                )
                .set_null(<u32 as ::cot::db::DatabaseField>::NULLABLE),
            ])
            .build()];
}

#[derive(::core::fmt::Debug)]
#[::cot::db::model(model_type = "migration")]
struct _StoredRun {
    #[model(primary_key)]
    pub(crate) id: cot::db::Auto<i64>,
    pub(crate) task: String,
    #[model(unique)]
    pub(crate) key: Option<String>,
    pub(crate) payload: Option<String>,
    pub(crate) due: i64,
    pub(crate) attempt: u32,
}
