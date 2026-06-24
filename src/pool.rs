use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::engine::{ParsedStatementCache, execute_statement_with_options, table_runtime_options};
use crate::logging::{
    DatabaseOptions, RedoEvent, append_binlog, append_error, append_redolog, append_slow_sql,
    append_undolog,
};
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
    /// - 中文: 创建一个以内存数据库为后端的连接池。
    /// - English: Creates a connection pool backed by an in-memory database.
    /// - 中文: 该构造函数使用数据库默认运行时选项，并把连接数上限交给统一初始化路径校验。
    /// - English: This constructor uses default database runtime options and routes connection-limit validation through the shared initialization path.
    /// - 中文: 成功时返回新的连接池，失败时传播连接池大小校验错误。
    /// - English: On success it returns a new pool, and on failure it propagates pool-size validation errors.
    pub fn memory(max_connections: usize) -> Result<Self> {
        Self::from_database(Database::memory(), max_connections)
    }

    /// - 中文: 使用显式数据库选项创建一个内存后端连接池。
    /// - English: Creates an in-memory connection pool with explicit database options.
    /// - 中文: 该函数先构造底层内存数据库，再复用统一的连接池初始化逻辑。
    /// - English: This function constructs the underlying in-memory database first and then reuses the shared pool initialization logic.
    /// - 中文: 返回值在成功时包含独立的共享数据库状态，失败时传播数据库或池大小错误。
    /// - English: On success the return value contains isolated shared database state, and on failure it propagates database or pool-size errors.
    pub fn memory_with_options(max_connections: usize, options: DatabaseOptions) -> Result<Self> {
        Self::from_database(Database::memory_with_options(options), max_connections)
    }

    /// - 中文: 打开一个文件后端数据库并基于它创建连接池。
    /// - English: Opens a file-backed database and creates a connection pool from it.
    /// - 中文: 该入口使用数据库默认运行时选项，并在数据库成功打开后初始化共享池状态。
    /// - English: This entry point uses default database runtime options and initializes shared pool state after the database opens successfully.
    /// - 中文: 失败时会直接传播数据库打开错误或连接池大小错误。
    /// - English: Failures propagate database-open errors or pool-size validation errors directly.
    pub fn open(path: impl AsRef<Path>, max_connections: usize) -> Result<Self> {
        Self::from_database(Database::open(path)?, max_connections)
    }

    /// - 中文: 使用显式运行时选项打开文件后端数据库并创建连接池。
    /// - English: Opens a file-backed database with explicit runtime options and creates a connection pool.
    /// - 中文: 该函数会先完成数据库校验与加载，再把共享状态包装进池实现。
    /// - English: This function completes database validation and loading first, then wraps the shared state into the pool implementation.
    /// - 中文: 成功时返回新的连接池，失败时传播数据库加载或池大小校验错误。
    /// - English: On success it returns a new pool, and on failure it propagates database-load or pool-size validation errors.
    pub fn open_with_options(
        path: impl AsRef<Path>,
        max_connections: usize,
        options: DatabaseOptions,
    ) -> Result<Self> {
        Self::from_database(Database::open_with_options(path, options)?, max_connections)
    }

    /// - 中文: 返回连接池配置的最大并发连接数。
    /// - English: Returns the maximum concurrent connection count configured for the pool.
    /// - 中文: 该值是初始化时固定的上限，不会随当前借出数量变化。
    /// - English: This value is the fixed limit established at initialization and does not change with the current checkout count.
    /// - 中文: 返回操作只读取元数据，不涉及锁等待。
    /// - English: This read only accesses metadata and does not involve lock waiting.
    pub fn max_connections(&self) -> usize {
        self.inner.max_connections
    }

    /// - 中文: 阻塞直到获取一个可用连接。
    /// - English: Blocks until it acquires an available connection.
    /// - 中文: 该函数会等待连接许可，并为新连接初始化独立的事务状态和语句缓存。
    /// - English: This function waits for a connection permit and initializes independent transaction state and statement cache for the new connection.
    /// - 中文: 成功时返回连接对象，失败时传播池锁或数据库锁中毒错误。
    /// - English: On success it returns a connection object, and on failure it propagates poisoned pool-lock or database-lock errors.
    /// - 中文: 许可的占用语义会持续到连接被释放或析构为止。
    /// - English: Permit ownership remains in effect until the connection is released or dropped.
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

    /// - 中文: 尝试在不阻塞的情况下获取一个可用连接。
    /// - English: Tries to acquire an available connection without blocking.
    /// - 中文: 当没有剩余许可时返回 `Ok(None)`，有许可时会像 `get` 一样初始化连接状态。
    /// - English: It returns `Ok(None)` when no permits remain, and when a permit is available it initializes connection state the same way as `get`.
    /// - 中文: 失败时传播池锁或共享数据库锁的中毒错误。
    /// - English: Failures propagate poisoned pool-lock or shared-database-lock errors.
    /// - 中文: 成功取到连接后同样会占用一个许可直到连接释放。
    /// - English: A successfully acquired connection also owns one permit until the connection is released.
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

    /// - 中文: 从现成数据库实例构造连接池内部状态。
    /// - English: Builds pool internals from an existing database instance.
    /// - 中文: 该函数会校验连接上限必须大于零，并初始化共享数据库、许可计数和事务 ID 生成器。
    /// - English: This function validates that the connection limit is greater than zero and initializes the shared database, permit counter, and transaction ID generator.
    /// - 中文: 成功时返回池对象，失败时返回连接池大小错误。
    /// - English: On success it returns the pool object, and on failure it returns a pool-size error.
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

    /// - 中文: 获取共享数据库状态的互斥锁。
    /// - English: Acquires the mutex guarding shared database state.
    /// - 中文: 该辅助函数统一把锁中毒转换为执行错误，供连接池内部路径复用。
    /// - English: This helper consistently converts lock poisoning into execution errors for internal pool paths.
    /// - 中文: 成功时返回带生命周期约束的锁守卫。
    /// - English: On success it returns a lock guard with the appropriate lifetime constraints.
    fn lock_shared(&self) -> Result<MutexGuard<'_, SharedDatabase>> {
        self.inner
            .shared
            .lock()
            .map_err(|_| Error::Execution("database lock poisoned".into()))
    }
}

