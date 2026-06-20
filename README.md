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
- Unit tests for the implemented behavior.
- Stable performance baselines for bulk inserts, indexed lookups, full-text
  search, vector ordering, and geo filters.

## Architecture

The crate is split into production modules:

- `engine`: public `Database`, transaction handling, statement execution, and
  persistence.
- `sql`: SQL AST and parser.
- `storage`: catalog, table, indexes, and disk codec.
- `value`, `query`, `error`, `identifier`: shared domain types and helpers.

The user-facing goal is intentionally larger than this first commit. A
production database with complete SQL compatibility, formally demonstrated
ACID guarantees, 100% measured coverage, and performance beyond SQLite is a
multi-release project. This crate is the executable foundation for that work.

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
```

## Development

```sh
cargo test
cargo llvm-cov --summary-only --fail-under-lines 100
```

`cargo test` also runs `tests/performance.rs`. Those performance tests are
skipped only when `cargo llvm-cov` sets `cfg(coverage)`, so benchmark assertions
do not pollute library source coverage.
