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

    pub fn with_page_size(mut self, page_size: usize) -> Self {
        self.page_size = page_size;
        self
    }

    pub fn with_cache_capacity(mut self, cache_capacity: usize) -> Self {
        self.cache_capacity = cache_capacity;
        self
    }

    pub fn with_wal_mode(mut self, wal_mode: WalMode) -> Self {
        self.wal_mode = wal_mode;
        self
    }

    pub fn with_fsync_mode(mut self, fsync_mode: FsyncMode) -> Self {
        self.fsync_mode = fsync_mode;
        self
    }

    pub fn with_worker_threads(mut self, worker_threads: usize) -> Self {
        self.worker_threads = worker_threads;
        self
    }

    pub fn with_sql_dialect(mut self, sql_dialect: SqlDialect) -> Self {
        self.sql_dialect = sql_dialect;
        self
    }

    pub fn with_fulltext_tokenizer(mut self, tokenizer: FullTextTokenizer) -> Self {
        self.fulltext_tokenizer = tokenizer;
        self
    }

    pub fn with_vector_index(mut self, vector_index: VectorIndexOptions) -> Self {
        self.vector_index = vector_index;
        self
    }

    pub fn with_geo_coordinate_system(mut self, coordinate_system: GeoCoordinateSystem) -> Self {
        self.geo_coordinate_system = coordinate_system;
        self
    }

    pub fn with_slow_sql_log(mut self, path: impl Into<PathBuf>, threshold: Duration) -> Self {
        self.slow_sql_log_path = Some(path.into());
        self.slow_sql_threshold = Some(threshold);
        self
    }

    pub fn with_error_log(mut self, path: impl Into<PathBuf>) -> Self {
        self.error_log_path = Some(path.into());
        self
    }

    pub fn with_binlog(mut self, path: impl Into<PathBuf>) -> Self {
        self.binlog_path = Some(path.into());
        self
    }

    pub fn with_redolog(mut self, path: impl Into<PathBuf>) -> Self {
        self.redolog_path = Some(path.into());
        self.wal_mode = WalMode::RedoLog;
        self
    }

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

pub(crate) fn append_error(options: &DatabaseOptions, sql: &str, error: &str) {
    let line = format!(
        "ERROR\t{}\t{}\t{}\n",
        timestamp_millis(),
        escape(sql),
        escape(error)
    );
    let _ = append_optional_line(options, &options.error_log_path, &line);
}

pub(crate) fn append_binlog(options: &DatabaseOptions, sql: &str) -> Result<()> {
    let line = format!("BIN\t{}\t{}\n", timestamp_millis(), escape(sql));
    append_optional_line(options, &options.binlog_path, &line)
}

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

pub(crate) fn sync_file(file: &fs::File, fsync_mode: FsyncMode) -> Result<()> {
    match fsync_mode {
        FsyncMode::Always => file.sync_all()?,
        FsyncMode::DataOnly => file.sync_data()?,
        FsyncMode::Never => {}
    }
    Ok(())
}

fn escape(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

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

    fn temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("fsql_log_{name}_{}_{}", std::process::id(), nanos))
    }

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

    #[test]
    fn slow_sql_respects_threshold_and_missing_paths_are_noops() {
        let path = temp_path("threshold").join("slow.log");
        let options = DatabaseOptions::default().with_slow_sql_log(&path, Duration::from_secs(1));
        append_slow_sql(&options, "SELECT 1", Duration::from_millis(1));
        assert!(!path.exists());
        append_error(&DatabaseOptions::default(), "BAD", "ignored");
        append_binlog(&DatabaseOptions::default(), "INSERT").unwrap();
    }

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