impl Connection {
    /// - 中文: 在连接上下文中执行一条 SQL 并记录相关日志。
    /// - English: Executes one SQL statement in the connection context and records related logs.
    /// - 中文: 该入口负责测量耗时并在执行后根据结果追加慢查询或错误日志。
    /// - English: This entry point measures elapsed time and appends slow-query or error logs after execution based on the result.
    /// - 中文: 返回原始查询结果，不会因为日志尝试而改变数据库行为。
    /// - English: It returns the original query result and does not change database behavior because of logging attempts.
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

    /// - 中文: 执行连接级 SQL 主流程，包括解析、事务控制和日志驱动。
    /// - English: Runs the main connection-level SQL flow, including parsing, transaction control, and log driving.
    /// - 中文: 该函数会结合连接私有语句缓存与共享数据库选项解析语句，再依据结果写入 redo、undo 和 binlog。
    /// - English: This function parses the statement using the connection-local statement cache plus shared database options, then writes redo, undo, and binlog records according to the outcome.
    /// - 中文: 返回真实执行结果，失败时保留各锁与事务的一致性约束。
    /// - English: It returns the real execution result and preserves lock and transaction consistency constraints on failure.
    /// - 中文: 事务与锁语义只绑定到当前连接持有的状态，不会越过该连接转移所有权。
    /// - English: Transaction and lock semantics remain bound to state owned by the current connection and are not transferred beyond it.
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
        let transaction_control = matches!(
            statement,
            Statement::Begin | Statement::Commit | Statement::Rollback
        );
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

    /// - 中文: 根据语句类型分发到事务控制或 catalog 执行路径。
    /// - English: Dispatches by statement type to either transaction-control or catalog execution paths.
    /// - 中文: `BEGIN`、`COMMIT` 和 `ROLLBACK` 会进入专门方法，其余语句走普通执行路径。
    /// - English: `BEGIN`, `COMMIT`, and `ROLLBACK` go to dedicated methods, while all other statements use the normal execution path.
    /// - 中文: 返回各分支的原始结果，不额外包装错误。
    /// - English: It returns the original result from each branch and does not add extra error wrapping.
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

