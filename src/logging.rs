use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Result;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DatabaseOptions {
    pub slow_sql_threshold: Option<Duration>,
    pub slow_sql_log_path: Option<PathBuf>,
    pub error_log_path: Option<PathBuf>,
    pub binlog_path: Option<PathBuf>,
    pub redolog_path: Option<PathBuf>,
    pub undolog_path: Option<PathBuf>,
}

impl DatabaseOptions {
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
    let _ = append_optional_line(&options.slow_sql_log_path, &line);
}

pub(crate) fn append_error(options: &DatabaseOptions, sql: &str, error: &str) {
    let line = format!(
        "ERROR\t{}\t{}\t{}\n",
        timestamp_millis(),
        escape(sql),
        escape(error)
    );
    let _ = append_optional_line(&options.error_log_path, &line);
}

pub(crate) fn append_binlog(options: &DatabaseOptions, sql: &str) -> Result<()> {
    let line = format!("BIN\t{}\t{}\n", timestamp_millis(), escape(sql));
    append_optional_line(&options.binlog_path, &line)
}

pub(crate) fn append_redolog(options: &DatabaseOptions, event: RedoEvent, sql: &str) -> Result<()> {
    let event = match event {
        RedoEvent::Begin => "BEGIN",
        RedoEvent::Commit => "COMMIT",
        RedoEvent::Abort => "ABORT",
    };
    let line = format!("REDO\t{}\t{}\t{}\n", timestamp_millis(), event, escape(sql));
    append_optional_line(&options.redolog_path, &line)
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
    append_optional_line(&options.undolog_path, &line)
}

fn append_optional_line(path: &Option<PathBuf>, line: &str) -> Result<()> {
    if let Some(path) = path {
        append_line(path, line)?;
    }
    Ok(())
}

fn append_line(path: &Path, line: &str) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.flush()?;
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
        let options = DatabaseOptions::default()
            .with_slow_sql_log("slow.log", Duration::from_millis(1))
            .with_error_log("error.log")
            .with_binlog("bin.log")
            .with_redolog("redo.log")
            .with_undolog("undo.log");
        assert_eq!(options.slow_sql_threshold, Some(Duration::from_millis(1)));
        assert_eq!(options.error_log_path, Some(PathBuf::from("error.log")));
        assert_eq!(options.binlog_path, Some(PathBuf::from("bin.log")));
        assert_eq!(options.redolog_path, Some(PathBuf::from("redo.log")));
        assert_eq!(options.undolog_path, Some(PathBuf::from("undo.log")));
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
}
