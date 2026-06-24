use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::logging::{
    DatabaseOptions, FsyncMode, RedoEvent, SqlDialect, append_binlog, append_error, append_redolog,
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

#[derive(Debug, Clone)]
pub(crate) struct ParsedStatementCache {
    capacity: usize,
    statements: BTreeMap<String, Statement>,
}

impl ParsedStatementCache {
    /// - 中文: 创建一个按容量限制保存已解析语句的缓存对象。
    /// - English: Creates a cache object that stores parsed statements with a capacity limit.
    /// - 中文: `capacity` 为零时表示禁用缓存，后续解析流程会直接落到解析器本身。
    /// - English: A `capacity` of zero means caching is disabled and later parse calls fall straight through to the parser.
    /// - 中文: 返回值只初始化内部存储，不会预热或解析任何 SQL 文本。
    /// - English: The return value only initializes internal storage and does not prewarm or parse any SQL text.
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            statements: BTreeMap::new(),
        }
    }

    /// - 中文: 在缓存命中时复用已解析语句，否则按指定方言解析并写回缓存。
    /// - English: Reuses a parsed statement on cache hits, otherwise parses with the specified dialect and writes the result back into the cache.
    /// - 中文: 输入 SQL 会先做尾部分号和空白裁剪，再作为缓存键参与复用判断。
    /// - English: The input SQL is trimmed for trailing semicolons and whitespace before being used as the cache key.
    /// - 中文: 解析失败会直接返回错误，缓存淘汰采用简单的容量截断策略而不是复杂的 LRU。
    /// - English: Parse failures are returned directly, and cache eviction uses a simple capacity-based truncation strategy instead of a full LRU policy.
    pub(crate) fn parse(&mut self, sql: &str, dialect: SqlDialect) -> Result<Statement> {
        if self.capacity == 0 {
            return parse_sql_with_dialect(sql, dialect);
        }

        let key = sql.trim().trim_end_matches(';').trim().to_string();
        if let Some(statement) = self.statements.get(&key) {
            return Ok(statement.clone());
        }

        let statement = parse_sql_with_dialect(sql, dialect)?;
        if self.statements.len() >= self.capacity {
            if let Some(first_key) = self.statements.keys().next().cloned() {
                self.statements.remove(&first_key);
            }
        }
        self.statements.insert(key, statement.clone());
        Ok(statement)
    }
}

impl Database {
    /// - 中文: 创建一个使用默认运行时选项的内存数据库实例。
    /// - English: Creates an in-memory database instance using the default runtime options.
    /// - 中文: 该构造函数不会访问磁盘路径，适合测试、临时数据和嵌入式内存场景。
    /// - English: This constructor does not touch any disk path and is suited for tests, temporary data, and embedded in-memory scenarios.
    /// - 中文: 返回值会在内部委托给 `memory_with_options`，因此沿用相同的默认配置语义。
    /// - English: The return value delegates internally to `memory_with_options`, so it inherits the same default-configuration semantics.
    pub fn memory() -> Self {
        Self::memory_with_options(DatabaseOptions::default())
    }

    /// - 中文: 使用显式给定的运行时选项创建一个内存数据库实例。
    /// - English: Creates an in-memory database instance using explicitly provided runtime options.
    /// - 中文: 该函数期望传入的配置已经可用于内存场景；若配置非法，会通过内部断言路径触发 panic。
    /// - English: This function expects the provided configuration to be usable for an in-memory scenario; invalid configurations panic through the internal checked path.
    /// - 中文: 它适合调用方已经接受“配置错误即程序错误”的场景；更保守的调用方可使用 `try_memory_with_options`。
    /// - English: It fits callers that accept “invalid configuration is a programmer error”; more defensive callers can use `try_memory_with_options` instead.
    pub fn memory_with_options(options: DatabaseOptions) -> Self {
        Self::try_memory_with_options(options).expect("invalid database options")
    }

    /// - 中文: 校验给定运行时选项后，尝试创建一个内存数据库实例。
    /// - English: Validates the provided runtime options and then tries to create an in-memory database instance.
    /// - 中文: 该入口适合需要显式处理配置错误的场景，而不是把错误升级为 panic。
    /// - English: This entry point is suited for scenarios that need to handle configuration errors explicitly instead of promoting them to a panic.
    /// - 中文: 成功时返回空 catalog 和空事务状态，失败时返回配置校验错误。
    /// - English: On success it returns an empty catalog with no active transaction; on failure it returns a configuration-validation error.
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

