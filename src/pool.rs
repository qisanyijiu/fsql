use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::engine::{ParsedStatementCache, execute_statement_with_options, table_runtime_options};
use crate::logging::{DatabaseOptions, RedoEvent, append_binlog, append_error, append_redolog, append_slow_sql, append_undolog};
use crate::sql::ast::Statement;
use crate::storage::{Catalog, RowId, index_key};
use crate::{Database, Error, QueryResult, Result};

#[derive(Clone)]
pub struct ConnectionPool {
    inner: Arc<PoolInner>,
}

pub struct Connection {
    inner: Arc<PoolInner>,
    transaction: Mutex<Option<TransactionState>>,
    statement_cache: Mutex<ParsedStatementCache>,
    released: bool,
}

struct PoolInner {
    shared: Mutex<SharedDatabase>,
    permits: Mutex<usize>,
    available: Condvar,
    max_connections: usize,
    next_transaction_id: AtomicU64,
}

struct SharedDatabase {
    database: Database,
    row_locks: BTreeMap<String, u64>,
}

#[derive(Debug, Clone)]
struct TransactionState {
    id: u64,
    catalog: Catalog,
    locked_rows: BTreeSet<String>,
}

impl ConnectionPool {
    pub fn memory(max_connections: usize) -> Result<Self> {
        Self::from_database(Database::memory(), max_connections)
    }

    pub fn memory_with_options(max_connections: usize, options: DatabaseOptions) -> Result<Self> {
        Self::from_database(Database::memory_with_options(options), max_connections)
    }

    pub fn open(path: impl AsRef<Path>, max_connections: usize) -> Result<Self> {
        Self::from_database(Database::open(path)?, max_connections)
    }

    pub fn open_with_options(
        path: impl AsRef<Path>,
        max_connections: usize,
        options: DatabaseOptions,
    ) -> Result<Self> {
        Self::from_database(Database::open_with_options(path, options)?, max_connections)
    }

    pub fn max_connections(&self) -> usize {
        self.inner.max_connections
    }

    pub fn get(&self) -> Result<Connection> {
        let mut permits = self
            .inner
            .permits
            .lock()
            .map_err(|_| Error::Execution("connection pool lock poisoned".into()))?;
        while *permits == 0 {
            permits = self
                .inner
                .available
                .wait(permits)
                .map_err(|_| Error::Execution("connection pool lock poisoned".into()))?;
        }
        *permits -= 1;
        let capacity = {
            let shared = self.lock_shared()?;
            shared.database.options().cache_capacity
        };
        Ok(Connection {
            inner: Arc::clone(&self.inner),
            transaction: Mutex::new(None),
            statement_cache: Mutex::new(ParsedStatementCache::new(capacity)),
            released: false,
        })
    }

    pub fn try_get(&self) -> Result<Option<Connection>> {
        let mut permits = self
            .inner
            .permits
            .lock()
            .map_err(|_| Error::Execution("connection pool lock poisoned".into()))?;
        if *permits == 0 {
            return Ok(None);
        }
        *permits -= 1;
        let capacity = {
            let shared = self.lock_shared()?;
            shared.database.options().cache_capacity
        };
        Ok(Some(Connection {
            inner: Arc::clone(&self.inner),
            transaction: Mutex::new(None),
            statement_cache: Mutex::new(ParsedStatementCache::new(capacity)),
            released: false,
        }))
    }

    fn from_database(database: Database, max_connections: usize) -> Result<Self> {
        if max_connections == 0 {
            return Err(Error::Execution(
                "connection pool size must be greater than zero".into(),
            ));
        }
        Ok(Self {
            inner: Arc::new(PoolInner {
                shared: Mutex::new(SharedDatabase {
                    database,
                    row_locks: BTreeMap::new(),
                }),
                permits: Mutex::new(max_connections),
                available: Condvar::new(),
                max_connections,
                next_transaction_id: AtomicU64::new(1),
            }),
        })
    }

    fn lock_shared(&self) -> Result<MutexGuard<'_, SharedDatabase>> {
        self.inner
            .shared
            .lock()
            .map_err(|_| Error::Execution("database lock poisoned".into()))
    }
}

