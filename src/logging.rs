use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseOptions {
    pub page_size: usize,
    pub cache_capacity: usize,
    pub wal_mode: WalMode,
    pub fsync_mode: FsyncMode,
    pub worker_threads: usize,
    pub sql_dialect: SqlDialect,
    pub fulltext_tokenizer: FullTextTokenizer,
    pub vector_index: VectorIndexOptions,
    pub geo_coordinate_system: GeoCoordinateSystem,
    pub slow_sql_threshold: Option<Duration>,
    pub slow_sql_log_path: Option<PathBuf>,
    pub error_log_path: Option<PathBuf>,
    pub binlog_path: Option<PathBuf>,
    pub redolog_path: Option<PathBuf>,
    pub undolog_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalMode {
    Disabled,
    RedoLog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsyncMode {
    Always,
    DataOnly,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlDialect {
    Fsql,
    Sqlite,
    PostgreSql,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullTextTokenizer {
    Simple,
    Whitespace,
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorMetric {
    Euclidean,
    Cosine,
    DotProduct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorIndexOptions {
    pub metric: VectorMetric,
    pub dimensions: Option<usize>,
    pub ef_search: usize,
    pub m: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeoCoordinateSystem {
    Wgs84,
    Cartesian,
}

impl Default for DatabaseOptions {
    /// - 中文: 构造数据库运行时选项的默认配置集合。
    /// - English: Builds the default set of database runtime options.
    /// - 中文: 该默认值启用内存友好的基础参数，并将日志与持久化路径保持为未配置状态。
    /// - English: These defaults use memory-friendly baseline parameters and leave log and persistence paths unset.
    /// - 中文: 返回的配置可直接用于常规数据库创建流程，后续可通过 builder 方法逐项覆盖。
    /// - English: The returned configuration is ready for normal database creation flows and can be refined later with builder methods.
    fn default() -> Self {
        Self {
            page_size: 4096,
            cache_capacity: 128,
            wal_mode: WalMode::Disabled,
            fsync_mode: FsyncMode::Always,
            worker_threads: 1,
            sql_dialect: SqlDialect::Fsql,
            fulltext_tokenizer: FullTextTokenizer::Simple,
            vector_index: VectorIndexOptions::default(),
            geo_coordinate_system: GeoCoordinateSystem::Wgs84,
            slow_sql_threshold: None,
            slow_sql_log_path: None,
            error_log_path: None,
            binlog_path: None,
            redolog_path: None,
            undolog_path: None,
        }
    }
}

impl Default for VectorIndexOptions {
    /// - 中文: 构造向量索引参数的默认配置。
    /// - English: Builds the default configuration for vector-index parameters.
    /// - 中文: 默认值面向通用 ANN 场景，调用方仍可按数据规模和召回需求覆盖。
    /// - English: The defaults target general ANN scenarios, and callers may still override them for dataset size or recall needs.
    /// - 中文: 返回值只设置静态参数，不会分配索引结构或验证外部数据。
    /// - English: The return value only sets static parameters and does not allocate index structures or validate external data.
    fn default() -> Self {
        Self {
            metric: VectorMetric::Euclidean,
            dimensions: None,
            ef_search: 128,
            m: 16,
        }
    }
}

impl DatabaseOptions {
    /// - 中文: 校验数据库运行时选项是否满足当前实现的基本约束。
    /// - English: Validates whether the database runtime options satisfy the implementation's basic constraints.
    /// - 中文: 会检查页大小、工作线程数以及向量索引参数范围，不会验证路径可写性或磁盘状态。
    /// - English: It checks page size, worker-thread count, and vector-index parameter ranges, but does not verify path writability or disk state.
    /// - 中文: 成功返回 `Ok(())`，失败时返回描述性执行错误。
    /// - English: It returns `Ok(())` on success and a descriptive execution error on failure.
    pub fn validate(&self) -> Result<()> {
        if !self.page_size.is_power_of_two() || !(512..=65_536).contains(&self.page_size) {
            return Err(Error::Execution(
                "page size must be a power of two between 512 and 65536".into(),
            ));
        }
        if self.worker_threads == 0 {
            return Err(Error::Execution(
                "worker thread count must be greater than zero".into(),
            ));
        }
        if self.vector_index.dimensions == Some(0) {
            return Err(Error::Execution(
                "vector dimensions must be greater than zero".into(),
            ));
        }
        if self.vector_index.ef_search == 0 {
            return Err(Error::Execution(
                "vector ef_search must be greater than zero".into(),
            ));
        }
        if self.vector_index.m == 0 {
            return Err(Error::Execution(
                "vector m must be greater than zero".into(),
            ));
        }
        Ok(())
    }

    /// - 中文: 覆盖数据库页大小配置。
    /// - English: Overrides the database page-size setting.
    /// - 中文: `page_size` 需在后续校验时满足 2 的幂且落在支持范围内。
    /// - English: `page_size` must later satisfy the supported power-of-two range during validation.
    /// - 中文: 返回更新后的 builder 值，不立即触发校验或 I/O。
    /// - English: Returns the updated builder value without immediately performing validation or I/O.
    pub fn with_page_size(mut self, page_size: usize) -> Self {
        self.page_size = page_size;
        self
    }

    /// - 中文: 覆盖 SQL 解析缓存容量配置。
    /// - English: Overrides the SQL parse-cache capacity setting.
    /// - 中文: `cache_capacity` 为零时表示禁用该缓存路径。
    /// - English: A `cache_capacity` of zero disables that caching path.
    /// - 中文: 返回新的 builder 值，不会立即清理或创建任何缓存对象。
    /// - English: Returns a new builder value without immediately clearing or creating any cache object.
    pub fn with_cache_capacity(mut self, cache_capacity: usize) -> Self {
        self.cache_capacity = cache_capacity;
        self
    }

    /// - 中文: 覆盖预写日志模式配置。
    /// - English: Overrides the write-ahead logging mode setting.
    /// - 中文: 该设置会影响 redo 路径是否真正写出，但仍需结合日志路径配置才会落盘。
    /// - English: This setting controls whether the redo path actually emits records, but it still needs a configured log path to reach disk.
    /// - 中文: 返回新的 builder 值，不会立刻创建或删除任何日志文件。
    /// - English: Returns a new builder value without immediately creating or deleting any log file.
    pub fn with_wal_mode(mut self, wal_mode: WalMode) -> Self {
        self.wal_mode = wal_mode;
        self
    }

    /// - 中文: 覆盖持久化写入后的同步策略。
    /// - English: Overrides the sync strategy used after persistence writes.
    /// - 中文: 该值只定义后续文件刷盘语义，不会在 builder 阶段执行系统调用。
    /// - English: This value only defines later file-flush semantics and does not issue syscalls during the builder phase.
    /// - 中文: 返回新的 builder 值，副作用仅限配置变更。
    /// - English: Returns a new builder value with configuration changes as the only side effect.
    pub fn with_fsync_mode(mut self, fsync_mode: FsyncMode) -> Self {
        self.fsync_mode = fsync_mode;
        self
    }

    /// - 中文: 覆盖运行时工作线程数量。
    /// - English: Overrides the runtime worker-thread count.
    /// - 中文: 该值必须大于零，并会传递给需要并行执行的表运行时配置。
    /// - English: This value must be greater than zero and is propagated into table runtime options that may execute in parallel.
    /// - 中文: 返回更新后的 builder 值，不会立即生成线程。
    /// - English: Returns the updated builder value and does not spawn threads immediately.
    pub fn with_worker_threads(mut self, worker_threads: usize) -> Self {
        self.worker_threads = worker_threads;
        self
    }

    /// - 中文: 覆盖 SQL 解析方言。
    /// - English: Overrides the SQL parsing dialect.
    /// - 中文: 该值会影响后续语句解析与缓存键对应的语法解释。
    /// - English: This value affects later statement parsing and the syntax interpretation tied to cached statements.
    /// - 中文: 返回新的 builder 值，不会主动重解析已有语句。
    /// - English: Returns a new builder value and does not proactively reparse existing statements.
    pub fn with_sql_dialect(mut self, sql_dialect: SqlDialect) -> Self {
        self.sql_dialect = sql_dialect;
        self
    }

    /// - 中文: 覆盖全文索引分词器配置。
    /// - English: Overrides the full-text index tokenizer configuration.
    /// - 中文: 该设置会进入表运行时选项，用于全文索引构建与查询语义。
    /// - English: This setting flows into table runtime options and shapes full-text index build and query semantics.
    /// - 中文: 返回新的 builder 值，本身不重建已有索引。
    /// - English: Returns a new builder value and does not rebuild existing indexes by itself.
    pub fn with_fulltext_tokenizer(mut self, tokenizer: FullTextTokenizer) -> Self {
        self.fulltext_tokenizer = tokenizer;
        self
    }

    /// - 中文: 覆盖向量索引运行时参数。
    /// - English: Overrides the runtime parameters for vector indexes.
    /// - 中文: 调用方应提供与数据维度和检索策略匹配的配置，非法值会在校验阶段报错。
    /// - English: Callers should supply settings that match data dimensions and retrieval strategy; invalid values fail during validation.
    /// - 中文: 返回新的 builder 值，不会立即创建或调整现有向量索引。
    /// - English: Returns a new builder value without immediately creating or retuning existing vector indexes.
    pub fn with_vector_index(mut self, vector_index: VectorIndexOptions) -> Self {
        self.vector_index = vector_index;
        self
    }

    /// - 中文: 覆盖地理坐标系统配置。
    /// - English: Overrides the geographic coordinate-system configuration.
    /// - 中文: 该值会影响后续地理空间计算和索引解释方式。
    /// - English: This value affects later geospatial calculations and index interpretation.
    /// - 中文: 返回新的 builder 值，不触发数据迁移或即时计算。
    /// - English: Returns a new builder value and does not trigger data migration or immediate computation.
    pub fn with_geo_coordinate_system(mut self, coordinate_system: GeoCoordinateSystem) -> Self {
        self.geo_coordinate_system = coordinate_system;
        self
    }

    /// - 中文: 配置慢 SQL 日志路径与阈值。
    /// - English: Configures the slow-SQL log path and threshold.
    /// - 中文: 只有语句耗时不低于 `threshold` 时才会尝试写日志，`path` 会被保存为拥有所有权的 `PathBuf`。
    /// - English: Logging is attempted only when statement latency is at least `threshold`, and `path` is stored as an owned `PathBuf`.
    /// - 中文: 返回新的 builder 值，不会立即创建日志文件。
    /// - English: Returns a new builder value and does not create the log file immediately.
    pub fn with_slow_sql_log(mut self, path: impl Into<PathBuf>, threshold: Duration) -> Self {
        self.slow_sql_log_path = Some(path.into());
        self.slow_sql_threshold = Some(threshold);
        self
    }

    /// - 中文: 配置错误日志输出路径。
    /// - English: Configures the error-log output path.
    /// - 中文: 该路径仅在后续执行失败时使用，builder 阶段不会探测文件系统。
    /// - English: This path is only used on later execution failures, and the builder phase does not probe the filesystem.
    /// - 中文: 返回新的 builder 值，副作用仅限保存路径。
    /// - English: Returns a new builder value with path storage as the only side effect.
    pub fn with_error_log(mut self, path: impl Into<PathBuf>) -> Self {
        self.error_log_path = Some(path.into());
        self
    }

    /// - 中文: 配置 binlog 输出路径。
    /// - English: Configures the binlog output path.
    /// - 中文: 该日志用于记录变更与事务控制语句，实际写入仍取决于后续执行流程。
    /// - English: This log records mutating and transaction-control statements, while actual writes still depend on later execution flow.
    /// - 中文: 返回新的 builder 值，不会立即触碰磁盘。
    /// - English: Returns a new builder value without touching disk immediately.
    pub fn with_binlog(mut self, path: impl Into<PathBuf>) -> Self {
        self.binlog_path = Some(path.into());
        self
    }

    /// - 中文: 配置 redo 日志路径并启用 redo 模式。
    /// - English: Configures the redo-log path and enables redo mode.
    /// - 中文: 该 builder 会把 `wal_mode` 切换为 `RedoLog`，适合需要崩溃恢复轨迹的场景。
    /// - English: This builder switches `wal_mode` to `RedoLog`, which suits scenarios that need crash-recovery traces.
    /// - 中文: 返回新的 builder 值，不会立即写入 redo 记录。
    /// - English: Returns a new builder value without immediately writing redo records.
    pub fn with_redolog(mut self, path: impl Into<PathBuf>) -> Self {
        self.redolog_path = Some(path.into());
        self.wal_mode = WalMode::RedoLog;
        self
    }

    /// - 中文: 配置 undo 日志输出路径。
    /// - English: Configures the undo-log output path.
    /// - 中文: 该路径用于在后续变更前记录 catalog 快照，是否写入取决于具体执行路径。
    /// - English: This path is used to record catalog snapshots before later mutations, and actual writes depend on the execution path.
    /// - 中文: 返回新的 builder 值，不会立即生成快照文件。
    /// - English: Returns a new builder value without immediately generating snapshot files.
    pub fn with_undolog(mut self, path: impl Into<PathBuf>) -> Self {
        self.undolog_path = Some(path.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedoEvent {
    Begin,
    Commit,
    Abort,
}

/// - 中文: 在 SQL 执行达到慢查询阈值时追加一条慢 SQL 日志。
/// - English: Appends a slow-SQL log entry when execution reaches the configured latency threshold.
/// - 中文: 需要同时配置阈值和日志路径；低于阈值或未配置时直接返回。
/// - English: Both a threshold and log path must be configured; it returns early when the duration is below threshold or logging is unset.
/// - 中文: 该函数会格式化时间戳与 SQL 文本，并忽略内部日志写入失败。
/// - English: This function formats the timestamp and SQL text and intentionally ignores internal log-write failures.
pub(crate) fn append_slow_sql(options: &DatabaseOptions, sql: &str, elapsed: Duration) {
    let Some(threshold) = options.slow_sql_threshold else {
        return;
    };
    if elapsed < threshold {
        return;
    }
    let line = format!(
        "SLOW\t{}\t{}\t{}\n",
        timestamp_millis(),
        elapsed.as_micros(),
        escape(sql)
    );
    let _ = append_optional_line(options, &options.slow_sql_log_path, &line);
}

/// - 中文: 为失败的 SQL 执行追加一条错误日志记录。
/// - English: Appends an error log record for a failed SQL execution.
/// - 中文: 只有配置了错误日志路径时才会尝试写盘，SQL 与错误文本会先做转义。
/// - English: Disk writes are attempted only when an error-log path is configured, and both SQL and error text are escaped first.
/// - 中文: 该函数忽略内部日志写入失败，不改变原始执行错误的传播路径。
/// - English: This function ignores internal log-write failures and does not change propagation of the original execution error.
pub(crate) fn append_error(options: &DatabaseOptions, sql: &str, error: &str) {
    let line = format!(
        "ERROR\t{}\t{}\t{}\n",
        timestamp_millis(),
        escape(sql),
        escape(error)
    );
    let _ = append_optional_line(options, &options.error_log_path, &line);
}

/// - 中文: 追加一条 binlog 记录以描述执行过的语句。
/// - English: Appends a binlog record describing an executed statement.
/// - 中文: 只有配置了 binlog 路径时才会真正写文件，SQL 文本会按日志格式转义。
/// - English: A file is written only when a binlog path is configured, and the SQL text is escaped for the log format.
/// - 中文: 成功返回 `Ok(())`，失败时传播底层文件系统错误。
/// - English: It returns `Ok(())` on success and propagates underlying filesystem errors on failure.
pub(crate) fn append_binlog(options: &DatabaseOptions, sql: &str) -> Result<()> {
    let line = format!("BIN\t{}\t{}\n", timestamp_millis(), escape(sql));
    append_optional_line(options, &options.binlog_path, &line)
}

/// - 中文: 追加一条 redo 日志事件记录。
/// - English: Appends a redo-log event record.
/// - 中文: 当 `wal_mode` 为禁用时直接返回；否则会把事件枚举映射为稳定的文本标记。
/// - English: It returns immediately when `wal_mode` is disabled; otherwise it maps the event enum to a stable text marker.
/// - 中文: 成功返回 `Ok(())`，失败时传播底层日志写入错误。
/// - English: It returns `Ok(())` on success and propagates underlying log-write errors on failure.
/// - 中文: 该函数不接管 SQL 字符串所有权，也不持有跨调用的 WAL 锁状态。
/// - English: This function does not take ownership of the SQL string and does not hold WAL lock state across calls.
pub(crate) fn append_redolog(options: &DatabaseOptions, event: RedoEvent, sql: &str) -> Result<()> {
    if options.wal_mode == WalMode::Disabled {
        return Ok(());
    }
    let event = match event {
        RedoEvent::Begin => "BEGIN",
        RedoEvent::Commit => "COMMIT",
        RedoEvent::Abort => "ABORT",
    };
    let line = format!("REDO\t{}\t{}\t{}\n", timestamp_millis(), event, escape(sql));
    append_optional_line(options, &options.redolog_path, &line)
}

/// - 中文: 在变更前追加一条 undo 日志快照记录。
/// - English: Appends an undo-log snapshot record before a mutation.
/// - 中文: 调用方应提供已编码的 catalog 快照字符串，本函数会将其转为十六进制负载写入日志。
/// - English: The caller should provide an encoded catalog snapshot string, and this function writes it as a hex payload in the log.
/// - 中文: 成功返回 `Ok(())`，失败时传播底层文件写入错误。
/// - English: It returns `Ok(())` on success and propagates underlying file-write errors on failure.
/// - 中文: 该快照参数仅按借用读取，不会延长原始事务状态或持久化对象生命周期。
/// - English: The snapshot parameter is only read by borrow and does not extend the lifetime of the original transaction state or persisted object.
pub(crate) fn append_undolog(
    options: &DatabaseOptions,
    sql: &str,
    catalog_snapshot: &str,
) -> Result<()> {
    let line = format!(
        "UNDO\t{}\t{}\t{}\n",
        timestamp_millis(),
        escape(sql),
        hex(catalog_snapshot.as_bytes())
    );
    append_optional_line(options, &options.undolog_path, &line)
}

/// - 中文: 在可选日志路径存在时追加一行文本。
/// - English: Appends one line of text when an optional log path is present.
/// - 中文: `path` 为空时直接成功返回，`options` 只用于读取同步策略。
/// - English: It returns success immediately when `path` is absent, and `options` is only used to read the sync policy.
/// - 中文: 成功返回 `Ok(())`，失败时传播底层追加写入错误。
/// - English: It returns `Ok(())` on success and propagates underlying append-write errors on failure.
fn append_optional_line(
    options: &DatabaseOptions,
    path: &Option<PathBuf>,
    line: &str,
) -> Result<()> {
    if let Some(path) = path {
        append_line(path, line, options.fsync_mode)?;
    }
    Ok(())
}

/// - 中文: 向目标文件末尾追加一行日志内容。
/// - English: Appends one log line to the end of the target file.
/// - 中文: 若父目录存在需求会先创建目录，然后以追加模式打开文件并写入完整字节串。
/// - English: It creates parent directories when needed, then opens the file in append mode and writes the full byte sequence.
/// - 中文: 成功时会按 `fsync_mode` 刷新文件，失败时传播目录创建、写入或同步错误。
/// - English: On success it syncs the file according to `fsync_mode`; on failure it propagates directory-creation, write, or sync errors.
fn append_line(path: &Path, line: &str, fsync_mode: FsyncMode) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.flush()?;
    sync_file(&file, fsync_mode)?;
    Ok(())
}

/// - 中文: 按给定策略执行文件同步。
/// - English: Synchronizes a file according to the requested policy.
/// - 中文: `Always` 调用 `sync_all`，`DataOnly` 调用 `sync_data`，`Never` 则跳过系统级刷盘。
/// - English: `Always` calls `sync_all`, `DataOnly` calls `sync_data`, and `Never` skips system-level flushing.
/// - 中文: 成功返回 `Ok(())`，失败时传播底层同步系统调用错误。
/// - English: It returns `Ok(())` on success and propagates underlying sync syscall failures on error.
pub(crate) fn sync_file(file: &fs::File, fsync_mode: FsyncMode) -> Result<()> {
    match fsync_mode {
        FsyncMode::Always => file.sync_all()?,
        FsyncMode::DataOnly => file.sync_data()?,
        FsyncMode::Never => {}
    }
    Ok(())
}

/// - 中文: 对日志字段中的反斜杠、制表符和换行符进行转义。
/// - English: Escapes backslashes, tabs, and newlines inside log fields.
/// - 中文: 输入按借用读取，不会修改原始字符串；输出用于构造单行日志协议。
/// - English: The input is read by borrow without modifying the original string, and the output is meant for the single-line log protocol.
/// - 中文: 返回新的转义字符串，副作用仅限分配结果缓冲区。
/// - English: It returns a newly allocated escaped string, with result-buffer allocation as the only side effect.
fn escape(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

/// - 中文: 将字节切片编码为小写十六进制字符串。
/// - English: Encodes a byte slice into a lowercase hexadecimal string.
/// - 中文: 输入按顺序逐字节格式化，适合用于日志中的二进制安全载荷表示。
/// - English: The input is formatted byte by byte in order and is suitable for binary-safe payload representation in logs.
/// - 中文: 返回新的十六进制文本，不会写入外部状态。
/// - English: It returns new hexadecimal text and does not mutate external state.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// - 中文: 读取当前系统时间并转换为 Unix 毫秒时间戳。
/// - English: Reads the current system time and converts it into a Unix millisecond timestamp.
/// - 中文: 当系统时间早于 Unix epoch 时会回退为零时长，避免把时钟错误升级为 panic。
/// - English: When system time is earlier than the Unix epoch, it falls back to a zero duration instead of turning clock skew into a panic.
/// - 中文: 返回无符号毫秒值，不产生外部副作用。
/// - English: It returns an unsigned millisecond value and has no external side effects.
fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// - 中文: 为测试用例生成唯一的临时路径。
    /// - English: Generates a unique temporary path for tests.
    /// - 中文: 路径组合当前进程 ID 和纳秒时间戳，以降低并发测试冲突概率。
    /// - English: The path combines the current process ID and a nanosecond timestamp to reduce collision risk in concurrent tests.
    /// - 中文: 返回值仅构造路径，不会提前创建文件或目录。
    /// - English: The return value only builds the path and does not create files or directories ahead of time.
    fn temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("fsql_log_{name}_{}_{}", std::process::id(), nanos))
    }

    /// - 中文: 验证选项 builder 能正确设置路径和核心字段。
    /// - English: Verifies that the options builder correctly sets paths and core fields.
    /// - 中文: 测试覆盖多类运行时选项组合，并在末尾调用 `validate` 检查整体合法性。
    /// - English: The test covers a mixed set of runtime options and calls `validate` at the end to confirm overall validity.
    /// - 中文: 断言失败会导致测试失败，不产生持久化副作用。
    /// - English: Assertion failures fail the test and do not produce persistent side effects.
    #[test]
    fn options_builder_sets_paths() {
        let vector_index = VectorIndexOptions {
            metric: VectorMetric::Cosine,
            dimensions: Some(3),
            ef_search: 64,
            m: 8,
        };
        let options = DatabaseOptions::default()
            .with_page_size(8192)
            .with_cache_capacity(16)
            .with_wal_mode(WalMode::RedoLog)
            .with_fsync_mode(FsyncMode::DataOnly)
            .with_worker_threads(4)
            .with_sql_dialect(SqlDialect::Sqlite)
            .with_fulltext_tokenizer(FullTextTokenizer::Whitespace)
            .with_vector_index(vector_index)
            .with_geo_coordinate_system(GeoCoordinateSystem::Cartesian)
            .with_slow_sql_log("slow.log", Duration::from_millis(1))
            .with_error_log("error.log")
            .with_binlog("bin.log")
            .with_redolog("redo.log")
            .with_undolog("undo.log");
        assert_eq!(options.page_size, 8192);
        assert_eq!(options.cache_capacity, 16);
        assert_eq!(options.wal_mode, WalMode::RedoLog);
        assert_eq!(options.fsync_mode, FsyncMode::DataOnly);
        assert_eq!(options.worker_threads, 4);
        assert_eq!(options.sql_dialect, SqlDialect::Sqlite);
        assert_eq!(options.fulltext_tokenizer, FullTextTokenizer::Whitespace);
        assert_eq!(options.vector_index, vector_index);
        assert_eq!(
            options.geo_coordinate_system,
            GeoCoordinateSystem::Cartesian
        );
        assert_eq!(options.slow_sql_threshold, Some(Duration::from_millis(1)));
        assert_eq!(options.error_log_path, Some(PathBuf::from("error.log")));
        assert_eq!(options.binlog_path, Some(PathBuf::from("bin.log")));
        assert_eq!(options.redolog_path, Some(PathBuf::from("redo.log")));
        assert_eq!(options.undolog_path, Some(PathBuf::from("undo.log")));
        options.validate().unwrap();
    }

    /// - 中文: 验证核心运行时参数的非法取值会被拒绝。
    /// - English: Verifies that invalid core runtime parameter values are rejected.
    /// - 中文: 测试聚焦页大小、线程数和向量索引参数的边界条件。
    /// - English: The test focuses on boundary conditions for page size, thread count, and vector-index parameters.
    /// - 中文: 结果通过 `is_err` 断言表达，不写入外部状态。
    /// - English: Results are expressed through `is_err` assertions and do not write external state.
    #[test]
    fn validates_core_runtime_options() {
        assert!(
            DatabaseOptions::default()
                .with_page_size(1000)
                .validate()
                .is_err()
        );
        assert!(
            DatabaseOptions::default()
                .with_worker_threads(0)
                .validate()
                .is_err()
        );
        assert!(
            DatabaseOptions::default()
                .with_vector_index(VectorIndexOptions {
                    dimensions: Some(0),
                    ..VectorIndexOptions::default()
                })
                .validate()
                .is_err()
        );
        assert!(
            DatabaseOptions::default()
                .with_vector_index(VectorIndexOptions {
                    ef_search: 0,
                    ..VectorIndexOptions::default()
                })
                .validate()
                .is_err()
        );
        assert!(
            DatabaseOptions::default()
                .with_vector_index(VectorIndexOptions {
                    m: 0,
                    ..VectorIndexOptions::default()
                })
                .validate()
                .is_err()
        );
    }

    /// - 中文: 验证慢查询、错误、binlog、redo 和 undo 日志都能写出。
    /// - English: Verifies that slow-query, error, binlog, redo, and undo logs can all be written.
    /// - 中文: 测试依赖临时目录并覆盖常见转义与编码路径。
    /// - English: The test uses a temporary directory and covers common escaping and encoding paths.
    /// - 中文: 结束时会删除临时目录，副作用限定在测试期间的临时文件。
    /// - English: It removes the temporary directory at the end, keeping side effects limited to temporary test files.
    #[test]
    fn writes_all_log_kinds() {
        let dir = temp_path("all");
        let options = DatabaseOptions::default()
            .with_slow_sql_log(dir.join("slow.log"), Duration::ZERO)
            .with_error_log(dir.join("error.log"))
            .with_binlog(dir.join("bin.log"))
            .with_redolog(dir.join("redo.log"))
            .with_undolog(dir.join("undo.log"));

        append_slow_sql(&options, "SELECT\t1", Duration::from_micros(1));
        append_error(&options, "BAD\nSQL", "broken");
        append_binlog(&options, "INSERT").unwrap();
        append_redolog(&options, RedoEvent::Begin, "INSERT").unwrap();
        append_redolog(&options, RedoEvent::Commit, "INSERT").unwrap();
        append_redolog(&options, RedoEvent::Abort, "INSERT").unwrap();
        append_undolog(&options, "INSERT", "snapshot").unwrap();

        assert!(
            fs::read_to_string(dir.join("slow.log"))
                .unwrap()
                .contains("SELECT\\t1")
        );
        assert!(
            fs::read_to_string(dir.join("error.log"))
                .unwrap()
                .contains("BAD\\nSQL")
        );
        assert!(
            fs::read_to_string(dir.join("bin.log"))
                .unwrap()
                .contains("BIN")
        );
        assert!(
            fs::read_to_string(dir.join("redo.log"))
                .unwrap()
                .contains("ABORT")
        );
        assert!(
            fs::read_to_string(dir.join("undo.log"))
                .unwrap()
                .contains("736e617073686f74")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// - 中文: 验证慢查询阈值与缺省日志路径的 no-op 行为。
    /// - English: Verifies slow-query threshold handling and no-op behavior for missing log paths.
    /// - 中文: 该测试要求低于阈值时不创建文件，并确认未配置路径时写日志不会报错。
    /// - English: The test expects no file creation below threshold and confirms that logging without configured paths does not fail.
    /// - 中文: 结果通过文件存在性和返回值断言表达。
    /// - English: Results are expressed through file-existence and return-value assertions.
    #[test]
    fn slow_sql_respects_threshold_and_missing_paths_are_noops() {
        let path = temp_path("threshold").join("slow.log");
        let options = DatabaseOptions::default().with_slow_sql_log(&path, Duration::from_secs(1));
        append_slow_sql(&options, "SELECT 1", Duration::from_millis(1));
        assert!(!path.exists());
        append_error(&DatabaseOptions::default(), "BAD", "ignored");
        append_binlog(&DatabaseOptions::default(), "INSERT").unwrap();
    }

    /// - 中文: 验证没有父目录组件的相对路径也能成功写日志。
    /// - English: Verifies that a relative path without a parent component can still receive log output.
    /// - 中文: 测试聚焦 `append_line` 对空父目录情况的处理分支。
    /// - English: The test focuses on the `append_line` branch that handles an empty parent directory.
    /// - 中文: 结束时会删除生成的日志文件。
    /// - English: It removes the generated log file at the end.
    #[test]
    fn writes_log_file_without_parent_component() {
        let path = PathBuf::from(format!(
            "fsql_log_current_{}_{}.log",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let options = DatabaseOptions::default().with_binlog(&path);
        append_binlog(&options, "INSERT").unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("INSERT"));
        fs::remove_file(path).unwrap();
    }

    /// - 中文: 验证不同 fsync 模式下日志写入仍能成功。
    /// - English: Verifies that log writes still succeed under different fsync modes.
    /// - 中文: 测试覆盖 `DataOnly` 与 `Never` 两条同步策略分支。
    /// - English: The test covers both the `DataOnly` and `Never` sync-policy branches.
    /// - 中文: 临时文件会在断言后清理。
    /// - English: Temporary files are cleaned up after assertions.
    #[test]
    fn writes_logs_with_data_only_and_never_fsync_modes() {
        let dir = temp_path("fsync_modes");
        let data_path = dir.join("data.log");
        let never_path = dir.join("never.log");

        append_binlog(
            &DatabaseOptions::default()
                .with_fsync_mode(FsyncMode::DataOnly)
                .with_binlog(&data_path),
            "INSERT DATA",
        )
        .unwrap();
        append_binlog(
            &DatabaseOptions::default()
                .with_fsync_mode(FsyncMode::Never)
                .with_binlog(&never_path),
            "INSERT NEVER",
        )
        .unwrap();

        assert!(
            fs::read_to_string(data_path)
                .unwrap()
                .contains("INSERT DATA")
        );
        assert!(
            fs::read_to_string(never_path)
                .unwrap()
                .contains("INSERT NEVER")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// - 中文: 验证禁用 WAL 时即使配置 redo 路径也不会写日志。
    /// - English: Verifies that no redo log is written when WAL is disabled even if a redo path is configured.
    /// - 中文: 测试聚焦 `append_redolog` 的快速返回分支。
    /// - English: The test focuses on the fast-return branch inside `append_redolog`.
    /// - 中文: 结果通过目标文件不存在这一副作用断言表达。
    /// - English: The result is expressed through the side-effect assertion that the target file does not exist.
    #[test]
    fn disabled_wal_skips_redolog_even_when_path_is_set() {
        let path = temp_path("disabled_wal").join("redo.log");
        let options = DatabaseOptions::default()
            .with_redolog(&path)
            .with_wal_mode(WalMode::Disabled);
        append_redolog(&options, RedoEvent::Begin, "INSERT").unwrap();
        assert!(!path.exists());
    }
}