    /// - 中文: 使用默认运行时选项打开一个文件后端数据库。
    /// - English: Opens a file-backed database using the default runtime options.
    /// - 中文: 若目标文件不存在或为空，本函数会从空 catalog 启动；若文件存在且有内容，则会尝试解码已有数据库内容。
    /// - English: If the target file does not exist or is empty, this function starts from an empty catalog; if the file exists and has content, it tries to decode the existing database state.
    /// - 中文: 返回值会复用 `open_with_options` 的校验和加载路径，因此具有相同的文件格式与错误语义。
    /// - English: The return value reuses the validation and loading path from `open_with_options`, so it carries the same file-format and error semantics.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, DatabaseOptions::default())
    }

    /// - 中文: 使用显式运行时选项打开或初始化一个文件后端数据库。
    /// - English: Opens or initializes a file-backed database with explicit runtime options.
    /// - 中文: 该函数会先校验 `options`，再按文件是否存在且非空决定加载已有 catalog 还是创建空 catalog。
    /// - English: This function validates `options` first, then decides between loading an existing catalog and creating an empty one based on whether the file exists and is non-empty.
    /// - 中文: 返回值持有目标路径用于后续持久化，加载失败时会直接传播解码或 I/O 错误。
    /// - English: The return value retains the target path for later persistence, and load failures propagate decode or I/O errors directly.
    /// - 中文: 路径所有权会在内部复制为 `PathBuf`，后续持久化语义由该数据库实例独立维护。
    /// - English: Path ownership is copied into an internal `PathBuf`, and later persistence semantics are managed independently by the database instance.
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

    /// - 中文: 执行一条 SQL 并在结束后记录慢查询或错误日志。
    /// - English: Executes one SQL statement and records slow-query or error logs afterward.
    /// - 中文: 该入口负责测量总耗时，并把真实执行委托给 `execute_inner`。
    /// - English: This entry point measures total elapsed time and delegates the actual work to `execute_inner`.
    /// - 中文: 返回原始查询结果，不会因为日志写入尝试而改变执行语义。
    /// - English: It returns the original query result and does not alter execution semantics because of logging attempts.
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

    /// - 中文: 执行核心 SQL 流程并驱动 redo、undo 与 binlog 记录。
    /// - English: Runs the core SQL flow and drives redo, undo, and binlog recording.
    /// - 中文: 该函数先解析语句并判断是否涉及 catalog 变更或事务控制，再按结果写入对应日志。
    /// - English: It parses the statement first, determines whether catalog mutation or transaction control is involved, and then writes the corresponding logs based on the outcome.
    /// - 中文: 返回真实执行结果，日志附加失败会按各自调用点的既定错误传播策略处理。
    /// - English: It returns the real execution result, and log-append failures follow the established propagation rules at each call site.
    /// - 中文: 事务相关日志的 begin、commit 与 abort 语义只覆盖本次调用，不会在函数外持有额外事务所有权。
    /// - English: Transaction log begin, commit, and abort semantics cover only this call and do not retain extra transaction ownership outside the function.
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

    /// - 中文: 使用当前方言和缓存配置解析一条 SQL 语句。
    /// - English: Parses one SQL statement using the current dialect and cache configuration.
    /// - 中文: 该函数只是 `parse_with_cache` 的实例级封装，复用数据库级缓存容量设置。
    /// - English: This function is an instance-level wrapper around `parse_with_cache` and reuses the database-level cache-capacity setting.
    /// - 中文: 成功时返回可执行的语法树，失败时传播解析错误。
    /// - English: On success it returns an executable syntax tree, and on failure it propagates parse errors.
    fn parse_statement(&mut self, sql: &str) -> Result<Statement> {
        parse_with_cache(
            &mut self.statement_cache,
            self.options.cache_capacity,
            sql,
            self.options.sql_dialect,
        )
    }

    /// - 中文: 执行一条已经解析完成的语句。
    /// - English: Executes a statement that has already been parsed.
    /// - 中文: 事务控制语句会分发到专门的 begin、commit 和 rollback 入口，其余语句走 catalog 执行路径。
    /// - English: Transaction-control statements are dispatched to dedicated begin, commit, and rollback paths, while all others use the catalog execution path.
    /// - 中文: 返回各分支的原始执行结果，不重新包装错误。
    /// - English: It returns the original result from each branch and does not rewrap errors.
    fn execute_parsed(&mut self, statement: Statement) -> Result<QueryResult> {
        match statement {
            Statement::Begin => self.begin(),
            Statement::Commit => self.commit(),
            Statement::Rollback => self.rollback(),
            Statement::Explain(statement) => self.explain(*statement),
            statement => self.execute_catalog_statement(statement),
        }
    }

    /// - 中文: 判断当前数据库实例是否处于活动事务中。
    /// - English: Reports whether the current database instance is inside an active transaction.
    /// - 中文: 该检查只查看内存中的事务快照状态，不访问磁盘或日志。
    /// - English: This check only inspects the in-memory transaction snapshot state and does not touch disk or logs.
    /// - 中文: 返回 `true` 表示后续写入会落到事务副本而不是主 catalog。
    /// - English: A `true` result means later writes go to the transaction copy instead of the primary catalog.
    pub fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    /// - 中文: 生成一条语句的执行路径说明结果。
    /// - English: Produces an execution-path explanation result for a statement.
    /// - 中文: 当前仅支持 `SELECT`，并会基于过滤条件推导访问路径与索引名称。
    /// - English: It currently supports only `SELECT` and derives the access path and index name from the filter condition.
    /// - 中文: 不支持的语句类型会返回执行错误而不是回退执行原语句。
    /// - English: Unsupported statement kinds return an execution error instead of falling back to executing the original statement.
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
                Ok(QueryResult::rows(vec![explain_row(
                    table,
                    filter,
                    access_path,
                )]))
            }
            _ => Err(Error::Execution("EXPLAIN only supports SELECT".into())),
        }
    }

    /// - 中文: 执行直接作用于 catalog 的语句，并在需要时处理自动持久化。
    /// - English: Executes a statement that operates on the catalog and handles auto-persistence when needed.
    /// - 中文: 非事务中的变更语句会先克隆当前 catalog 以支持失败回滚，事务中的语句则直接作用于活动 catalog。
    /// - English: Mutating statements outside a transaction clone the current catalog first to support rollback on failure, while statements inside a transaction act directly on the active catalog.
    /// - 中文: 返回语句执行结果，持久化失败时会恢复之前状态并传播错误。
    /// - English: It returns the statement result, and on persistence failure it restores the previous state and propagates the error.
    /// - 中文: 持久化语义只在无活动事务的变更路径上触发，避免提前提交事务内的暂存状态。
    /// - English: Persistence semantics are triggered only for mutating paths without an active transaction, avoiding premature commit of staged transactional state.
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

    /// - 中文: 启动一个新的事务快照。
    /// - English: Starts a new transaction snapshot.
    /// - 中文: 若已有活动事务则返回错误，否则会复制当前主 catalog 作为事务工作副本。
    /// - English: It returns an error when a transaction is already active; otherwise it clones the current primary catalog as the transaction working copy.
    /// - 中文: 成功时返回事务开始消息，不触发磁盘持久化。
    /// - English: On success it returns a transaction-started message and does not trigger disk persistence.
    /// - 中文: 事务副本的所有权保存在 `self.transaction` 中，直到提交或回滚为止。
    /// - English: Ownership of the transaction copy is stored in `self.transaction` until commit or rollback.
    fn begin(&mut self) -> Result<QueryResult> {
        if self.transaction.is_some() {
            return Err(Error::Execution("transaction already active".into()));
        }
        self.transaction = Some(self.catalog.clone());
        Ok(QueryResult::message("transaction started"))
    }

    /// - 中文: 提交当前活动事务并持久化提交后的 catalog。
    /// - English: Commits the current active transaction and persists the committed catalog.
    /// - 中文: 若不存在活动事务则返回错误；存在时会把事务副本替换为主 catalog 再尝试持久化。
    /// - English: It returns an error when no transaction is active; otherwise it swaps the transaction copy into the primary catalog before attempting persistence.
    /// - 中文: 持久化失败时会恢复先前 catalog 并返回错误，成功时返回提交完成消息。
    /// - English: On persistence failure it restores the previous catalog and returns the error; on success it returns a committed message.
    /// - 中文: 该函数在提交成功前不会释放事务语义，确保内存状态与持久化结果保持一致。
    /// - English: This function does not release transaction semantics until persistence succeeds, keeping in-memory state aligned with the durable result.
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

    /// - 中文: 回滚当前活动事务并丢弃事务副本。
    /// - English: Rolls back the current active transaction and discards the transaction copy.
    /// - 中文: 若当前没有活动事务则返回错误，否则清空事务状态并保留主 catalog 不变。
    /// - English: It returns an error when no transaction is active; otherwise it clears the transaction state and leaves the primary catalog unchanged.
    /// - 中文: 成功时返回回滚完成消息，不执行持久化写入。
    /// - English: On success it returns a rolled-back message and performs no persistence write.
    /// - 中文: 事务所有权会在 `take` 后被释放，确保暂存修改不会继续被后续操作访问。
    /// - English: Transaction ownership is released via `take`, ensuring staged mutations are no longer reachable by later operations.
    fn rollback(&mut self) -> Result<QueryResult> {
        if self.transaction.take().is_none() {
            return Err(Error::Execution("no active transaction".into()));
        }
        Ok(QueryResult::message("transaction rolled back"))
    }

    /// - 中文: 返回当前应接受写入的活动 catalog 可变引用。
    /// - English: Returns a mutable reference to the active catalog that should receive writes.
    /// - 中文: 有活动事务时返回事务副本，否则返回主 catalog。
    /// - English: It returns the transaction copy when a transaction is active, otherwise the primary catalog.
    /// - 中文: 该函数只做路由选择，不执行克隆或持久化。
    /// - English: This function only chooses the routing target and does not clone or persist anything.
    /// - 中文: 返回的借用遵循当前事务所有权边界，避免同时暴露主副本和事务副本的可变访问。
    /// - English: The returned borrow follows the current transaction ownership boundary, avoiding simultaneous mutable access to both primary and staged copies.
    pub(crate) fn active_catalog_mut(&mut self) -> &mut Catalog {
        self.transaction.as_mut().unwrap_or(&mut self.catalog)
    }

    /// - 中文: 返回当前对读取可见的活动 catalog 只读引用。
    /// - English: Returns a shared reference to the active catalog visible to reads.
    /// - 中文: 读取在事务中会看到事务副本，在非事务中会看到主 catalog。
    /// - English: Reads see the transaction copy inside a transaction and the primary catalog outside one.
    /// - 中文: 该函数不分配内存，也不改变数据库状态。
    /// - English: This function performs no allocation and does not change database state.
    pub(crate) fn active_catalog(&self) -> &Catalog {
        self.transaction.as_ref().unwrap_or(&self.catalog)
    }

    /// - 中文: 返回主 catalog 的只读引用。
    /// - English: Returns a shared reference to the primary catalog.
    /// - 中文: 该访问器不会切换到事务副本，适合需要观察已提交状态的内部路径。
    /// - English: This accessor never switches to the transaction copy and suits internal paths that need committed state.
    /// - 中文: 返回值只借用现有状态，不引入额外同步或持久化行为。
    /// - English: The return value only borrows existing state and introduces no extra synchronization or persistence behavior.
    pub(crate) fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// - 中文: 返回主 catalog 的可变引用。
    /// - English: Returns a mutable reference to the primary catalog.
    /// - 中文: 该访问器绕过事务路由，适合需要直接操作已提交状态的内部路径。
    /// - English: This accessor bypasses transaction routing and suits internal paths that must edit committed state directly.
    /// - 中文: 它本身不做保护性回滚，调用方需要自行维护一致性。
    /// - English: It does not perform protective rollback by itself, so callers must maintain consistency on their own.
    pub(crate) fn catalog_mut(&mut self) -> &mut Catalog {
        &mut self.catalog
    }

    /// - 中文: 用给定 catalog 替换当前主 catalog，并清空事务状态。
    /// - English: Replaces the current primary catalog with the given one and clears transaction state.
    /// - 中文: 该函数用于提交或恢复路径，调用后活动事务会被视为结束。
    /// - English: This function is used by commit or recovery paths, and any active transaction is considered finished afterward.
    /// - 中文: 它不执行持久化，副作用仅限内存中的状态替换。
    /// - English: It performs no persistence, and its side effect is limited to in-memory state replacement.
    /// - 中文: 新 catalog 的所有权会整体移入数据库实例，旧事务副本会被丢弃。
    /// - English: Ownership of the new catalog moves into the database instance as a whole, and any old transaction copy is discarded.
    pub(crate) fn replace_catalog(&mut self, catalog: Catalog) {
        self.catalog = catalog;
        self.transaction = None;
    }

    /// - 中文: 返回当前数据库的运行时选项引用。
    /// - English: Returns a shared reference to the current database runtime options.
    /// - 中文: 该访问器仅暴露只读配置视图，不会触发重新校验。
    /// - English: This accessor exposes a read-only configuration view and does not trigger revalidation.
    /// - 中文: 返回值可供日志、解析和表运行时路径复用。
    /// - English: The return value can be reused by logging, parsing, and table-runtime paths.
    pub(crate) fn options(&self) -> &DatabaseOptions {
        &self.options
    }

    /// - 中文: 按给定运行时选项把当前主 catalog 持久化到数据库文件。
    /// - English: Persists the current primary catalog to the database file using the given runtime options.
    /// - 中文: 内存数据库会直接返回成功；文件数据库会写入临时文件、同步并原子替换目标文件。
    /// - English: In-memory databases return success immediately; file-backed databases write a temp file, sync it, and atomically replace the target file.
    /// - 中文: 成功返回 `Ok(())`，失败时传播目录创建、写入、重命名或同步错误。
    /// - English: It returns `Ok(())` on success and propagates directory-creation, write, rename, or sync errors on failure.
    /// - 中文: 该持久化过程不接管 `options` 的所有权，但会遵循其中的页大小与刷盘语义。
    /// - English: This persistence flow does not take ownership of `options`, but it follows the page-size and flush semantics defined there.
    pub(crate) fn persist_catalog(&self, options: &DatabaseOptions) -> Result<()> {
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
        for chunk in encoded.as_bytes().chunks(options.page_size) {
            file.write_all(chunk)?;
        }
        sync_file(&file, options.fsync_mode)?;
        drop(file);
        fs::rename(&tmp, path)?;

        if options.fsync_mode != FsyncMode::Never {
            if let Some(parent_file) = path.parent().and_then(|parent| File::open(parent).ok()) {
                let _ = parent_file.sync_all();
            }
        }
        Ok(())
    }

    /// - 中文: 使用数据库自身配置把当前主 catalog 持久化到目标文件。
    /// - English: Persists the current primary catalog to the target file using the database's own configuration.
    /// - 中文: 该函数是 `persist_catalog` 的实例化版本，会读取 `self.options` 中的页大小和同步策略。
    /// - English: This function is the instance-bound variant of `persist_catalog` and reads page-size and sync policy from `self.options`.
    /// - 中文: 成功返回 `Ok(())`，失败时传播底层文件系统错误。
    /// - English: It returns `Ok(())` on success and propagates underlying filesystem errors on failure.
    /// - 中文: 持久化仅覆盖已提交主 catalog，不会把独立事务副本直接写到磁盘。
    /// - English: Persistence covers only the committed primary catalog and does not write an isolated transaction copy directly to disk.
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
/// - 中文: 在测试中以默认表运行时选项执行一条语句。
/// - English: Executes a statement with default table runtime options in tests.
/// - 中文: 该辅助函数减少测试样板代码，并把核心行为委托给 `execute_statement_with_options`。
/// - English: This helper reduces test boilerplate and delegates the core behavior to `execute_statement_with_options`.
/// - 中文: 返回底层执行结果，不增加额外状态管理。
/// - English: It returns the underlying execution result without adding extra state management.
fn execute_statement(catalog: &mut Catalog, statement: Statement) -> Result<QueryResult> {
    execute_statement_with_options(catalog, statement, TableRuntimeOptions::default())
}

