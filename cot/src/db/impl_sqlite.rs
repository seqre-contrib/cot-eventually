//! Database interface implementation – SQLite backend.

use sea_query::extension::sqlite::SqliteExpr;
use sea_query::{ExprTrait, LikeExpr, SimpleExpr};
use sea_query_sqlx::SqlxValues;

use crate::db::query::QueryBuildingError;
use crate::db::query::expr::like::{
    CaseSensitivity, LIKE_ESCAPE_CHAR, LikeExprBuilder, to_sql_like,
};
use crate::db::sea_query_db::{impl_sea_query_db_backend, impl_sea_query_transaction_backend};

impl_sea_query_db_backend!(DatabaseSqlite: sqlx::sqlite::Sqlite, sqlx::sqlite::SqlitePool, SqliteRow, SqliteValueRef, sea_query::SqliteQueryBuilder);
impl_sea_query_transaction_backend!(DatabaseSqlite, TransactionSqlite: sqlx::sqlite::Sqlite, SqliteRow, sea_query::SqliteQueryBuilder);

impl DatabaseSqlite {
    pub(super) async fn with_config(
        url: &str,
        config: &crate::config::SqliteConfig,
    ) -> crate::db::Result<Self> {
        use crate::config::SqliteTransactionBehavior;

        let options = connect_options(url, config)?;
        let db_connection = sqlx::sqlite::SqlitePool::connect_with(options).await?;
        let mut db = Self::from_pool(db_connection).await?;
        db.begin_statement = config.transaction_behavior.map(|behavior| match behavior {
            SqliteTransactionBehavior::Deferred => "BEGIN DEFERRED",
            SqliteTransactionBehavior::Immediate => "BEGIN IMMEDIATE",
            SqliteTransactionBehavior::Exclusive => "BEGIN EXCLUSIVE",
        });
        Ok(db)
    }

    async fn init(&self) -> crate::db::Result<()> {
        self.raw("PRAGMA foreign_keys = ON").await?;
        Ok(())
    }

    async fn raw(&self, sql: &str) -> crate::db::Result<crate::db::StatementResult> {
        self.raw_with(sql, SqlxValues(sea_query::Values(Vec::new())))
            .await
    }

    fn prepare_values(values: &mut SqlxValues) {
        for value in &mut values.0.0 {
            crate::db::fields::jiff_to_sqlite_text_value(value);
        }
    }

    #[expect(clippy::unnecessary_wraps)] // to have a unified interface between database impls
    fn last_inserted_row_id_for(result: &sqlx::sqlite::SqliteQueryResult) -> Option<u64> {
        #[expect(clippy::cast_sign_loss)]
        Some(result.last_insert_rowid() as u64)
    }

    #[expect(clippy::unused_self)] // to have a unified interface between database impls
    pub(super) fn sea_query_column_type_for(
        &self,
        column_type: crate::db::ColumnType,
    ) -> sea_query::ColumnType {
        sea_query::ColumnType::from(column_type)
    }
}

fn connect_options(
    url: &str,
    config: &crate::config::SqliteConfig,
) -> crate::db::Result<sqlx::sqlite::SqliteConnectOptions> {
    use std::str::FromStr;

    use crate::config::{SqliteJournalMode, SqliteSynchronous};

    let mut options = sqlx::sqlite::SqliteConnectOptions::from_str(url)?;
    if let Some(journal_mode) = config.journal_mode {
        options = options.journal_mode(match journal_mode {
            SqliteJournalMode::Delete => sqlx::sqlite::SqliteJournalMode::Delete,
            SqliteJournalMode::Truncate => sqlx::sqlite::SqliteJournalMode::Truncate,
            SqliteJournalMode::Persist => sqlx::sqlite::SqliteJournalMode::Persist,
            SqliteJournalMode::Memory => sqlx::sqlite::SqliteJournalMode::Memory,
            SqliteJournalMode::Wal => sqlx::sqlite::SqliteJournalMode::Wal,
            SqliteJournalMode::Off => sqlx::sqlite::SqliteJournalMode::Off,
        });
    }
    if let Some(synchronous) = config.synchronous {
        options = options.synchronous(match synchronous {
            SqliteSynchronous::Off => sqlx::sqlite::SqliteSynchronous::Off,
            SqliteSynchronous::Normal => sqlx::sqlite::SqliteSynchronous::Normal,
            SqliteSynchronous::Full => sqlx::sqlite::SqliteSynchronous::Full,
            SqliteSynchronous::Extra => sqlx::sqlite::SqliteSynchronous::Extra,
        });
    }
    if let Some(busy_timeout) = config.busy_timeout {
        options = options.busy_timeout(busy_timeout);
    }
    Ok(options)
}