impl Connection {
    pub fn execute(&self, sql: &str) -> Result<QueryResult> {
        let started = std::time::Instant::now();
        let result = self.execute_inner(sql);
        let elapsed = started.elapsed();

        let options = match self.inner.shared.lock() {
            Ok(shared) => shared.database.options().clone(),
            Err(_) => return Err(Error::Execution("database lock poisoned".into())),
        };
        match &result {
            Ok(_) => append_slow_sql(&options, sql, elapsed),
            Err(error) => append_error(&options, sql, &error.to_string()),
        }
        result
    }

    fn execute_inner(&self, sql: &str) -> Result<QueryResult> {
        let (statement, options) = {
            let shared = self
                .inner
                .shared
                .lock()
                .map_err(|_| Error::Execution("database lock poisoned".into()))?;
            let dialect = shared.database.options().sql_dialect;
            let options = shared.database.options().clone();
            drop(shared);
            let mut cache = self
                .statement_cache
                .lock()
                .map_err(|_| Error::Execution("statement cache lock poisoned".into()))?;
            let statement = cache.parse(sql, dialect)?;
            (statement, options)
        };

        let mut transaction = self
            .transaction
            .lock()
            .map_err(|_| Error::Execution("transaction lock poisoned".into()))?;

        let mutates = statement.mutates_catalog();
        let transaction_control = matches!(statement, Statement::Begin | Statement::Commit | Statement::Rollback);
        let commits = matches!(statement, Statement::Commit);

        if mutates {
            let shared = self
                .inner
                .shared
                .lock()
                .map_err(|_| Error::Execution("database lock poisoned".into()))?;
            if options.undolog_path.is_some() {
                let snapshot = if let Some(tx) = transaction.as_ref() {
                    tx.catalog.encode()
                } else {
                    shared.database.active_catalog().encode()
                };
                append_undolog(&options, sql, &snapshot)?;
            }
            append_redolog(&options, RedoEvent::Begin, sql)?;
        } else if commits {
            append_redolog(&options, RedoEvent::Begin, sql)?;
        }

        let result = self.execute_statement(statement, &options, &mut transaction);
        match &result {
            Ok(_) => {
                if mutates || transaction_control {
                    append_binlog(&options, sql)?;
                }
                if mutates || commits {
                    append_redolog(&options, RedoEvent::Commit, sql)?;
                }
            }
            Err(_) => {
                if mutates || commits {
                    let _ = append_redolog(&options, RedoEvent::Abort, sql);
                }
            }
        }
        result
    }

    fn execute_statement(
        &self,
        statement: Statement,
        options: &DatabaseOptions,
        transaction: &mut Option<TransactionState>,
    ) -> Result<QueryResult> {
        match statement {
            Statement::Begin => self.begin(transaction),
            Statement::Commit => self.commit(options, transaction),
            Statement::Rollback => self.rollback(transaction),
            statement => self.execute_catalog_statement(statement, options, transaction),
        }
    }

    fn begin(&self, transaction: &mut Option<TransactionState>) -> Result<QueryResult> {
        if transaction.is_some() {
            return Err(Error::Execution("transaction already active".into()));
        }
        let shared = self
            .inner
            .shared
            .lock()
            .map_err(|_| Error::Execution("database lock poisoned".into()))?;
        let id = self.inner.next_transaction_id.fetch_add(1, Ordering::Relaxed);
        *transaction = Some(TransactionState {
            id,
            catalog: shared.database.active_catalog().clone(),
            locked_rows: BTreeSet::new(),
        });
        Ok(QueryResult::message("transaction started"))
    }

    fn commit(
        &self,
        options: &DatabaseOptions,
        transaction: &mut Option<TransactionState>,
    ) -> Result<QueryResult> {
        let Some(state) = transaction.take() else {
            return Err(Error::Execution("no active transaction".into()));
        };

        let TransactionState {
            id,
            catalog,
            locked_rows,
        } = state;

        let mut shared = self
            .inner
            .shared
            .lock()
            .map_err(|_| Error::Execution("database lock poisoned".into()))?;
        let previous = shared.database.catalog().clone();
        let merged = merge_catalogs(shared.database.catalog(), &catalog, &locked_rows)?;
        shared.database.replace_catalog(merged);
        if let Err(error) = shared.database.persist_catalog(options) {
            shared.database.replace_catalog(previous);
            self.release_locks(
                &mut shared.row_locks,
                &TransactionState {
                    id,
                    catalog: Catalog::empty(),
                    locked_rows,
                },
            );
            return Err(error);
        }
        self.release_locks(
            &mut shared.row_locks,
            &TransactionState {
                id,
                catalog: Catalog::empty(),
                locked_rows,
            },
        );
        Ok(QueryResult::message("transaction committed"))
    }