/// - 中文: 按给定表运行时选项在 catalog 上执行一条非事务管理语句。
/// - English: Executes a non-transaction-manager statement against a catalog with the given table runtime options.
/// - 中文: 该函数负责 CRUD、建表和建索引分发，并拒绝直接执行事务控制语句。
/// - English: This function dispatches CRUD, create-table, and create-index statements, and rejects direct execution of transaction-control statements.
/// - 中文: 成功时返回相应查询结果，失败时传播执行、校验或索引构建错误。
/// - English: On success it returns the appropriate query result, and on failure it propagates execution, validation, or index-build errors.
/// - 中文: 调用方保留对 `catalog` 的所有权与事务边界控制，本函数只在借用期间修改其内容。
/// - English: The caller retains ownership of `catalog` and control over transaction boundaries, and this function mutates it only for the duration of the borrow.
pub(crate) fn execute_statement_with_options(
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
        Statement::ParsedOnly { kind, sql } => Err(Error::Execution(format!(
            "{} parsed successfully but execution is not implemented yet: {}",
            kind.as_str(),
            sql
        ))),
        Statement::Begin | Statement::Commit | Statement::Rollback | Statement::Explain(_) => {
            Err(Error::Execution(
                "transaction statements must be executed by the transaction manager".into(),
            ))
        }
    }
}