impl LikeExprBuilder for DatabaseSqlite {
    fn like_expr(
        &self,
        lhs: SimpleExpr,
        glob_pattern: &str,
        case_sensitivity: CaseSensitivity,
    ) -> Result<SimpleExpr, QueryBuildingError> {
        build_like_expr(lhs, glob_pattern, case_sensitivity)
    }
}

/// Builds the SQLite pattern-matching expression for the given case
/// sensitivity. Shared between the connection and transaction backends.
// Returns `Result` to match the fallible `LikeExprBuilder::like_expr` contract,
// even though this backend can always express the pattern.
#[expect(clippy::unnecessary_wraps)]
pub(crate) fn build_like_expr(
    lhs: SimpleExpr,
    glob_pattern: &str,
    case_sensitivity: CaseSensitivity,
) -> Result<SimpleExpr, QueryBuildingError> {
    match case_sensitivity {
        CaseSensitivity::Sensitive => {
            let glob = to_sqlite_glob(glob_pattern);
            Ok(lhs.glob(glob))
        }
        CaseSensitivity::Insensitive => {
            let like =
                LikeExpr::new(to_sql_like(&glob_pattern.to_lowercase())).escape(LIKE_ESCAPE_CHAR);
            Ok(sea_query::Func::lower(lhs).like(like))
        }
    }
}

fn to_sqlite_glob(glob: &str) -> String {
    let mut escaped = String::with_capacity(glob.len());
    let mut chars = glob.chars();
    while let Some(ch) = chars.next() {
        match ch {
            LIKE_ESCAPE_CHAR => {
                if let Some(ch) = chars.next() {
                    push_glob_literal(&mut escaped, ch);
                }
            }
            '*' => escaped.push('*'),
            '?' => escaped.push('?'),
            other => push_glob_literal(&mut escaped, other),
        }
    }
    escaped
}

fn push_glob_literal(out: &mut String, ch: char) {
    if matches!(ch, '*' | '?' | '[') {
        out.push('[');
        out.push(ch);
        out.push(']');
    } else {
        out.push(ch);
    }
}

#[cfg(test)]
mod tests {
    use sea_query::{Alias, Asterisk, Query, SqliteQueryBuilder};

    use super::*;
    use crate::test::TestDatabase;