    fn rollback(&self, transaction: &mut Option<TransactionState>) -> Result<QueryResult> {
        let Some(state) = transaction.take() else {
            return Err(Error::Execution("no active transaction".into()));
        };
        let mut shared = self
            .inner
            .shared
            .lock()
            .map_err(|_| Error::Execution("database lock poisoned".into()))?;
        self.release_locks(&mut shared.row_locks, &state);
        Ok(QueryResult::message("transaction rolled back"))
    }

    fn execute_catalog_statement(
        &self,
        statement: Statement,
        options: &DatabaseOptions,
        transaction: &mut Option<TransactionState>,
    ) -> Result<QueryResult> {
        if is_ddl_statement(&statement) {
            let shared = self
                .inner
                .shared
                .lock()
                .map_err(|_| Error::Execution("database lock poisoned".into()))?;
            if transaction.is_some() {
                return Err(Error::Execution("ddl is not allowed inside an active transaction".into()));
            }
            if !shared.row_locks.is_empty() {
                return Err(Error::Execution("ddl blocked by active dml transactions".into()));
            }
            drop(shared);
        }

        if let Some(state) = transaction.as_mut() {
            self.lock_statement_rows(&statement, state, options)?;
            return execute_statement_with_options(
                &mut state.catalog,
                statement,
                table_runtime_options(options),
            );
        }

        let mut shared = self
            .inner
            .shared
            .lock()
            .map_err(|_| Error::Execution("database lock poisoned".into()))?;
        let before = shared.database.catalog().clone();
        let result = execute_statement_with_options(
            shared.database.catalog_mut(),
            statement,
            table_runtime_options(options),
        );
        match result {
            Ok(result) => {
                if let Err(error) = shared.database.persist_catalog(options) {
                    shared.database.replace_catalog(before);
                    return Err(error);
                }
                Ok(result)
            }
            Err(error) => {
                shared.database.replace_catalog(before);
                Err(error)
            }
        }
    }

    fn lock_statement_rows(
        &self,
        statement: &Statement,
        transaction: &mut TransactionState,
        options: &DatabaseOptions,
    ) -> Result<()> {
        let mut shared = self
            .inner
            .shared
            .lock()
            .map_err(|_| Error::Execution("database lock poisoned".into()))?;
        let rows = rows_to_lock(&transaction.catalog, statement, table_runtime_options(options))?;
        for row in rows {
            match shared.row_locks.get(&row) {
                Some(owner) if *owner != transaction.id => {
                    return Err(Error::Execution(format!("row lock conflict on {row}")));
                }
                _ => {
                    shared.row_locks.insert(row.clone(), transaction.id);
                    transaction.locked_rows.insert(row);
                }
            }
        }
        reserve_insert_row_id(&mut shared, transaction, statement)?;
        Ok(())
    }

