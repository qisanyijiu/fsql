# fsql

`fsql` is a small embedded SQL database prototype written in Rust.

This repository starts the project as a real, runnable crate rather than a
placeholder. The first milestone includes:

- Embedded library API with file-backed or in-memory databases.
- Atomic commit persistence through a temp-file-and-rename write path.
- Single-writer transactions with `BEGIN`, `COMMIT`, and `ROLLBACK`.
- Typed tables, primary keys, secondary equality indexes, and index rebuilds.
- Full-text inverted indexes over `TEXT` columns.
- Vector nearest-neighbor queries over `VECTOR` columns.
- Geographic distance filters over `POINT(lon, lat)` columns.
- Thread-safe connection pools with configurable checkout limits.
- Slow SQL logs, error logs, binlog, redolog, and undolog append streams.
- Unit tests for the implemented behavior.
- Stable performance baselines for bulk inserts, indexed lookups, full-text
  search, vector ordering, and geo filters.

## Architecture

The crate is split into production modules:

- `engine`: public `Database`, transaction handling, statement execution, and
  persistence.
- `pool`: thread-safe `ConnectionPool` and checked-out `Connection` handles.
- `logging`: `DatabaseOptions` plus append-only SQL and recovery log writers.
- `sql`: SQL AST and parser.
- `storage`: catalog, table, indexes, and disk codec.
- `value`, `query`, `error`, `identifier`: shared domain types and helpers.

The user-facing goal is intentionally larger than this first commit. A
production database with complete SQL compatibility, formally demonstrated
ACID guarantees, 100% measured coverage, and performance beyond SQLite is a
multi-release project. This crate is the executable foundation for that work.

## Library usage

The crate exposes a single embedded database type:

```rust
use fsql::Database;

let mut db = Database::memory();
db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")?;
db.execute("INSERT INTO users VALUES (1, 'Ada')")?;
let rows = db.execute("SELECT name FROM users WHERE id = 1")?.rows;
```

Use `Database::memory()` for an in-memory database or `Database::open(path)` for
file-backed persistence. Mutating statements outside an explicit transaction are
persisted immediately. `BEGIN`, `COMMIT`, and `ROLLBACK` keep a single-writer
transaction open in memory until commit time.

## Connection pool and logs

Use `ConnectionPool` when the database is shared by multiple threads. The pool
limits the number of checked-out connections, offers blocking `get()` and
non-blocking `try_get()`, and serializes statement execution through the shared
embedded engine.

```rust
use std::time::Duration;
use fsql::{
    ConnectionPool, DatabaseOptions, FsyncMode, FullTextTokenizer,
    GeoCoordinateSystem, SqlDialect, VectorIndexOptions, VectorMetric, WalMode,
};

let options = DatabaseOptions::default()
    .with_page_size(8192)
    .with_cache_capacity(256)
    .with_wal_mode(WalMode::RedoLog)
    .with_fsync_mode(FsyncMode::DataOnly)
    .with_worker_threads(4)
    .with_sql_dialect(SqlDialect::Sqlite)
    .with_fulltext_tokenizer(FullTextTokenizer::Simple)
    .with_vector_index(VectorIndexOptions {
        metric: VectorMetric::Cosine,
        dimensions: Some(2),
        ..VectorIndexOptions::default()
    })
    .with_geo_coordinate_system(GeoCoordinateSystem::Wgs84)
    .with_slow_sql_log("slow.log", Duration::from_millis(50))
    .with_error_log("error.log")
    .with_binlog("bin.log")
    .with_redolog("redo.log")
    .with_undolog("undo.log");

let pool = ConnectionPool::open_with_options("app.fsql", 8, options)?;
let connection = pool.get()?;
connection.execute("SELECT * FROM users")?;
```

The log streams are append-only text files:

- Slow SQL log: successful statements whose elapsed time meets the configured
  threshold.
- Error log: failed SQL text and the returned error.
- Binlog: logical mutating SQL and transaction-control SQL.
- Redolog: `BEGIN`, `COMMIT`, and `ABORT` records around mutations and commits.
- Undolog: catalog snapshots before mutating statements.

## Configuration

`DatabaseOptions` is the central runtime configuration object. Defaults are
valid and conservative; `open_with_options` validates the options before
opening a file-backed database, and `try_memory_with_options` does the same for
in-memory databases.

