//! Adds the API tokens of database users.

#[derive(Debug, Copy, Clone)]
pub(super) struct Migration;
impl ::cot::db::migrations::Migration for Migration {
    const APP_NAME: &'static str = "cot";
    const MIGRATION_NAME: &'static str = "m_0002_database_user_token";
    const DEPENDENCIES: &'static [::cot::db::migrations::MigrationDependency] =
        &[::cot::db::migrations::MigrationDependency::migration(
            "cot",
            "m_0001_initial",
        )];
    const OPERATIONS: &'static [::cot::db::migrations::Operation] = &[
        ::cot::db::migrations::Operation::create_model()
            .table_name(::cot::db::Identifier::new("cot__database_user_token"))
            .fields(
                &[
                    ::cot::db::migrations::Field::new(
                            ::cot::db::Identifier::new("id"),
                            <cot::db::Auto<i64> as ::cot::db::DatabaseField>::TYPE,
                        )
                        .auto()
                        .primary_key()
                        .set_null(
                            <cot::db::Auto<i64> as ::cot::db::DatabaseField>::NULLABLE,
                        ),
                    ::cot::db::migrations::Field::new(
                            ::cot::db::Identifier::new("user"),
                            <cot::db::ForeignKey<
                                crate::auth::db::DatabaseUser,
                            > as ::cot::db::DatabaseField>::TYPE,
                        )
                        .set_null(
                            <cot::db::ForeignKey<
                                crate::auth::db::DatabaseUser,
                            > as ::cot::db::DatabaseField>::NULLABLE,
                        )
                        .foreign_key(
                            <crate::auth::db::DatabaseUser as ::cot::db::Model>::TABLE_NAME,
                            <crate::auth::db::DatabaseUser as ::cot::db::Model>::PRIMARY_KEY_NAME,
                            ::cot::db::ForeignKeyOnDeletePolicy::Cascade,
                            ::cot::db::ForeignKeyOnUpdatePolicy::Cascade,
                        ),
                    ::cot::db::migrations::Field::new(
                            ::cot::db::Identifier::new("name"),
                            <crate::db::LimitedString<
                                { crate::auth::db::MAX_TOKEN_NAME_LENGTH },
                            > as ::cot::db::DatabaseField>::TYPE,
                        )
                        .set_null(
                            <crate::db::LimitedString<
                                { crate::auth::db::MAX_TOKEN_NAME_LENGTH },
                            > as ::cot::db::DatabaseField>::NULLABLE,
                        ),
                    ::cot::db::migrations::Field::new(
                            ::cot::db::Identifier::new("token_hash"),
                            <crate::db::LimitedString<
                                { crate::auth::db::TOKEN_HASH_LENGTH },
                            > as ::cot::db::DatabaseField>::TYPE,
                        )
                        .set_null(
                            <crate::db::LimitedString<
                                { crate::auth::db::TOKEN_HASH_LENGTH },
                            > as ::cot::db::DatabaseField>::NULLABLE,
                        )
                        .unique(),
                    ::cot::db::migrations::Field::new(
                            ::cot::db::Identifier::new("created"),
                            <chrono::DateTime<
                                chrono::FixedOffset,
                            > as ::cot::db::DatabaseField>::TYPE,
                        )
                        .set_null(
                            <chrono::DateTime<
                                chrono::FixedOffset,
                            > as ::cot::db::DatabaseField>::NULLABLE,
                        ),
                ],
            )
            .build(),
    ];
}

#[derive(::core::fmt::Debug)]
#[::cot::db::model(model_type = "migration")]
struct _DatabaseUserToken {
    #[model(primary_key)]
    id: cot::db::Auto<i64>,
    #[model(foreign_key(on_delete = "cascade"))]
    user: cot::db::ForeignKey<crate::auth::db::DatabaseUser>,
    name: crate::db::LimitedString<{ crate::auth::db::MAX_TOKEN_NAME_LENGTH }>,
    #[model(unique)]
    token_hash: crate::db::LimitedString<{ crate::auth::db::TOKEN_HASH_LENGTH }>,
    created: chrono::DateTime<chrono::FixedOffset>,
}