    /// - 中文: 为当前连接启动一个新的事务状态。
    /// - English: Starts a new transaction state for the current connection.
    /// - 中文: 若事务已存在则返回错误，否则会复制共享数据库的活动 catalog 并分配事务 ID。
    /// - English: It returns an error when a transaction already exists; otherwise it clones the shared database's active catalog and allocates a transaction ID.
    /// - 中文: 成功时返回事务开始消息，不会立即持久化或申请行锁。
    /// - English: On success it returns a transaction-started message and does not immediately persist or acquire row locks.
    /// - 中文: 新事务状态的所有权保留在当前连接内，直到提交、回滚或连接析构。
    /// - English: Ownership of the new transaction state remains inside the current connection until commit, rollback, or connection drop.
    fn begin(&self, transaction: &mut Option<TransactionState>) -> Result<QueryResult> {
        if transaction.is_some() {
            return Err(Error::Execution("transaction already active".into()));
        }
        let shared = self
            .inner
            .shared
            .lock()
            .map_err(|_| Error::Execution("database lock poisoned".into()))?;
        let id = self
            .inner
            .next_transaction_id
            .fetch_add(1, Ordering::Relaxed);
        *transaction = Some(TransactionState {
            id,
            catalog: shared.database.active_catalog().clone(),
            locked_rows: BTreeSet::new(),
        });
        Ok(QueryResult::message("transaction started"))
    }

    /// - 中文: 提交当前连接的事务状态并把变更合并回共享数据库。
    /// - English: Commits the current connection's transaction state and merges its changes back into the shared database.
    /// - 中文: 该函数会合并锁定行对应的数据，尝试持久化，并在失败时恢复先前 catalog。
    /// - English: This function merges data for locked rows, attempts persistence, and restores the previous catalog on failure.
    /// - 中文: 成功时返回提交消息，失败时传播合并或持久化错误。
    /// - English: On success it returns a committed message, and on failure it propagates merge or persistence errors.
    /// - 中文: 锁与事务所有权只有在提交成功或错误回滚路径释放后才会结束。
    /// - English: Lock and transaction ownership end only after successful commit or after release in the error rollback path.
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

    /// - 中文: 回滚当前连接的事务并释放其持有的行锁。
    /// - English: Rolls back the current connection's transaction and releases its held row locks.
    /// - 中文: 若没有活动事务则返回错误，否则会移除事务状态并清理对应锁记录。
    /// - English: It returns an error when there is no active transaction; otherwise it removes the transaction state and clears the corresponding lock records.
    /// - 中文: 成功时返回回滚消息，不会修改共享数据库的已提交内容。
    /// - English: On success it returns a rollback message and does not modify the committed contents of the shared database.
    /// - 中文: 锁的释放语义与事务所有权终止保持同步。
    /// - English: Lock-release semantics stay synchronized with termination of transaction ownership.
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