    fn release_locks(
        &self,
        row_locks: &mut BTreeMap<String, u64>,
        transaction: &TransactionState,
    ) {
        for key in &transaction.locked_rows {
            if row_locks.get(key) == Some(&transaction.id) {
                row_locks.remove(key);
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        if let Ok(mut transaction) = self.transaction.lock() {
            if let Some(state) = transaction.take() {
                if let Ok(mut shared) = self.inner.shared.lock() {
                    for key in &state.locked_rows {
                        if shared.row_locks.get(key) == Some(&state.id) {
                            shared.row_locks.remove(key);
                        }
                    }
                }
            }
        }
        if let Ok(mut permits) = self.inner.permits.lock() {
            *permits += 1;
            self.inner.available.notify_one();
        }
    }
}

fn merge_catalogs(
    base: &Catalog,
    staged: &Catalog,
    locked_rows: &BTreeSet<String>,
) -> Result<Catalog> {
    let mut merged = base.clone();
    for (table_name, table) in &staged.tables {
        let Some(target) = merged.tables.get_mut(table_name) else {
            merged.tables.insert(table_name.clone(), table.clone());
            continue;
        };
        for key in locked_rows {
            let prefix = format!("row:{table_name}:");
            if !key.starts_with(&prefix) {
                continue;
            }
            let row_id = key[prefix.len()..]
                .parse::<RowId>()
                .map_err(|_| Error::Execution("invalid locked row id".into()))?;
            match table.rows.get(&row_id) {
                Some(row) => {
                    target.rows.insert(row_id, row.clone());
                }
                None => {
                    target.rows.remove(&row_id);
                }
            }
        }
        for row_id in target.next_row_id..table.next_row_id {
            if let Some(row) = table.rows.get(&row_id) {
                target.rows.insert(row_id, row.clone());
            }
        }
        target.next_row_id = target.next_row_id.max(table.next_row_id);
        target.rebuild_indexes_with_options(crate::storage::TableRuntimeOptions::default())?;
    }
    Ok(merged)
}

fn is_ddl_statement(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::CreateTable { .. } | Statement::CreateIndex { .. }
    )
}

fn row_lock_key(table: &str, row_id: RowId) -> String {
    format!("row:{table}:{row_id}")
}

fn reserve_insert_row_id(
    shared: &mut SharedDatabase,
    transaction: &mut TransactionState,
    statement: &Statement,
) -> Result<()> {
    let Statement::Insert { table, .. } = statement else {
        return Ok(());
    };
    let reserved = {
        let committed = shared
            .database
            .catalog_mut()
            .tables
            .get_mut(table)
            .ok_or_else(|| Error::Execution("unknown table".into()))?;
        let reserved = committed.next_row_id;
        committed.next_row_id += 1;
        reserved
    };
    let staged = transaction
        .catalog
        .tables
        .get_mut(table)
        .ok_or_else(|| Error::Execution("unknown table".into()))?;
    if staged.next_row_id < reserved {
        staged.next_row_id = reserved;
    }
    let row_key = row_lock_key(table, reserved);
    shared.row_locks.insert(row_key.clone(), transaction.id);
    transaction.locked_rows.insert(row_key);
    Ok(())
}

fn insert_lock_key(catalog: &Catalog, table: &str, statement: &Statement) -> Result<String> {
    let Statement::Insert { columns, values, .. } = statement else {
        return Err(Error::Execution("insert lock requested for non-insert statement".into()));
    };
    let table_ref = catalog
        .tables
        .get(table)
        .ok_or_else(|| Error::Execution("unknown table".into()))?;
    if let Some(primary_key) = table_ref.primary_key_column() {
        let target_columns = columns.clone().unwrap_or_else(|| {
            table_ref
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect()
        });
        if let Some((_, value)) = target_columns
            .iter()
            .zip(values.iter())
            .find(|(column, _)| column.as_str() == primary_key)
        {
            return Ok(format!("insert:{table}:{}", index_key(value)?));
        }
    }
    Ok(format!("insert:{table}:next:{}", table_ref.next_row_id))
}

fn rows_to_lock(
    catalog: &Catalog,
    statement: &Statement,
    options: crate::storage::TableRuntimeOptions,
) -> Result<Vec<String>> {
    match statement {
        Statement::Update { table, filter, .. } | Statement::Delete { table, filter } => {
            let table_ref = catalog
                .tables
                .get(table)
                .ok_or_else(|| Error::Execution("unknown table".into()))?;
            let row_ids = table_ref.matching_row_ids_for_lock(filter.clone(), options)?;
            Ok(row_ids
                .into_iter()
                .map(|row_id| row_lock_key(table, row_id))
                .collect())
        }
        Statement::Insert { table, .. } => Ok(vec![insert_lock_key(catalog, table, statement)?]),
        _ => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("fsql_pool_{name}_{}_{}", std::process::id(), nanos))
    }

    #[test]
    fn rejects_zero_sized_pools() {
        assert!(ConnectionPool::memory(0).is_err());
    }