/// - 中文: 把访问路径说明转换为 `EXPLAIN` 结果行。
/// - English: Converts access-path details into one `EXPLAIN` result row.
/// - 中文: 该函数会写入表名、谓词摘要、访问方法和索引名称字段。
/// - English: This function fills in the table name, predicate summary, access method, and index name fields.
/// - 中文: 返回新构造的行对象，不修改外部 catalog 状态。
/// - English: It returns a newly built row object and does not mutate external catalog state.
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
            row.insert(
                "access_method".into(),
                Value::Text("secondary_index".into()),
            );
            row.insert("index_name".into(), Value::Text(index_name));
        }
        AccessPath::FullTextIndex { index_name } => {
            row.insert("access_method".into(), Value::Text("fulltext_index".into()));
            row.insert("index_name".into(), Value::Text(index_name));
        }
    }
    row
}

/// - 中文: 使用调用方提供的缓存和容量限制解析 SQL。
/// - English: Parses SQL using a caller-provided cache and capacity limit.
/// - 中文: 该函数会对 SQL 末尾空白和分号做标准化，再尝试命中或写入缓存。
/// - English: This function normalizes trailing whitespace and semicolons in the SQL before trying a cache hit or insert.
/// - 中文: 解析失败会直接返回错误，容量淘汰使用基于首键的简单截断策略。
/// - English: Parse failures are returned directly, and capacity eviction uses a simple first-key truncation strategy.
/// - 中文: 缓存映射的所有权仍由调用方持有，本函数只在借用期间更新其中条目。
/// - English: Ownership of the cache map remains with the caller, and this function updates entries only during the borrow.
pub(crate) fn parse_with_cache(
    cache: &mut BTreeMap<String, Statement>,
    capacity: usize,
    sql: &str,
    dialect: SqlDialect,
) -> Result<Statement> {
    if capacity == 0 {
        return parse_sql_with_dialect(sql, dialect);
    }

    let key = sql.trim().trim_end_matches(';').trim().to_string();
    if let Some(statement) = cache.get(&key) {
        return Ok(statement.clone());
    }

    let statement = parse_sql_with_dialect(sql, dialect)?;
    if cache.len() >= capacity {
        if let Some(first_key) = cache.keys().next().cloned() {
            cache.remove(&first_key);
        }
    }
    cache.insert(key, statement.clone());
    Ok(statement)
}