    /// - 中文: 执行一条普通 catalog 语句，并处理 DDL 限制与自动持久化。
    /// - English: Executes a regular catalog statement and handles DDL restrictions plus auto-persistence.
    /// - 中文: DDL 会检查活动事务和行锁冲突，事务内 DML 直接作用于事务副本，非事务变更则走共享数据库提交路径。
    /// - English: DDL checks for active transactions and row-lock conflicts, DML inside a transaction acts on the transaction copy, and non-transaction mutations use the shared-database commit path.
    /// - 中文: 返回语句结果，持久化失败时会恢复先前已提交状态。
    /// - English: It returns the statement result and restores the previous committed state when persistence fails.
    /// - 中文: 该函数显式维护锁、事务与持久化边界，避免未提交状态泄露到共享 catalog。
    /// - English: This function explicitly maintains lock, transaction, and persistence boundaries to prevent uncommitted state from leaking into the shared catalog.
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
                return Err(Error::Execution(
                    "ddl is not allowed inside an active transaction".into(),
                ));
            }
            if !shared.row_locks.is_empty() {
                return Err(Error::Execution(
                    "ddl blocked by active dml transactions".into(),
                ));
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

    /// - 中文: 为一条待执行语句锁定其涉及的行或插入槽位。
    /// - English: Locks the rows or insert slot involved in a statement about to execute.
    /// - 中文: 该函数会计算需要锁定的键，检测冲突，并在需要时为插入预留新的行 ID。
    /// - English: This function computes the keys that need locking, detects conflicts, and reserves a new row ID for inserts when needed.
    /// - 中文: 成功返回 `Ok(())`，失败时返回锁冲突或查表错误。
    /// - English: It returns `Ok(())` on success and returns lock-conflict or table-lookup errors on failure.
    /// - 中文: 锁记录会绑定到当前事务 ID，并持续到提交、回滚或连接析构。
    /// - English: Lock records are bound to the current transaction ID and persist until commit, rollback, or connection drop.
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
        let rows = rows_to_lock(
            &transaction.catalog,
            statement,
            table_runtime_options(options),
        )?;
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

    /// - 中文: 释放指定事务持有的所有行锁。
    /// - English: Releases all row locks held by the specified transaction.
    /// - 中文: 该函数只移除确认为当前事务拥有的锁键，避免误删其他事务的锁。
    /// - English: This function removes only lock keys confirmed to belong to the current transaction, avoiding accidental deletion of other transactions' locks.
    /// - 中文: 它不返回错误，适合作为提交、回滚和析构路径的清理步骤。
    /// - English: It returns no error and is suited for cleanup during commit, rollback, and drop paths.
    /// - 中文: 锁释放不会转移所有权，只会终止当前事务对这些键的占用语义。
    /// - English: Lock release does not transfer ownership; it only ends the current transaction's claim over those keys.
    fn release_locks(&self, row_locks: &mut BTreeMap<String, u64>, transaction: &TransactionState) {
        for key in &transaction.locked_rows {
            if row_locks.get(key) == Some(&transaction.id) {
                row_locks.remove(key);
            }
        }
    }
}

impl Drop for Connection {
    /// - 中文: 在连接析构时归还许可并清理未提交事务遗留的锁。
    /// - English: Returns the permit and cleans up locks left by any uncommitted transaction when the connection is dropped.
    /// - 中文: 该析构函数会避免重复归还许可，并在可能时安全释放当前连接拥有的行锁。
    /// - English: This destructor avoids returning the permit twice and safely releases row locks owned by the current connection when possible.
    /// - 中文: 它忽略清理阶段的锁中毒错误，以保证析构过程本身不再传播失败。
    /// - English: It ignores poisoned-lock errors during cleanup so the destructor itself does not propagate failures.
    /// - 中文: 许可与事务锁的所有权都会在析构结束时终止。
    /// - English: Ownership of both the permit and any transaction locks ends when destruction finishes.
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

/// - 中文: 把事务暂存 catalog 中受锁保护的变更合并回已提交 catalog。
/// - English: Merges lock-protected changes from a staged transaction catalog back into the committed catalog.
/// - 中文: 该函数按锁定行覆盖更新或删除，并同步新增行与索引重建。
/// - English: This function applies updates or deletes by locked row, then synchronizes appended rows and rebuilds indexes.
/// - 中文: 成功时返回新的合并 catalog，失败时传播无效行 ID 或索引重建错误。
/// - English: On success it returns a new merged catalog, and on failure it propagates invalid row-ID or index-rebuild errors.
/// - 中文: 合并语义依赖 `locked_rows` 作为所有权边界，避免把未锁定的事务内容错误持久化。
/// - English: Merge semantics rely on `locked_rows` as the ownership boundary, preventing unowned transactional contents from being persisted incorrectly.
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

/// - 中文: 判断一条语句是否属于 DDL。
/// - English: Reports whether a statement is DDL.
/// - 中文: 当前仅把建表和建索引视为 DDL，用于连接池事务限制检查。
/// - English: It currently treats only create-table and create-index statements as DDL for pool transaction restrictions.
/// - 中文: 返回布尔值，不读取或修改共享状态。
/// - English: It returns a boolean and neither reads nor mutates shared state.
fn is_ddl_statement(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::CreateTable { .. } | Statement::CreateIndex { .. }
    )
}

