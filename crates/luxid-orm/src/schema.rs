//! Reading the live database's shape.
//!
//! `db:sync` needs to know what columns actually exist so generated factories
//! and form requests can be refreshed from the schema rather than from a guess.
//!
//! Introspection is dialect-specific by nature — SQLite has `PRAGMA`, Postgres
//! has `information_schema` — so the two are handled explicitly rather than
//! through an abstraction that would hide which one ran.

use luxid_core::error::{Error, Result};
use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

use crate::db::Db;

/// One column of a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// The database's own spelling of the type, e.g. `INTEGER`, `text`.
    pub sql_type: String,
    pub nullable: bool,
    pub primary_key: bool,
}

impl Column {
    /// The Rust type this column deserializes into, as a best effort.
    ///
    /// Deliberately conservative: an unrecognised type becomes `String`, which
    /// compiles and can be corrected, rather than a guess that does not.
    pub fn rust_type(&self) -> &'static str {
        let sql = self.sql_type.to_ascii_lowercase();

        // Dates and unrecognised types both land on `String`. That is
        // deliberate: a chrono type would need the right feature and the right
        // import, and guessing wrong produces code that does not compile.
        if sql.contains("smallint") {
            "i16"
        } else if sql.contains("int") && !sql.contains("point") {
            "i64"
        } else if sql.contains("bool") {
            "bool"
        } else if sql.contains("real") || sql.contains("double") || sql.contains("float") {
            "f64"
        } else if sql.contains("json") {
            "serde_json::Value"
        } else {
            "String"
        }
    }

    /// A plausible factory value for this column.
    pub fn sample(&self) -> String {
        match self.rust_type() {
            "bool" => "Set(false)".to_owned(),
            "i16" | "i64" => "Set(0)".to_owned(),
            "f64" => "Set(0.0)".to_owned(),
            "serde_json::Value" => "Set(serde_json::json!({}))".to_owned(),
            _ => format!("Set(\"{}\".to_owned())", self.name),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
}

impl Table {
    /// Columns a factory must supply: everything except nullable columns and
    /// the auto-assigned primary key.
    pub fn required_columns(&self) -> impl Iterator<Item = &Column> {
        self.columns
            .iter()
            .filter(|column| !column.nullable && !column.primary_key)
    }
}

#[derive(Debug, FromQueryResult)]
struct SqliteTable {
    name: String,
}

#[derive(Debug, FromQueryResult)]
struct SqliteColumn {
    name: String,
    #[sea_orm(alias = "type")]
    sql_type: String,
    notnull: i32,
    pk: i32,
}

#[derive(Debug, FromQueryResult)]
struct PostgresColumn {
    table_name: String,
    column_name: String,
    data_type: String,
    is_nullable: String,
    is_primary: bool,
}

impl Db {
    /// Every user table in the database, with its columns.
    ///
    /// Migration bookkeeping tables are excluded — nobody wants a factory for
    /// `seaql_migrations`.
    pub async fn tables(&self) -> Result<Vec<Table>> {
        match self.connection().get_database_backend() {
            DatabaseBackend::Sqlite => self.sqlite_tables().await,
            DatabaseBackend::Postgres => self.postgres_tables().await,
            other => Err(Error::internal(format!(
                "schema introspection is not implemented for {other:?}. \
                 SQLite and Postgres are supported."
            ))),
        }
    }

    async fn sqlite_tables(&self) -> Result<Vec<Table>> {
        let backend = DatabaseBackend::Sqlite;
        let connection = self.connection();

        let names = SqliteTable::find_by_statement(Statement::from_string(
            backend,
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' \
               AND name NOT LIKE 'sqlite_%' \
               AND name <> 'seaql_migrations' \
             ORDER BY name",
        ))
        .all(connection)
        .await
        .map_err(introspection_failed)?;

        let mut tables = Vec::with_capacity(names.len());

        for table in names {
            let columns = SqliteColumn::find_by_statement(Statement::from_string(
                backend,
                // The name is from sqlite_master, not from user input.
                format!("PRAGMA table_info('{}')", table.name.replace('\'', "''")),
            ))
            .all(connection)
            .await
            .map_err(introspection_failed)?;

            tables.push(Table {
                name: table.name,
                columns: columns
                    .into_iter()
                    .map(|column| Column {
                        name: column.name,
                        sql_type: column.sql_type,
                        nullable: column.notnull == 0,
                        primary_key: column.pk > 0,
                    })
                    .collect(),
            });
        }

        Ok(tables)
    }

    async fn postgres_tables(&self) -> Result<Vec<Table>> {
        let rows = PostgresColumn::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT c.table_name,
                    c.column_name,
                    c.data_type,
                    c.is_nullable,
                    COALESCE(pk.is_primary, false) AS is_primary
               FROM information_schema.columns c
               LEFT JOIN (
                    SELECT kcu.table_name, kcu.column_name, true AS is_primary
                      FROM information_schema.table_constraints tc
                      JOIN information_schema.key_column_usage kcu
                        ON kcu.constraint_name = tc.constraint_name
                     WHERE tc.constraint_type = 'PRIMARY KEY'
               ) pk ON pk.table_name = c.table_name AND pk.column_name = c.column_name
              WHERE c.table_schema = 'current_schema'
                 OR c.table_schema = ANY (current_schemas(false))
                AND c.table_name <> 'seaql_migrations'
              ORDER BY c.table_name, c.ordinal_position",
        ))
        .all(self.connection())
        .await
        .map_err(introspection_failed)?;

        let mut tables: Vec<Table> = Vec::new();

        for row in rows {
            if row.table_name == "seaql_migrations" {
                continue;
            }

            let column = Column {
                name: row.column_name,
                sql_type: row.data_type,
                nullable: row.is_nullable.eq_ignore_ascii_case("yes"),
                primary_key: row.is_primary,
            };

            match tables.last_mut() {
                Some(table) if table.name == row.table_name => table.columns.push(column),
                _ => tables.push(Table {
                    name: row.table_name,
                    columns: vec![column],
                }),
            }
        }

        Ok(tables)
    }
}