| Option | Default | Effect |
| --- | --- | --- |
| `page_size` | `4096` | Chunk size used by the file persistence write path. Must be a power of two from 512 to 65536. |
| `cache_capacity` | `128` | Number of parsed SQL statements kept in the in-memory statement cache. `0` disables the cache. |
| `wal_mode` | `Disabled` | Controls redo logging. `with_redolog(...)` enables `RedoLog`; `Disabled` suppresses redo output even when a path is set. |
| `fsync_mode` | `Always` | Persistence and log sync strategy: `Always`, `DataOnly`, or `Never`. |
| `worker_threads` | `1` | Runtime worker-thread setting. Vector distance ordering uses it to parallelize candidate scoring when more than one worker is configured. |
| `sql_dialect` | `Fsql` | Parser dialect. `Sqlite` accepts aliases such as `BEGIN IMMEDIATE` and `END`; `PostgreSql` accepts `BEGIN WORK`, `COMMIT WORK`, and `ROLLBACK WORK`. |
| `fulltext_tokenizer` | `Simple` | Full-text tokenizer used for index rebuilds and `MATCH`: `Simple`, `Whitespace`, or `Exact`. |
| `vector_index` | Euclidean, dynamic dimensions | Vector search settings: metric (`Euclidean`, `Cosine`, `DotProduct`), optional dimension enforcement, `ef_search`, and `m`. |
| `geo_coordinate_system` | `Wgs84` | Geo distance calculation: haversine meters for `Wgs84`, Euclidean units for `Cartesian`. |
| `slow_sql_threshold` / `slow_sql_log_path` | disabled | Successful statements at or above the threshold are appended to the slow SQL log. |
| `error_log_path` | disabled | Failed statements are appended to the error log. |
| `binlog_path` | disabled | Mutating SQL and transaction-control SQL are appended to the binlog. |
| `redolog_path` | disabled | Redo events are appended when `wal_mode` is `RedoLog`. |
| `undolog_path` | disabled | Catalog snapshots are appended before mutating statements. |

## SQL subset

The current parser supports a compact SQL subset:

```sql
CREATE TABLE docs (
  id INTEGER PRIMARY KEY,
  title TEXT,
  body TEXT,
  embedding VECTOR,
  place POINT
);

CREATE INDEX docs_title ON docs(title);
CREATE FULLTEXT INDEX docs_body ON docs(body);

INSERT INTO docs (id, title, body, embedding, place)
VALUES (1, 'Rust', 'rust sql database', [0.1, 0.2], POINT(121.47, 31.23));

SELECT * FROM docs WHERE id = 1;
SELECT title FROM docs WHERE MATCH(body, 'rust database');
SELECT * FROM docs ORDER BY VECTOR_DISTANCE(embedding, [0.0, 0.0]) LIMIT 10;
SELECT * FROM docs WHERE GEO_DISTANCE(place, POINT(121.47, 31.23)) < 1000;

BEGIN;
UPDATE docs SET title = 'Fast Rust' WHERE id = 1;
ROLLBACK;
DELETE FROM docs WHERE id = 1;
```

### Supported statements and values

- `CREATE TABLE`, `CREATE INDEX`, `CREATE FULLTEXT INDEX`
- `INSERT`, `SELECT`, `UPDATE`, `DELETE`
- `BEGIN`, `COMMIT`, `ROLLBACK`
- Column types: `INTEGER`, `FLOAT`, `BOOLEAN`, `TEXT`, `VECTOR`, `POINT`
- Value literals: `NULL`, `TRUE`, `FALSE`, quoted strings, numeric literals,
  vectors like `[0.1, 0.2]`, and points like `POINT(121.47, 31.23)`

### Current query capabilities

- `WHERE` supports equality predicates such as `id = 1`
- `WHERE MATCH(column, 'terms')` supports full-text token matching on `TEXT`
  columns
- `WHERE GEO_DISTANCE(column, POINT(...)) < meters` and `<=` support radius
  filters on `POINT` columns
- `ORDER BY VECTOR_DISTANCE(column, [...]) ASC|DESC` supports nearest-neighbor
  ordering on `VECTOR` columns
- `LIMIT n` is supported on `SELECT`

## Current limitations

This is intentionally a prototype, not a full SQL engine.

- `WHERE` does not support `AND`, `OR`, ranges, joins, or arbitrary expressions
- `ORDER BY` only supports `VECTOR_DISTANCE(...)`
- Full-text tokenization is configurable; the default `Simple` tokenizer splits
  by non-alphanumeric separators and lowercases tokens
- Vector ordering requires the query vector and stored vector to have matching
  dimensions, and configured dimensions are enforced when set
- Geographic distance defaults to WGS84 haversine meters and can be switched to
  Cartesian distance
- The connection pool is thread-safe and supports concurrent callers, but the
  current engine serializes statement execution instead of providing MVCC.
- Binlog, redolog, and undolog are written and tested as append-only streams;
  automated crash recovery and replay are future work.

## Development

```sh
cargo test
cargo llvm-cov --summary-only --fail-under-lines 100
```

`cargo test` also runs `tests/performance.rs`. Those tests enforce lightweight
performance baselines for bulk inserts, indexed lookups, full-text search,
vector ordering, and geo filters.

The performance tests are skipped only when `cargo llvm-cov` sets
`cfg(coverage)`, so benchmark-style assertions do not pollute library source
coverage.
