use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::logging::{
    DatabaseOptions, FsyncMode, RedoEvent, append_binlog, append_error, append_redolog,
    append_slow_sql, append_undolog, sync_file,
};
use crate::query::QueryResult;
use crate::sql::ast::{Filter, Statement};
use crate::sql::parse_sql_with_dialect;
use crate::storage::{AccessPath, Catalog, Table, TableRuntimeOptions};
use crate::value::{Row, Value};
use crate::{Error, Result};

pub struct Database {
    path: Option<PathBuf>,
    catalog: Catalog,
    transaction: Option<Catalog>,
    options: DatabaseOptions,
    statement_cache: BTreeMap<String, Statement>,
}

impl Database {
    pub fn memory() -> Self {
        Self::memory_with_options(DatabaseOptions::default())
    }

    pub fn memory_with_options(options: DatabaseOptions) -> Self {
        Self::try_memory_with_options(options).expect("invalid database options")
    }

    pub fn try_memory_with_options(options: DatabaseOptions) -> Result<Self> {
        options.validate()?;
        Ok(Self {
            path: None,
            catalog: Catalog::empty(),
            transaction: None,
            options,
            statement_cache: BTreeMap::new(),
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, DatabaseOptions::default())
    }

    pub fn open_with_options(path: impl AsRef<Path>, options: DatabaseOptions) -> Result<Self> {
        options.validate()?;
        let path = path.as_ref().to_path_buf();
        let table_options = table_runtime_options(&options);
        let catalog = if path.exists() && path.metadata()?.len() > 0 {
            Catalog::decode_with_options(&fs::read_to_string(&path)?, table_options)?
        } else {
            Catalog::empty()
        };

        Ok(Self {
            path: Some(path),
            catalog,
            transaction: None,
            options,
            statement_cache: BTreeMap::new(),
        })
    }

    pub fn execute(&mut self, sql: &str) -> Result<QueryResult> {
        let started = Instant::now();
        let result = self.execute_inner(sql);
        let elapsed = started.elapsed();
        match &result {
            Ok(_) => append_slow_sql(&self.options, sql, elapsed),
            Err(error) => append_error(&self.options, sql, &error.to_string()),
        }
        result
    }

    fn execute_inner(&mut self, sql: &str) -> Result<QueryResult> {
        let statement = self.parse_statement(sql)?;
        let mutates = statement.mutates_catalog();
        let transaction_control = matches!(
            &statement,
            Statement::Begin | Statement::Commit | Statement::Rollback
        );
        let commits = matches!(&statement, Statement::Commit);

        if mutates {
            if self.options.undolog_path.is_some() {
                append_undolog(&self.options, sql, &self.active_catalog().encode())?;
            }
            append_redolog(&self.options, RedoEvent::Begin, sql)?;
        } else if commits {
            append_redolog(&self.options, RedoEvent::Begin, sql)?;
        }

        let result = self.execute_parsed(statement);
        match &result {
            Ok(_) => {
                if mutates || transaction_control {
                    append_binlog(&self.options, sql)?;
                }
                if mutates || commits {
                    append_redolog(&self.options, RedoEvent::Commit, sql)?;
                }
            }
            Err(_) => {
                if mutates || commits {
                    let _ = append_redolog(&self.options, RedoEvent::Abort, sql);
                }
            }
        }
        result
    }

    fn parse_statement(&mut self, sql: &str) -> Result<Statement> {
        if self.options.cache_capacity == 0 {
            return parse_sql_with_dialect(sql, self.options.sql_dialect);
        }

        let key = sql.trim().trim_end_matches(';').trim().to_string();
        if let Some(statement) = self.statement_cache.get(&key) {
            return Ok(statement.clone());
        }

        let statement = parse_sql_with_dialect(sql, self.options.sql_dialect)?;
        if self.statement_cache.len() >= self.options.cache_capacity {
            if let Some(first_key) = self.statement_cache.keys().next().cloned() {
                self.statement_cache.remove(&first_key);
            }
        }
        self.statement_cache.insert(key, statement.clone());
        Ok(statement)
    }

    fn execute_parsed(&mut self, statement: Statement) -> Result<QueryResult> {
        match statement {
            Statement::Begin => self.begin(),
            Statement::Commit => self.commit(),
            Statement::Rollback => self.rollback(),
            Statement::Explain(statement) => self.explain(*statement),
            statement => self.execute_catalog_statement(statement),
        }
    }

    pub fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    fn explain(&self, statement: Statement) -> Result<QueryResult> {
        let options = table_runtime_options(&self.options);
        match statement {
            Statement::Select {
                table,
                filter,
                order: _,
                projection: _,
                limit: _,
            } => {
                let table_ref = self
                    .active_catalog()
                    .tables
                    .get(&table)
                    .ok_or_else(|| Error::Execution("unknown table".into()))?;
                let access_path = table_ref.explain_filter(filter.clone(), options)?;
                Ok(QueryResult::rows(vec![explain_row(table, filter, access_path)]))
            }
            _ => Err(Error::Execution("EXPLAIN only supports SELECT".into())),
        }
    }

    fn execute_catalog_statement(&mut self, statement: Statement) -> Result<QueryResult> {
        let mutates = statement.mutates_catalog();
        if mutates && self.transaction.is_none() {
            let before = self.catalog.clone();
            let options = table_runtime_options(&self.options);
            match execute_statement_with_options(&mut self.catalog, statement, options) {
                Ok(result) => {
                    if let Err(error) = self.persist() {
                        self.catalog = before;
                        return Err(error);
                    }
                    Ok(result)
                }
                Err(error) => {
                    self.catalog = before;
                    Err(error)
                }
            }
        } else {
            let options = table_runtime_options(&self.options);
            execute_statement_with_options(self.active_catalog_mut(), statement, options)
        }
    }

    fn begin(&mut self) -> Result<QueryResult> {
        if self.transaction.is_some() {
            return Err(Error::Execution("transaction already active".into()));
        }
        self.transaction = Some(self.catalog.clone());
        Ok(QueryResult::message("transaction started"))
    }

    fn commit(&mut self) -> Result<QueryResult> {
        let Some(transaction) = self.transaction.clone() else {
            return Err(Error::Execution("no active transaction".into()));
        };
        let previous = std::mem::replace(&mut self.catalog, transaction);
        if let Err(error) = self.persist() {
            self.catalog = previous;
            return Err(error);
        }
        self.transaction = None;
        Ok(QueryResult::message("transaction committed"))
    }

    fn rollback(&mut self) -> Result<QueryResult> {
        if self.transaction.take().is_none() {
            return Err(Error::Execution("no active transaction".into()));
        }
        Ok(QueryResult::message("transaction rolled back"))
    }

    fn active_catalog_mut(&mut self) -> &mut Catalog {
        self.transaction.as_mut().unwrap_or(&mut self.catalog)
    }

    fn active_catalog(&self) -> &Catalog {
        self.transaction.as_ref().unwrap_or(&self.catalog)
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }

        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut file = File::create(&tmp)?;
        let encoded = self.catalog.encode();
        for chunk in encoded.as_bytes().chunks(self.options.page_size) {
            file.write_all(chunk)?;
        }
        sync_file(&file, self.options.fsync_mode)?;
        drop(file);
        fs::rename(&tmp, path)?;

        if self.options.fsync_mode != FsyncMode::Never {
            if let Some(parent_file) = path.parent().and_then(|parent| File::open(parent).ok()) {
                let _ = parent_file.sync_all();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
fn execute_statement(catalog: &mut Catalog, statement: Statement) -> Result<QueryResult> {
    execute_statement_with_options(catalog, statement, TableRuntimeOptions::default())
}

fn execute_statement_with_options(
    catalog: &mut Catalog,
    statement: Statement,
    options: TableRuntimeOptions,
) -> Result<QueryResult> {
    match statement {
        Statement::CreateTable { name, columns } => {
            if catalog.tables.contains_key(&name) {
                return Err(Error::Execution(format!("table {name} already exists")));
            }
            catalog
                .tables
                .insert(name.clone(), Table::new(name, columns)?);
            Ok(QueryResult::message("table created"))
        }
        Statement::CreateIndex {
            name,
            table,
            column,
            fulltext,
        } => {
            let table = catalog
                .tables
                .get_mut(&table)
                .ok_or_else(|| Error::Execution("unknown table".into()))?;
            if fulltext {
                table.create_fulltext_index_with_options(name, column, options)?;
            } else {
                table.create_index(name, column)?;
            }
            Ok(QueryResult::message("index created"))
        }
        Statement::Insert {
            table,
            columns,
            values,
        } => {
            let table = catalog
                .tables
                .get_mut(&table)
                .ok_or_else(|| Error::Execution("unknown table".into()))?;
            table.insert_with_options(columns, values, options)?;
            Ok(QueryResult::affected(1, "row inserted"))
        }
        Statement::Select {
            table,
            projection,
            filter,
            order,
            limit,
        } => {
            let table = catalog
                .tables
                .get(&table)
                .ok_or_else(|| Error::Execution("unknown table".into()))?;
            Ok(QueryResult::rows(table.select_with_options(
                projection, filter, order, limit, options,
            )?))
        }
        Statement::Update {
            table,
            assignments,
            filter,
        } => {
            let table = catalog
                .tables
                .get_mut(&table)
                .ok_or_else(|| Error::Execution("unknown table".into()))?;
            let affected = table.update_with_options(assignments, filter, options)?;
            Ok(QueryResult::affected(affected, "row(s) updated"))
        }
        Statement::Delete { table, filter } => {
            let table = catalog
                .tables
                .get_mut(&table)
                .ok_or_else(|| Error::Execution("unknown table".into()))?;
            let affected = table.delete_with_options(filter, options)?;
            Ok(QueryResult::affected(affected, "row(s) deleted"))
        }
        Statement::Begin | Statement::Commit | Statement::Rollback | Statement::Explain(_) => {
            Err(Error::Execution(
                "transaction statements must be executed by the transaction manager".into(),
            ))
        }
    }
}

fn explain_row(table: String, filter: Option<Filter>, access_path: AccessPath) -> Row {
    let mut row = Row::new();
    row.insert("table".into(), Value::Text(table));
    row.insert(
        "predicate".into(),
        Value::Text(match filter {
            Some(Filter::Equals(column, _)) => format!("{column} = ?"),
            Some(Filter::FullText { column, .. }) => format!("MATCH({column}, ...)"),
            Some(Filter::GeoWithin { column, .. }) => format!("GEO_DISTANCE({column}, ...)"),
            None => "none".into(),
        }),
    );
    match access_path {
        AccessPath::TableScan => {
            row.insert("access_method".into(), Value::Text("table_scan".into()));
            row.insert("index_name".into(), Value::Null);
        }
        AccessPath::PrimaryKey => {
            row.insert("access_method".into(), Value::Text("primary_key".into()));
            row.insert("index_name".into(), Value::Text("PRIMARY".into()));
        }
        AccessPath::SecondaryIndex { index_name } => {
            row.insert("access_method".into(), Value::Text("secondary_index".into()));
            row.insert("index_name".into(), Value::Text(index_name));
        }
        AccessPath::FullTextIndex { index_name } => {
            row.insert("access_method".into(), Value::Text("fulltext_index".into()));
            row.insert("index_name".into(), Value::Text(index_name));
        }
    }
    row
}

fn table_runtime_options(options: &DatabaseOptions) -> TableRuntimeOptions {
    TableRuntimeOptions {
        fulltext_tokenizer: options.fulltext_tokenizer,
        vector_index: options.vector_index,
        geo_coordinate_system: options.geo_coordinate_system,
        worker_threads: options.worker_threads,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::ast::{Column, ColumnType};
    use crate::value::Value;
    use crate::{SqlDialect, WalMode};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("fsql_{name}_{}_{}.db", std::process::id(), nanos))
    }

    fn create_users(db: &mut Database) {
        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
            .expect("create users");
    }

    #[test]
    fn memory_database_executes_crud_and_transactions() {
        let mut db = Database::memory();
        create_users(&mut db);
        db.execute("CREATE FULLTEXT INDEX users_name_fts ON users(name)")
            .unwrap();
        db.execute("INSERT INTO users VALUES (1, 'Ada', 36)")
            .unwrap();
        assert_eq!(
            db.execute("SELECT name FROM users WHERE id = 1")
                .unwrap()
                .rows[0]
                .get("name"),
            Some(&Value::Text("Ada".into()))
        );
        db.execute("BEGIN").unwrap();
        assert!(db.in_transaction());
        db.execute("UPDATE users SET name = 'Grace' WHERE id = 1")
            .unwrap();
        db.execute("ROLLBACK").unwrap();
        assert!(!db.in_transaction());
        assert_eq!(
            db.execute("SELECT name FROM users WHERE id = 1")
                .unwrap()
                .rows[0]
                .get("name"),
            Some(&Value::Text("Ada".into()))
        );
        db.execute("BEGIN").unwrap();
        db.execute("DELETE FROM users WHERE id = 1").unwrap();
        db.execute("COMMIT").unwrap();
        assert!(db.execute("SELECT * FROM users").unwrap().rows.is_empty());
    }

    #[test]
    fn file_database_persists_and_reopens() {
        let path = temp_path("persist");
        {
            let mut db = Database::open(&path).unwrap();
            create_users(&mut db);
            db.execute("CREATE INDEX users_age ON users(age)").unwrap();
            db.execute("INSERT INTO users VALUES (1, 'Ada', 36)")
                .unwrap();
        }
        let mut db = Database::open(&path).unwrap();
        assert_eq!(
            db.execute("SELECT name FROM users WHERE age = 36")
                .unwrap()
                .rows
                .len(),
            1
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn file_database_creates_missing_parent_directories() {
        let base = temp_path("nested_parent");
        let path = base.join("a").join("db.fsql");
        let mut db = Database::open(&path).unwrap();
        create_users(&mut db);
        assert!(path.exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn file_database_can_persist_relative_path_without_parent() {
        let path = format!(
            "fsql_relative_{}_{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        );
        let mut db = Database::open(&path).unwrap();
        create_users(&mut db);
        assert!(Path::new(&path).exists());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_transaction_misuse_and_unknown_tables() {
        let mut db = Database::memory();
        assert!(db.execute("COMMIT").is_err());
        assert!(db.execute("ROLLBACK").is_err());
        db.execute("BEGIN").unwrap();
        assert!(db.execute("BEGIN").is_err());
        db.execute("ROLLBACK").unwrap();
        assert!(db.execute("SELECT * FROM missing").is_err());
        assert!(db.execute("CREATE INDEX i ON missing(id)").is_err());
        assert!(db.execute("INSERT INTO missing VALUES (1)").is_err());
        assert!(db.execute("UPDATE missing SET id = 1").is_err());
        assert!(db.execute("DELETE FROM missing").is_err());
    }

    #[test]
    fn explain_reports_access_paths() {
        let mut db = Database::memory();
        create_users(&mut db);
        db.execute("CREATE INDEX users_age ON users(age)").unwrap();
        db.execute("CREATE FULLTEXT INDEX users_name_fts ON users(name)")
            .unwrap();
        db.execute("INSERT INTO users VALUES (1, 'Ada Lovelace', 36)")
            .unwrap();

        let primary = db
            .execute("EXPLAIN SELECT * FROM users WHERE id = 1")
            .unwrap();
        assert_eq!(
            primary.rows[0].get("access_method"),
            Some(&Value::Text("primary_key".into()))
        );
        assert_eq!(
            primary.rows[0].get("index_name"),
            Some(&Value::Text("PRIMARY".into()))
        );

        let secondary = db
            .execute("EXPLAIN SELECT * FROM users WHERE age = 36")
            .unwrap();
        assert_eq!(
            secondary.rows[0].get("access_method"),
            Some(&Value::Text("secondary_index".into()))
        );
        assert_eq!(
            secondary.rows[0].get("index_name"),
            Some(&Value::Text("users_age".into()))
        );

        let fulltext = db
            .execute("EXPLAIN SELECT * FROM users WHERE MATCH(name, 'ada')")
            .unwrap();
        assert_eq!(
            fulltext.rows[0].get("access_method"),
            Some(&Value::Text("fulltext_index".into()))
        );
        assert_eq!(
            fulltext.rows[0].get("index_name"),
            Some(&Value::Text("users_name_fts".into()))
        );

        let scan = db
            .execute("EXPLAIN SELECT * FROM users WHERE name = 'Ada Lovelace'")
            .unwrap();
        assert_eq!(
            scan.rows[0].get("access_method"),
            Some(&Value::Text("table_scan".into()))
        );
        assert_eq!(scan.rows[0].get("index_name"), Some(&Value::Null));
    }

    #[test]
    fn rolls_back_auto_commit_failures() {
        let blocker = temp_path("blocker");
        fs::write(&blocker, "not a directory").unwrap();
        let bad_path = blocker.join("db.fsql");
        let mut db = Database::open(&bad_path).unwrap();
        assert!(
            db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY)")
                .is_err()
        );
        assert!(db.execute("SELECT * FROM users").is_err());
        fs::remove_file(blocker).unwrap();
    }

    #[test]
    fn preserves_transaction_when_commit_persist_fails() {
        let blocker = temp_path("blocker_tx");
        fs::write(&blocker, "not a directory").unwrap();
        let bad_path = blocker.join("db.fsql");
        let mut db = Database::open(&bad_path).unwrap();
        db.execute("BEGIN").unwrap();
        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY)")
            .unwrap();
        assert!(db.execute("COMMIT").is_err());
        assert!(db.in_transaction());
        db.execute("ROLLBACK").unwrap();
        fs::remove_file(blocker).unwrap();
    }

    #[test]
    fn opens_bad_database_files_as_errors() {
        let path = temp_path("bad");
        fs::write(&path, "BAD\n").unwrap();
        assert!(Database::open(&path).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn options_write_execution_logs() {
        let dir = temp_path("exec_logs");
        let options = DatabaseOptions::default()
            .with_slow_sql_log(dir.join("slow.log"), std::time::Duration::ZERO)
            .with_error_log(dir.join("error.log"))
            .with_binlog(dir.join("bin.log"))
            .with_redolog(dir.join("redo.log"))
            .with_undolog(dir.join("undo.log"));
        let mut db = Database::memory_with_options(options);

        create_users(&mut db);
        db.execute("INSERT INTO users VALUES (1, 'Ada', 36)")
            .unwrap();
        db.execute("BEGIN").unwrap();
        db.execute("COMMIT").unwrap();
        assert!(db.execute("INSERT INTO missing VALUES (2)").is_err());
        assert!(db.execute("SELECT * FROM missing").is_err());

        assert!(
            fs::read_to_string(dir.join("slow.log"))
                .unwrap()
                .contains("CREATE TABLE users")
        );
        assert!(
            fs::read_to_string(dir.join("error.log"))
                .unwrap()
                .contains("unknown table")
        );
        assert!(
            fs::read_to_string(dir.join("bin.log"))
                .unwrap()
                .contains("COMMIT")
        );
        let redo = fs::read_to_string(dir.join("redo.log")).unwrap();
        assert!(redo.contains("BEGIN"));
        assert!(redo.contains("COMMIT"));
        assert!(redo.contains("ABORT"));
        assert!(
            fs::read_to_string(dir.join("undo.log"))
                .unwrap()
                .contains("UNDO")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn runtime_options_validate_and_drive_engine_behavior() {
        let path = temp_path("bad_options");
        assert!(
            Database::open_with_options(&path, DatabaseOptions::default().with_page_size(1000))
                .is_err()
        );

        let dir = temp_path("runtime_options");
        let redo_path = dir.join("redo.log");
        let options = DatabaseOptions::default()
            .with_page_size(512)
            .with_cache_capacity(1)
            .with_fsync_mode(FsyncMode::Never)
            .with_sql_dialect(SqlDialect::Sqlite)
            .with_redolog(&redo_path)
            .with_wal_mode(WalMode::Disabled);
        let mut db = Database::try_memory_with_options(options).unwrap();
        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        db.execute("BEGIN IMMEDIATE").unwrap();
        db.execute("INSERT INTO users VALUES (1, 'Ada')").unwrap();
        db.execute("END").unwrap();
        assert_eq!(
            db.execute("SELECT name FROM users WHERE id = 1")
                .unwrap()
                .rows[0]
                .get("name"),
            Some(&Value::Text("Ada".into()))
        );
        assert!(
            db.execute("SELECT * FROM users ORDER BY VECTOR_DISTANCE(name, [1.0])")
                .is_err()
        );
        assert!(!redo_path.exists());

        let file_path = temp_path("fsync_never_file");
        let mut file_db = Database::open_with_options(
            &file_path,
            DatabaseOptions::default()
                .with_page_size(512)
                .with_cache_capacity(0)
                .with_fsync_mode(FsyncMode::Never),
        )
        .unwrap();
        create_users(&mut file_db);
        file_db
            .execute("INSERT INTO users VALUES (1, 'Ada', 36)")
            .unwrap();
        assert!(file_path.exists());
        fs::remove_file(file_path).unwrap();

        let mut cached =
            Database::try_memory_with_options(DatabaseOptions::default().with_cache_capacity(1))
                .unwrap();
        cached
            .execute("CREATE TABLE cache (id INTEGER PRIMARY KEY)")
            .unwrap();
        cached.execute("SELECT * FROM cache").unwrap();
        cached.execute("SELECT id FROM cache").unwrap();
    }

    #[test]
    fn execute_statement_handles_direct_transaction_statement_defensively() {
        let mut catalog = Catalog::empty();
        assert!(execute_statement(&mut catalog, Statement::Begin).is_err());
    }

    #[test]
    fn execute_statement_rejects_duplicate_tables() {
        let mut catalog = Catalog::empty();
        let columns = vec![Column {
            name: "id".into(),
            ty: ColumnType::Integer,
            primary_key: true,
        }];
        execute_statement(
            &mut catalog,
            Statement::CreateTable {
                name: "users".into(),
                columns: columns.clone(),
            },
        )
        .unwrap();
        assert!(
            execute_statement(
                &mut catalog,
                Statement::CreateTable {
                    name: "users".into(),
                    columns,
                },
            )
            .is_err()
        );
    }
}