fn introspection_failed(err: sea_orm::DbErr) -> Error {
    Error::internal(format!("could not read the database schema: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, sql_type: &str, nullable: bool, primary_key: bool) -> Column {
        Column {
            name: name.to_owned(),
            sql_type: sql_type.to_owned(),
            nullable,
            primary_key,
        }
    }

    #[test]
    fn maps_sql_types_conservatively() {
        assert_eq!(column("id", "INTEGER", false, true).rust_type(), "i64");
        assert_eq!(column("n", "bigint", false, false).rust_type(), "i64");
        assert_eq!(column("ok", "BOOLEAN", false, false).rust_type(), "bool");
        assert_eq!(
            column("x", "double precision", false, false).rust_type(),
            "f64"
        );
        assert_eq!(
            column("meta", "jsonb", false, false).rust_type(),
            "serde_json::Value"
        );
        assert_eq!(column("name", "TEXT", false, false).rust_type(), "String");

        // Unknown types fall back to something that compiles.
        assert_eq!(
            column("weird", "geography", false, false).rust_type(),
            "String"
        );
    }

    #[test]
    fn samples_match_their_type() {
        assert_eq!(column("ok", "BOOLEAN", false, false).sample(), "Set(false)");
        assert_eq!(column("n", "INTEGER", false, false).sample(), "Set(0)");
        assert_eq!(
            column("name", "TEXT", false, false).sample(),
            "Set(\"name\".to_owned())"
        );
    }

    #[test]
    fn required_columns_skip_the_key_and_the_nullable() {
        let table = Table {
            name: "users".into(),
            columns: vec![
                column("id", "INTEGER", false, true),
                column("name", "TEXT", false, false),
                column("nickname", "TEXT", true, false),
            ],
        };

        let required: Vec<_> = table.required_columns().map(|c| c.name.as_str()).collect();
        assert_eq!(required, vec!["name"]);
    }

    #[tokio::test]
    async fn reads_a_real_sqlite_schema() {
        use sea_orm::ConnectionTrait;

        let db = Db::in_memory().await.expect("opens");
        db.connection()
            .execute_unprepared(
                "CREATE TABLE users (
                    id       INTEGER PRIMARY KEY AUTOINCREMENT,
                    name     TEXT NOT NULL,
                    nickname TEXT,
                    active   BOOLEAN NOT NULL
                 );
                 CREATE TABLE seaql_migrations (version TEXT PRIMARY KEY, applied_at BIGINT);",
            )
            .await
            .expect("creates schema");

        let tables = db.tables().await.expect("introspects");

        assert_eq!(tables.len(), 1, "migration bookkeeping is excluded");

        let users = &tables[0];
        assert_eq!(users.name, "users");

        let names: Vec<_> = users.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "nickname", "active"]);

        assert!(users.columns[0].primary_key);
        assert!(!users.columns[1].nullable);
        assert!(users.columns[2].nullable, "nickname has no NOT NULL");

        let required: Vec<_> = users.required_columns().map(|c| c.name.as_str()).collect();
        assert_eq!(required, vec!["name", "active"]);
    }
}