/// - 中文: 从数据库运行时选项派生表层运行时选项。
/// - English: Derives table-level runtime options from database runtime options.
/// - 中文: 该转换只挑选表执行需要的字段，例如全文、向量、地理和线程配置。
/// - English: This conversion selects only the fields needed by table execution, such as full-text, vector, geospatial, and worker settings.
/// - 中文: 返回新的轻量配置值，不读取或修改数据库状态。
/// - English: It returns a new lightweight configuration value without reading or mutating database state.
pub(crate) fn table_runtime_options(options: &DatabaseOptions) -> TableRuntimeOptions {
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

    /// - 中文: 为引擎测试构造一个唯一的临时数据库路径。
    /// - English: Builds a unique temporary database path for engine tests.
    /// - 中文: 路径通过进程 ID 与纳秒时间戳组合来降低并发冲突概率。
    /// - English: The path combines the process ID and a nanosecond timestamp to reduce collision risk under concurrent tests.
    /// - 中文: 返回值只构造路径，不会提前创建文件或目录。
    /// - English: The return value only builds the path and does not create files or directories ahead of time.
    fn temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("fsql_{name}_{}_{}.db", std::process::id(), nanos))
    }

    /// - 中文: 为测试准备一个 `users` 表。
    /// - English: Prepares a `users` table for tests.
    /// - 中文: 该辅助函数集中复用同一建表 SQL，避免各测试重复书写。
    /// - English: This helper centralizes reuse of the same create-table SQL and avoids repetition across tests.
    /// - 中文: 建表失败会通过 `expect` 让当前测试立刻失败。
    /// - English: Table-creation failure makes the current test fail immediately through `expect`.
    fn create_users(db: &mut Database) {
        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
            .expect("create users");
    }

    #[test]
    /// - 中文: 验证内存数据库支持基础 CRUD 与事务回滚、提交流程。
    /// - English: Verifies that the in-memory database supports basic CRUD plus rollback and commit flows.
    /// - 中文: 测试覆盖全文索引、更新回滚和删除提交后的可见性。
    /// - English: The test covers full-text indexing, update rollback, and post-commit visibility after deletes.
    /// - 中文: 断言失败会直接终止测试，不引入持久化副作用。
    /// - English: Assertion failures terminate the test directly and introduce no persistence side effects.
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
    /// - 中文: 验证文件数据库关闭后仍能重新打开并读取已持久化数据。
    /// - English: Verifies that a file-backed database can be reopened and still read persisted data.
    /// - 中文: 测试覆盖建表、建索引、插入和重启后的查询路径。
    /// - English: The test covers table creation, index creation, inserts, and the query path after reopening.
    /// - 中文: 结束时会删除临时数据库文件。
    /// - English: It removes the temporary database file at the end.
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
    /// - 中文: 验证文件数据库会自动创建缺失的父目录。
    /// - English: Verifies that a file-backed database creates missing parent directories automatically.
    /// - 中文: 测试通过嵌套路径触发持久化目录创建分支。
    /// - English: The test triggers the persistence branch that creates directories by using a nested path.
    /// - 中文: 结束时会删除生成的临时目录树。
    /// - English: It removes the generated temporary directory tree at the end.
    fn file_database_creates_missing_parent_directories() {
        let base = temp_path("nested_parent");
        let path = base.join("a").join("db.fsql");
        let mut db = Database::open(&path).unwrap();
        create_users(&mut db);
        assert!(path.exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    /// - 中文: 验证没有父目录组件的相对路径也能完成持久化。
    /// - English: Verifies that a relative path without a parent component can still be persisted.
    /// - 中文: 测试聚焦持久化逻辑对空父目录分支的处理。
    /// - English: The test focuses on how the persistence logic handles the empty-parent-directory branch.
    /// - 中文: 结束时会删除生成的数据库文件。
    /// - English: It removes the generated database file at the end.
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
    /// - 中文: 验证事务误用和未知表访问都会返回错误。
    /// - English: Verifies that transaction misuse and unknown-table access both return errors.
    /// - 中文: 测试覆盖非法提交、重复开始事务以及多类缺失表语句。
    /// - English: The test covers invalid commit, repeated begin, and several statement forms against missing tables.
    /// - 中文: 结果通过 `is_err` 断言表达，不产生��久化副作用。
    /// - English: Results are expressed through `is_err` assertions and produce no persistence side effects.
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
    /// - 中文: 验证语法层已接受但执行器暂未实现的 SQL 会明确失败且不改变 catalog。
    /// - English: Verifies that SQL accepted by the language layer but not yet implemented by the executor fails clearly and leaves the catalog unchanged.
    /// - 中文: 测试覆盖复杂查询和 DROP TABLE 两类 ParsedOnly 语句。
    /// - English: The test covers both a complex query and a DROP TABLE ParsedOnly statement.
    /// - 中文: 失败后继续读取原表，确认没有发生假执行或误删除。
    /// - English: It reads the original table after the failures to confirm no fake execution or accidental deletion occurred.
    fn parsed_only_sql_fails_without_mutating_catalog() {
        let mut db = Database::memory();
        create_users(&mut db);
        db.execute("INSERT INTO users VALUES (1, 'Ada', 36)")
            .unwrap();

        let query_error = db
            .execute("SELECT * FROM users WHERE age > 18 ORDER BY name")
            .unwrap_err()
            .to_string();
        assert!(query_error.contains("SELECT parsed successfully"));
        assert!(query_error.contains("not implemented yet"));

        let drop_error = db.execute("DROP TABLE users").unwrap_err().to_string();
        assert!(drop_error.contains("DROP TABLE parsed successfully"));

        assert_eq!(
            db.execute("SELECT name FROM users WHERE id = 1")
                .unwrap()
                .rows[0]
                .get("name"),
            Some(&Value::Text("Ada".into()))
        );
    }

    #[test]
    /// - 中文: 验证 `EXPLAIN` 会报告不同查询谓词对应的访问路径。
    /// - English: Verifies that `EXPLAIN` reports the access path for different query predicates.
    /// - 中文: 测试覆盖主键、二级索引、全文索引和全表扫描分支。
    /// - English: The test covers primary-key, secondary-index, full-text-index, and table-scan branches.
    /// - 中文: 断言聚焦输出行中的访问方法与索引名称字段。
    /// - English: Assertions focus on the access-method and index-name fields in the output row.
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
    /// - 中文: 验证自动提交路径在持久化失败时会回滚内存状态。
    /// - English: Verifies that the auto-commit path rolls back in-memory state when persistence fails.
    /// - 中文: 测试通过阻断父目录来制造写盘失败。
    /// - English: The test manufactures a write failure by blocking the parent directory path.
    /// - 中文: 失败后应看不到半提交的表定义。
    /// - English: After failure, no half-committed table definition should remain visible.
    /// - 中文: 该场景直接验证持久化失败下的原子性语义。
    /// - English: This scenario directly validates atomicity semantics under persistence failure.
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
    /// - 中文: 验证事务提交持久化失败时事务状态会被保留。
    /// - English: Verifies that transaction state is preserved when commit persistence fails.
    /// - 中文: 测试通过无效父目录让提交阶段写盘失败。
    /// - English: The test forces write failure during commit by using an invalid parent directory.
    /// - 中文: 提交失败后仍应保持活动事务，以便调用方继续回滚。
    /// - English: After commit failure, the transaction should remain active so the caller can still roll it back.
    /// - 中文: 该断言覆盖事务所有权和持久化一致性语义。
    /// - English: This assertion covers transaction-ownership and persistence-consistency semantics.
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
    /// - 中文: 验证损坏的数据库文件在打开时会返回错误。
    /// - English: Verifies that corrupted database files return an error when opened.
    /// - 中文: 测试通过写入无效内容覆盖 catalog 解码失败路径。
    /// - English: The test covers the catalog-decode failure path by writing invalid contents.
    /// - 中文: 结束时会删除临时坏文件。
    /// - English: It removes the temporary bad file at the end.
    fn opens_bad_database_files_as_errors() {
        let path = temp_path("bad");
        fs::write(&path, "BAD\n").unwrap();
        assert!(Database::open(&path).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    /// - 中文: 验证运行时选项能够驱动多类执行日志写出。
    /// - English: Verifies that runtime options drive emission of several execution log types.
    /// - 中文: 测试覆盖慢查询、错误、binlog、redo 和 undo 日志内容。
    /// - English: The test covers slow-query, error, binlog, redo, and undo log contents.
    /// - 中文: 结束时会清理生成的临时日志目录。
    /// - English: It cleans up the generated temporary log directory at the end.
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
    /// - 中文: 验证运行时选项既会被校验，也会影响引擎行为。
    /// - English: Verifies that runtime options are both validated and reflected in engine behavior.
    /// - 中文: 测试覆盖页大小、缓存、方言、redo 开关和 fsync 模式等配置分支。
    /// - English: The test covers configuration branches for page size, cache, dialect, redo toggles, and fsync mode.
    /// - 中文: 临时文件会在断言完成后清理。
    /// - English: Temporary files are cleaned up after the assertions complete.
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
    /// - 中文: 验证底层语句执行器会防御性拒绝直接事务语句。
    /// - English: Verifies that the low-level statement executor defensively rejects direct transaction statements.
    /// - 中文: 测试确保事务控制必须由更高层事务管理器处理。
    /// - English: The test ensures transaction control must be handled by the higher-level transaction manager.
    /// - 中文: 结果通过错误断言表达，不修改外部持久化状态。
    /// - English: The result is expressed through an error assertion and does not mutate external persistent state.
    fn execute_statement_handles_direct_transaction_statement_defensively() {
        let mut catalog = Catalog::empty();
        assert!(execute_statement(&mut catalog, Statement::Begin).is_err());
    }

    #[test]
    /// - 中文: 验证重复建表会被底层执行器拒绝。
    /// - English: Verifies that duplicate table creation is rejected by the low-level executor.
    /// - 中文: 测试先成功创建表，再对相同表名执行第二次建表。
    /// - English: The test creates a table successfully first and then issues a second create against the same table name.
    /// - 中文: 结果通过最终的错误断言表达。
    /// - English: The result is expressed through the final error assertion.
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