/// - 中文: 生成某个表行对应的锁键字符串。
/// - English: Builds the lock-key string for a row in a table.
/// - 中文: 键格式稳定为 `row:<table>:<row_id>`，供行锁映射统一使用。
/// - English: The format is stably `row:<table>:<row_id>` for shared use in the row-lock map.
/// - 中文: 返回新字符串，不产生外部副作用。
/// - English: It returns a new string and has no external side effects.
fn row_lock_key(table: &str, row_id: RowId) -> String {
    format!("row:{table}:{row_id}")
}

/// - 中文: 为事务中的插入语句预留一个新的行 ID 并登记对应锁。
/// - English: Reserves a new row ID for an insert inside a transaction and registers the corresponding lock.
/// - 中文: 该函数会递增共享已提交表的 `next_row_id`，再同步事务副本中的行 ID 游标。
/// - English: This function increments the shared committed table's `next_row_id` and then synchronizes the row-ID cursor in the transaction copy.
/// - 中文: 成功返回 `Ok(())`，失败时传播未知表等执行错误。
/// - English: It returns `Ok(())` on success and propagates execution errors such as unknown tables on failure.
/// - 中文: 预留出的行 ID 与锁键所有权都会绑定到当前事务，直到事务结束。
/// - English: Ownership of the reserved row ID and lock key is bound to the current transaction until the transaction ends.
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