    #[test]
    fn limits_checked_out_connections() {
        let pool = ConnectionPool::memory(1).unwrap();
        assert_eq!(pool.max_connections(), 1);
        let first = pool.get().unwrap();
        assert!(pool.try_get().unwrap().is_none());
        drop(first);
        assert!(pool.try_get().unwrap().is_some());
    }

    #[test]
    fn supports_multithreaded_writes_and_reads() {
        let pool = ConnectionPool::memory(4).unwrap();
        pool.get()
            .unwrap()
            .execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();

        let mut handles = Vec::new();
        for id in 0..16 {
            let pool = pool.clone();
            handles.push(thread::spawn(move || {
                let connection = pool.get().unwrap();
                connection
                    .execute(&format!("INSERT INTO users VALUES ({id}, 'u{id}')"))
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let rows = pool
            .get()
            .unwrap()
            .execute("SELECT * FROM users")
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 16);
        assert!(
            rows.iter()
                .any(|row| row.get("name") == Some(&Value::Text("u7".into())))
        );
    }

    #[test]
    fn different_connections_can_hold_transactions_independently() {
        let pool = ConnectionPool::memory(2).unwrap();
        let setup = pool.get().unwrap();
        setup
            .execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        setup.execute("INSERT INTO users VALUES (1, 'Ada')").unwrap();
        setup.execute("INSERT INTO users VALUES (2, 'Grace')").unwrap();
        drop(setup);

        let first = pool.get().unwrap();
        let second = pool.get().unwrap();
        first.execute("BEGIN").unwrap();
        second.execute("BEGIN").unwrap();
        first
            .execute("UPDATE users SET name = 'Ada-1' WHERE id = 1")
            .unwrap();
        second
            .execute("UPDATE users SET name = 'Grace-2' WHERE id = 2")
            .unwrap();
        first.execute("COMMIT").unwrap();
        second.execute("COMMIT").unwrap();
        drop(first);
        drop(second);

        let rows = pool
            .get()
            .unwrap()
            .execute("SELECT * FROM users")
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.get("name") == Some(&Value::Text("Ada-1".into()))));
        assert!(rows.iter().any(|row| row.get("name") == Some(&Value::Text("Grace-2".into()))));
    }

    #[test]
    fn conflicting_row_updates_fail_with_lock_conflict() {
        let pool = ConnectionPool::memory(2).unwrap();
        let setup = pool.get().unwrap();
        setup.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        setup.execute("INSERT INTO users VALUES (1, 'Ada')").unwrap();
        drop(setup);

        let first = pool.get().unwrap();
        let second = pool.get().unwrap();
        first.execute("BEGIN").unwrap();
        second.execute("BEGIN").unwrap();
        first
            .execute("UPDATE users SET name = 'A' WHERE id = 1")
            .unwrap();
        let error = second
            .execute("UPDATE users SET name = 'B' WHERE id = 1")
            .unwrap_err();
        assert!(error.to_string().contains("row lock conflict"));
    }

    #[test]
    fn concurrent_inserts_with_different_primary_keys_commit() {
        let pool = ConnectionPool::memory(2).unwrap();
        let setup = pool.get().unwrap();
        setup.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        drop(setup);

        let first = pool.get().unwrap();
        let second = pool.get().unwrap();
        first.execute("BEGIN").unwrap();
        second.execute("BEGIN").unwrap();
        first
            .execute("INSERT INTO users VALUES (1, 'Ada')")
            .unwrap();
        second
            .execute("INSERT INTO users VALUES (2, 'Grace')")
            .unwrap();
        first.execute("COMMIT").unwrap();
        second.execute("COMMIT").unwrap();
        drop(first);
        drop(second);

        let rows = pool
            .get()
            .unwrap()
            .execute("SELECT * FROM users")
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn ddl_is_blocked_by_active_transaction_locks() {
        let pool = ConnectionPool::memory(2).unwrap();
        let setup = pool.get().unwrap();
        setup.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        setup.execute("INSERT INTO users VALUES (1, 'Ada')").unwrap();
        drop(setup);

        let first = pool.get().unwrap();
        let second = pool.get().unwrap();
        first.execute("BEGIN").unwrap();
        first
            .execute("UPDATE users SET name = 'A' WHERE id = 1")
            .unwrap();
        let error = second
            .execute("CREATE INDEX users_name ON users(name)")
            .unwrap_err();
        assert!(error.to_string().contains("ddl blocked"));
    }

    #[test]
    fn blocking_get_waits_until_connection_is_released() {
        let pool = ConnectionPool::memory(1).unwrap();
        let first = pool.get().unwrap();
        let (tx, rx) = mpsc::channel();
        let waiter = {
            let pool = pool.clone();
            thread::spawn(move || {
                let connection = pool.get().unwrap();
                tx.send(connection.execute("SELECT * FROM missing").is_err())
                    .unwrap();
            })
        };
        assert!(rx.try_recv().is_err());
        drop(first);
        assert!(rx.recv().unwrap());
        waiter.join().unwrap();
    }

    #[test]
    fn constructors_support_options_and_file_backing() {
        let log_dir = temp_path("logs");
        let options =
            DatabaseOptions::default().with_slow_sql_log(log_dir.join("slow.log"), Duration::ZERO);
        let pool = ConnectionPool::memory_with_options(1, options).unwrap();
        pool.get()
            .unwrap()
            .execute("CREATE TABLE logs (id INTEGER PRIMARY KEY)")
            .unwrap();
        assert!(
            std::fs::read_to_string(log_dir.join("slow.log"))
                .unwrap()
                .contains("CREATE TABLE logs")
        );
        std::fs::remove_dir_all(log_dir).unwrap();

        let path = temp_path("file").with_extension("db");
        let pool = ConnectionPool::open_with_options(&path, 2, DatabaseOptions::default()).unwrap();
        pool.get()
            .unwrap()
            .execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        pool.get()
            .unwrap()
            .execute("INSERT INTO users VALUES (1, 'Ada')")
            .unwrap();
        drop(pool);

        let reopened = ConnectionPool::open(&path, 1).unwrap();
        let rows = reopened
            .get()
            .unwrap()
            .execute("SELECT name FROM users WHERE id = 1")
            .unwrap()
            .rows;
        assert_eq!(rows[0].get("name"), Some(&Value::Text("Ada".into())));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn released_connections_do_not_return_permits_twice() {
        let pool = ConnectionPool::memory(1).unwrap();
        let mut connection = pool.get().unwrap();
        connection.released = true;
        drop(connection);
        assert!(pool.try_get().unwrap().is_none());
    }

    #[test]
    fn poisoned_locks_are_reported_without_panicking() {
        let pool = ConnectionPool::memory(1).unwrap();
        let connection = pool.get().unwrap();
        let poisoner = {
            let pool = pool.clone();
            thread::spawn(move || {
                let _guard = pool.inner.permits.lock().unwrap();
                panic!("poison permits");
            })
        };
        assert!(poisoner.join().is_err());
        drop(connection);
        assert!(pool.try_get().is_err());
        assert!(pool.get().is_err());

        let pool = ConnectionPool::memory(1).unwrap();
        let connection = pool.get().unwrap();
        let poisoner = {
            let inner = Arc::clone(&connection.inner);
            thread::spawn(move || {
                let _guard = inner.shared.lock().unwrap();
                panic!("poison database");
            })
        };
        assert!(poisoner.join().is_err());
        assert!(connection.execute("SELECT * FROM missing").is_err());
    }

    #[test]
    fn waiting_get_reports_poisoned_lock_after_wakeup() {
        let pool = ConnectionPool::memory(1).unwrap();
        let first = pool.get().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let waiter = {
            let pool = pool.clone();
            thread::spawn(move || {
                started_tx.send(()).unwrap();
                pool.get().is_err()
            })
        };
        started_rx.recv().unwrap();
        thread::sleep(Duration::from_millis(50));

        let poisoner = {
            let pool = pool.clone();
            thread::spawn(move || {
                let _guard = pool.inner.permits.lock().unwrap();
                panic!("poison permits while another thread waits");
            })
        };
        assert!(poisoner.join().is_err());
        pool.inner.available.notify_one();
        assert!(waiter.join().unwrap());
        drop(first);
    }
}