    #[tokio::test]
    #[cfg_attr(
        miri,
        ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
    )]
    async fn with_config_sets_connection_options() {
        use std::time::Duration;

        use crate::config::{SqliteConfig, SqliteJournalMode, SqliteSynchronous};

        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("db.sqlite3").display()
        );
        let config = SqliteConfig::builder()
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(10))
            .build();

        let db = DatabaseSqlite::with_config(&url, &config).await.unwrap();

        // every connection in the pool gets the options, not just the first one
        let mut first = db.db_connection.acquire().await.unwrap();
        let mut second = db.db_connection.acquire().await.unwrap();
        for connection in [&mut first, &mut second] {
            let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
                .fetch_one(&mut **connection)
                .await
                .unwrap();
            let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
                .fetch_one(&mut **connection)
                .await
                .unwrap();
            let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
                .fetch_one(&mut **connection)
                .await
                .unwrap();

            assert_eq!(journal_mode, "wal");
            assert_eq!(synchronous, 1); // NORMAL
            assert_eq!(busy_timeout, 10_000);
        }
    }

    #[tokio::test]
    #[cfg_attr(
        miri,
        ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2`"
    )]
    async fn immediate_transactions_wait_for_each_other() {
        use std::sync::Arc;
        use std::time::Duration;

        use crate::config::{SqliteConfig, SqliteJournalMode, SqliteTransactionBehavior};

        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("db.sqlite3").display()
        );
        let config = SqliteConfig::builder()
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5))
            .transaction_behavior(SqliteTransactionBehavior::Immediate)
            .build();
        let db = Arc::new(DatabaseSqlite::with_config(&url, &config).await.unwrap());
        sqlx::query("CREATE TABLE counts (n INTEGER)")
            .execute(&db.db_connection)
            .await
            .unwrap();

        // Each transaction reads and then writes. When transactions are
        // deferred, the second one to write fails with "database is locked"
        // if the other one committed after it read.
        let read_then_write = |db: Arc<DatabaseSqlite>| async move {
            let mut transaction = db.begin().await?;
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM counts")
                .fetch_one(&mut *transaction)
                .await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
            sqlx::query("INSERT INTO counts (n) VALUES (?)")
                .bind(count)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            Ok::<_, crate::db::DatabaseError>(())
        };
        let (first, second) = tokio::join!(
            read_then_write(Arc::clone(&db)),
            read_then_write(Arc::clone(&db))
        );
        first.unwrap();
        second.unwrap();

        // one after the other: the second saw the first one's row
        let counts: Vec<i64> = sqlx::query_scalar("SELECT n FROM counts ORDER BY n")
            .fetch_all(&db.db_connection)
            .await
            .unwrap();
        assert_eq!(counts, [0, 1]);
    }

    #[test]
    fn test_to_sqlite_glob() {
        assert_eq!(to_sqlite_glob("*foo*"), "*foo*");
        assert_eq!(to_sqlite_glob("f?o"), "f?o");
        assert_eq!(to_sqlite_glob("a\\*b"), "a[*]b");
        assert_eq!(to_sqlite_glob("a\\?b"), "a[?]b");
        assert_eq!(to_sqlite_glob("100[percent]"), "100[[]percent]");
        assert_eq!(to_sqlite_glob("[abc]"), "[[]abc]");
        assert_eq!(to_sqlite_glob("hello world"), "hello world");
        assert_eq!(to_sqlite_glob(""), "");
        assert_eq!(to_sqlite_glob("foo\\"), "foo");
        assert_eq!(to_sqlite_glob("C:\\\\Users"), "C:\\Users");
        assert_eq!(to_sqlite_glob("*foo\\*bar?baz*"), "*foo[*]bar?baz*");
        assert_eq!(to_sqlite_glob("\\*\\?\\["), "[*][?][[]");
        assert_eq!(to_sqlite_glob("café*"), "café*");
        assert_eq!(to_sqlite_glob("日本語?"), "日本語?");
        assert_eq!(to_sqlite_glob("***"), "***");
        assert_eq!(to_sqlite_glob("???"), "???");
    }

    fn assert_where(expr: SimpleExpr, expected_where: &str) {
        let sql = render(expr);
        let expected = format!("SELECT * FROM \"t\" WHERE {expected_where}");
        assert_eq!(sql, expected);
    }

    async fn test_db() -> TestDatabase {
        TestDatabase::new_sqlite().await.unwrap()
    }

    fn col_expr() -> SimpleExpr {
        sea_query::Expr::col(Alias::new("name"))
    }

    fn render(expr: SimpleExpr) -> String {
        Query::select()
            .column(Asterisk)
            .from(Alias::new("t"))
            .and_where(expr)
            .to_string(SqliteQueryBuilder)
    }

    #[cfg_attr(
        miri,
        ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2` on OS `linux`"
    )]
    #[cot::test]
    async fn case_sensitive_uses_glob() {
        let db = test_db().await;
        let expr = db
            .like_expr(col_expr(), "foo*", CaseSensitivity::Sensitive)
            .unwrap();
        assert_where(expr, "\"name\" GLOB 'foo*'");
    }

    #[cfg_attr(
        miri,
        ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2` on OS `linux`"
    )]
    #[cot::test]
    async fn case_sensitive_positional_pattern_translates_question_marks() {
        let db = test_db().await;
        let expr = db
            .like_expr(col_expr(), "f??o", CaseSensitivity::Sensitive)
            .unwrap();
        assert_where(expr, "\"name\" GLOB 'f??o'");
    }

    #[cfg_attr(
        miri,
        ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2` on OS `linux`"
    )]
    #[cot::test]
    async fn case_sensitive_escapes_literal_wildcard_chars_for_glob() {
        let db = test_db().await;
        let expr = db
            .like_expr(col_expr(), "50\\*off", CaseSensitivity::Sensitive)
            .unwrap();
        assert_where(expr, "\"name\" GLOB '50[*]off'");
    }

    #[cfg_attr(
        miri,
        ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2` on OS `linux`"
    )]
    #[cot::test]
    async fn case_insensitive_uses_lower_and_like() {
        let db = test_db().await;
        let expr = db
            .like_expr(col_expr(), "Foo*", CaseSensitivity::Insensitive)
            .unwrap();
        assert_where(expr, "LOWER(\"name\") LIKE 'foo%' ESCAPE '\\'");
    }

    #[cfg_attr(
        miri,
        ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2` on OS `linux`"
    )]
    #[cot::test]
    async fn case_insensitive_pattern_is_lowercased_before_conversion() {
        let db = test_db().await;
        let expr = db
            .like_expr(col_expr(), "README", CaseSensitivity::Insensitive)
            .unwrap();
        assert_where(expr, "LOWER(\"name\") LIKE 'readme' ESCAPE '\\'");
    }

    #[cfg_attr(
        miri,
        ignore = "unsupported operation: can't call foreign function `sqlite3_open_v2` on OS `linux`"
    )]
    #[cot::test]
    async fn case_insensitive_includes_escape_clause() {
        let db = test_db().await;
        let expr = db
            .like_expr(col_expr(), "100\\%off", CaseSensitivity::Insensitive)
            .unwrap();
        assert_where(expr, "LOWER(\"name\") LIKE '100\\%off' ESCAPE '\\'");
    }
}