/// - 中文: 为插入语句计算用于冲突检测的锁键。
/// - English: Computes the lock key used for conflict detection on an insert statement.
/// - 中文: 若表存在主键且插入值中包含该主键，则优先基于主键值生成稳定键，否则退回到 `next_row_id`。
/// - English: When the table has a primary key and the insert includes it, this prefers a stable key derived from that primary-key value; otherwise it falls back to `next_row_id`.
/// - 中文: 成功时返回锁键字符串，失败时传播语句类型或表查找错误。
/// - English: On success it returns the lock-key string, and on failure it propagates statement-kind or table-lookup errors.
/// - 中文: 锁键计算只借用输入数据，不会接管 catalog 或语句的所有权。
/// - English: Lock-key computation only borrows its inputs and does not take ownership of the catalog or statement.
fn insert_lock_key(catalog: &Catalog, table: &str, statement: &Statement) -> Result<String> {
    let Statement::Insert {
        columns, values, ..
    } = statement
    else {
        return Err(Error::Execution(
            "insert lock requested for non-insert statement".into(),
        ));
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

/// - 中文: 计算一条语句在执行前需要锁定的所有键。
/// - English: Computes all keys that must be locked before executing a statement.
/// - 中文: `UPDATE` 和 `DELETE` 会解析匹配到的行 ID，`INSERT` 会生成插入冲突键，其余语句返回空集合。
/// - English: `UPDATE` and `DELETE` resolve matching row IDs, `INSERT` generates an insert-conflict key, and all other statements return an empty set.
/// - 中文: 成功时返回锁键列表，失败时传播查表或过滤求值错误。
/// - English: On success it returns the list of lock keys, and on failure it propagates table-lookup or filter-evaluation errors.
/// - 中文: 这些键定义了后续事务锁语义的边界，但不会在本函数内实际登记所有权。
/// - English: These keys define the boundary of later transaction lock semantics, but this function does not actually register ownership itself.
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

    /// - 中文: 为连接池测试构造唯一的临时路径。
    /// - English: Builds a unique temporary path for connection-pool tests.
    /// - 中文: 路径通过进程 ID 与纳秒时间戳组合来降低冲突概率。
    /// - English: The path combines the process ID and a nanosecond timestamp to reduce collision risk.
    /// - 中文: 返回值只构造路径，不会提前创建文件系统对象。
    /// - English: The return value only builds the path and does not create filesystem objects ahead of time.
    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("fsql_pool_{name}_{}_{}", std::process::id(), nanos))
    }

    #[test]
    /// - 中文: 验证连接池大小为零时会被拒绝。
    /// - English: Verifies that a zero-sized connection pool is rejected.
    /// - 中文: 测试聚焦池初始化阶段的基本参数校验。
    /// - English: The test focuses on basic parameter validation during pool initialization.
    /// - 中文: 结果通过 `is_err` 断言表达。
    /// - English: The result is expressed through an `is_err` assertion.
    fn rejects_zero_sized_pools() {
        assert!(ConnectionPool::memory(0).is_err());
    }

    #[test]
    /// - 中文: 验证连接池会限制同时借出的连接数量。
    /// - English: Verifies that the pool limits the number of simultaneously checked-out connections.
    /// - 中文: 测试覆盖 `get`、`try_get` 和连接释放后的许可归还路径。
    /// - English: The test covers `get`, `try_get`, and the permit-return path after a connection is released.
    /// - 中文: 断言聚焦许可数量语义，不涉及持久化副作用。
    /// - English: Assertions focus on permit semantics and do not involve persistence side effects.
    fn limits_checked_out_connections() {
        let pool = ConnectionPool::memory(1).unwrap();
        assert_eq!(pool.max_connections(), 1);
        let first = pool.get().unwrap();
        assert!(pool.try_get().unwrap().is_none());
        drop(first);
        assert!(pool.try_get().unwrap().is_some());
    }

    #[test]
    /// - 中文: 验证连接池支持多线程并发写入后再读取结果。
    /// - English: Verifies that the connection pool supports multithreaded concurrent writes followed by reads.
    /// - 中文: 测试通过多个线程并发插入记录，再检查最终行数与内容。
    /// - English: The test inserts records concurrently from multiple threads and then checks the final row count and contents.
    /// - 中文: 结果体现共享数据库状态在并发路径上的一致性。
    /// - English: The result demonstrates consistency of shared database state on concurrent paths.
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
    /// - 中文: 验证不同连接可以独立持有并提交各自事务。
    /// - English: Verifies that different connections can hold and commit their own transactions independently.
    /// - 中文: 测试让两个连接分别更新不同记录，再确认提交后结果同时可见。
    /// - English: The test updates different rows from two connections and then confirms both results are visible after commit.
    /// - 中文: 断言聚焦连接级事务隔离与合并语义。
    /// - English: Assertions focus on connection-level transaction isolation and merge semantics.
    /// - 中文: 该场景直接覆盖事务所有权彼此独立的约束。
    /// - English: This scenario directly covers the constraint that transaction ownership remains independent per connection.
    fn different_connections_can_hold_transactions_independently() {
        let pool = ConnectionPool::memory(2).unwrap();
        let setup = pool.get().unwrap();
        setup
            .execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        setup
            .execute("INSERT INTO users VALUES (1, 'Ada')")
            .unwrap();
        setup
            .execute("INSERT INTO users VALUES (2, 'Grace')")
            .unwrap();
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
        assert!(
            rows.iter()
                .any(|row| row.get("name") == Some(&Value::Text("Ada-1".into())))
        );
        assert!(
            rows.iter()
                .any(|row| row.get("name") == Some(&Value::Text("Grace-2".into())))
        );
    }

    #[test]
    /// - 中文: 验证两个事务更新同一行时会触发行锁冲突。
    /// - English: Verifies that two transactions updating the same row trigger a row-lock conflict.
    /// - 中文: 测试先由一个连接获取行锁，再由另一连接尝试冲突更新。
    /// - English: The test has one connection acquire the row lock first and then another connection attempt the conflicting update.
    /// - 中文: 结果通过错误文本包含锁冲突信息来断言。
    /// - English: The result is asserted by checking that the error text contains the lock-conflict message.
    /// - 中文: 该场景直接验证锁所有权边界。
    /// - English: This scenario directly validates lock-ownership boundaries.
    fn conflicting_row_updates_fail_with_lock_conflict() {
        let pool = ConnectionPool::memory(2).unwrap();
        let setup = pool.get().unwrap();
        setup
            .execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        setup
            .execute("INSERT INTO users VALUES (1, 'Ada')")
            .unwrap();
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
    /// - 中文: 验证不同主键的并发插入可以分别提交成功。
    /// - English: Verifies that concurrent inserts with different primary keys can commit successfully.
    /// - 中文: 测试覆盖两个事务各自插入不同主键记录的路径。
    /// - English: The test covers the path where two transactions insert rows with different primary keys.
    /// - 中文: 最终通过读取总行数确认合并结果。
    /// - English: It confirms the merged result by checking the final row count.
    fn concurrent_inserts_with_different_primary_keys_commit() {
        let pool = ConnectionPool::memory(2).unwrap();
        let setup = pool.get().unwrap();
        setup
            .execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
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
    /// - 中文: 验证活动事务持有行锁时 DDL 会被阻塞。
    /// - English: Verifies that DDL is blocked while an active transaction holds row locks.
    /// - 中文: 测试通过一个连接更新行并持锁，另一个连接执行建索引。
    /// - English: The test has one connection update a row and hold the lock while another connection runs create-index.
    /// - 中文: 结果通过错误文本中的阻塞提示断言。
    /// - English: The result is asserted through the blocking hint in the error text.
    /// - 中文: 该场景覆盖锁与 DDL 互斥语义。
    /// - English: This scenario covers the mutual-exclusion semantics between locks and DDL.
    fn ddl_is_blocked_by_active_transaction_locks() {
        let pool = ConnectionPool::memory(2).unwrap();
        let setup = pool.get().unwrap();
        setup
            .execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        setup
            .execute("INSERT INTO users VALUES (1, 'Ada')")
            .unwrap();
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
    /// - 中文: 验证阻塞式 `get` 会等待直到其他连接被释放。
    /// - English: Verifies that blocking `get` waits until another connection is released.
    /// - 中文: 测试通过线程和通道观察等待线程在释放前后行为变化。
    /// - English: The test uses a thread and channel to observe the waiting thread before and after release.
    /// - 中文: 断言聚焦许可等待与唤醒语义。
    /// - English: Assertions focus on permit waiting and wakeup semantics.
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
    /// - 中文: 验证连接池构造器支持显式选项和文件后端。
    /// - English: Verifies that pool constructors support explicit options and file-backed storage.
    /// - 中文: 测试先检查带日志配置的内存池，再验证文件池的重开读取行为。
    /// - English: The test first checks a logging-configured in-memory pool and then validates reopen-and-read behavior for a file-backed pool.
    /// - 中文: 结束时会清理临时日志目录和数据库文件。
    /// - English: It cleans up the temporary log directory and database file at the end.
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
    /// - 中文: 验证已标记释放的连接不会重复归还许可。
    /// - English: Verifies that a connection marked as released does not return its permit twice.
    /// - 中文: 测试直接操纵内部标志以覆盖析构中的保护分支。
    /// - English: The test manipulates the internal flag directly to cover the protective branch inside destruction.
    /// - 中文: 结果通过后续无法再次取到连接来断言。
    /// - English: The result is asserted by confirming that another connection cannot be acquired afterward.
    fn released_connections_do_not_return_permits_twice() {
        let pool = ConnectionPool::memory(1).unwrap();
        let mut connection = pool.get().unwrap();
        connection.released = true;
        drop(connection);
        assert!(pool.try_get().unwrap().is_none());
    }

    #[test]
    /// - 中文: 验证锁中毒会以错误���式报告而不是让调用方 panic。
    /// - English: Verifies that poisoned locks are reported as errors instead of panicking the caller.
    /// - 中文: 测试分别覆盖许可锁和共享数据库锁被其他线程毒化的场景。
    /// - English: The test separately covers cases where another thread poisons the permit lock and the shared database lock.
    /// - 中文: 结果通过 `is_err` 断言表达。
    /// - English: Results are expressed through `is_err` assertions.
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
    /// - 中文: 验证等待中的 `get` 在线程被唤醒后也会报告锁中毒。
    /// - English: Verifies that a waiting `get` still reports lock poisoning after the thread wakes up.
    /// - 中文: 测试让一个线程先进入等待，再由其他线程毒化许可锁并发送唤醒。
    /// - English: The test lets one thread block first, then another thread poisons the permit lock and sends a wakeup.
    /// - 中文: 断言聚焦等待路径上的错误传播。
    /// - English: Assertions focus on error propagation along the waiting path.
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
