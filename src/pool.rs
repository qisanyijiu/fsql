use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use crate::{Database, DatabaseOptions, QueryResult, Result};

#[derive(Clone)]
pub struct ConnectionPool {
    inner: Arc<PoolInner>,
}

pub struct Connection {
    inner: Arc<PoolInner>,
    released: bool,
}

struct PoolInner {
    database: Mutex<Database>,
    permits: Mutex<usize>,
    available: Condvar,
    max_connections: usize,
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
            .map_err(|_| crate::Error::Execution("connection pool lock poisoned".into()))?;
        while *permits == 0 {
            permits = self
                .inner
                .available
                .wait(permits)
                .map_err(|_| crate::Error::Execution("connection pool lock poisoned".into()))?;
        }
        *permits -= 1;
        Ok(Connection {
            inner: Arc::clone(&self.inner),
            released: false,
        })
    }

    pub fn try_get(&self) -> Result<Option<Connection>> {
        let mut permits = self
            .inner
            .permits
            .lock()
            .map_err(|_| crate::Error::Execution("connection pool lock poisoned".into()))?;
        if *permits == 0 {
            return Ok(None);
        }
        *permits -= 1;
        Ok(Some(Connection {
            inner: Arc::clone(&self.inner),
            released: false,
        }))
    }

    fn from_database(database: Database, max_connections: usize) -> Result<Self> {
        if max_connections == 0 {
            return Err(crate::Error::Execution(
                "connection pool size must be greater than zero".into(),
            ));
        }
        Ok(Self {
            inner: Arc::new(PoolInner {
                database: Mutex::new(database),
                permits: Mutex::new(max_connections),
                available: Condvar::new(),
                max_connections,
            }),
        })
    }
}

impl Connection {
    pub fn execute(&self, sql: &str) -> Result<QueryResult> {
        self.inner
            .database
            .lock()
            .map_err(|_| crate::Error::Execution("database lock poisoned".into()))?
            .execute(sql)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        if let Ok(mut permits) = self.inner.permits.lock() {
            *permits += 1;
            self.inner.available.notify_one();
        }
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
                let _guard = inner.database.lock().unwrap();
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
