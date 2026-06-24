#![cfg(not(coverage))]

use std::hint::black_box;
use std::time::{Duration, Instant};

use fsql::{Database, Value};

/// - 创建性能基准测试使用的 `docs` 表结构。
/// - Creates the `docs` table schema used by the performance baseline tests.
/// - 夹具要求传入可写内存数据库，并固定包含全文、向量和地理查询所需列。
/// - The fixture requires a writable in-memory database and always includes the columns needed for full-text, vector, and geo queries.
/// - 通过执行 `CREATE TABLE` 修改数据库状态；建表失败会直接 panic。
/// - Mutates database state by executing `CREATE TABLE`; panics immediately if creation fails.
fn create_perf_table(db: &mut Database) {
    db.execute(
        "CREATE TABLE docs (
            id INTEGER PRIMARY KEY,
            title TEXT,
            body TEXT,
            age INTEGER,
            embedding VECTOR,
            place POINT
        )",
    )
    .unwrap();
}

#[test]
/// - 测量批量插入与普通索引查找的基础性能。
/// - Measures the baseline performance of bulk inserts and secondary-index lookups.
/// - 场景聚焦批量写入后按年龄索引查询。
/// - The scenario focuses on indexed age lookups after bulk inserts.
fn perf_bulk_insert_and_index_lookup_baseline() {
    let mut db = Database::memory();
    create_perf_table(&mut db);
    db.execute("CREATE INDEX docs_age ON docs(age)").unwrap();

    let started = Instant::now();
    for id in 0..1_000 {
        db.execute(&format!(
            "INSERT INTO docs VALUES ({id}, 'title{id}', 'rust database text{id}', {}, [1.0, 2.0], POINT(0.0, 0.0))",
            id % 50
        ))
        .unwrap();
    }
    let insert_elapsed = started.elapsed();

    let started = Instant::now();
    let mut rows = 0usize;
    for age in 0..50 {
        rows += db
            .execute(&format!("SELECT id FROM docs WHERE age = {age}"))
            .unwrap()
            .rows
            .len();
    }
    let lookup_elapsed = started.elapsed();

    assert_eq!(rows, 1_000);
    assert!(
        insert_elapsed < Duration::from_secs(10),
        "bulk insert baseline too slow: {insert_elapsed:?}"
    );
    assert!(
        lookup_elapsed < Duration::from_secs(2),
        "indexed lookup baseline too slow: {lookup_elapsed:?}"
    );
    black_box((insert_elapsed, lookup_elapsed, rows));
}

#[test]
/// - 测量全文、向量排序和地理过滤查询的基础性能。
/// - Measures the baseline performance of full-text, vector-ordering, and geo-filter queries.
/// - 场景聚焦搜索夹具上的全文、向量和地理查询。
/// - The scenario focuses on full-text, vector, and geo queries over the search fixture.
fn perf_fulltext_vector_and_geo_baseline() {
    let mut db = Database::memory();
    create_perf_table(&mut db);
    db.execute("CREATE FULLTEXT INDEX docs_body ON docs(body)")
        .unwrap();

    let started = Instant::now();
    for id in 0..400 {
        let x = (id % 20) as f32 / 20.0;
        let y = (id % 10) as f32 / 10.0;
        db.execute(&format!(
            "INSERT INTO docs VALUES ({id}, 'doc{id}', 'rust vector geo search {id}', {}, [{x}, {y}], POINT({}, {}))",
            id % 25,
            id as f64 * 0.001,
            id as f64 * 0.001
        ))
        .unwrap();
    }
    let load_elapsed = started.elapsed();

    let started = Instant::now();
    let fulltext_rows = db
        .execute("SELECT id FROM docs WHERE MATCH(body, 'rust search')")
        .unwrap()
        .rows
        .len();
    let vector_rows = db
        .execute("SELECT id FROM docs ORDER BY VECTOR_DISTANCE(embedding, [0.0, 0.0]) LIMIT 20")
        .unwrap()
        .rows;
    let geo_rows = db
        .execute("SELECT id FROM docs WHERE GEO_DISTANCE(place, POINT(0.0, 0.0)) < 1000")
        .unwrap()
        .rows
        .len();
    let query_elapsed = started.elapsed();

    assert_eq!(fulltext_rows, 400);
    assert_eq!(vector_rows.len(), 20);
    assert!(matches!(vector_rows[0].get("id"), Some(Value::Integer(_))));
    assert!(geo_rows > 0);
    assert!(
        load_elapsed < Duration::from_secs(10),
        "search fixture load baseline too slow: {load_elapsed:?}"
    );
    assert!(
        query_elapsed < Duration::from_secs(2),
        "search query baseline too slow: {query_elapsed:?}"
    );
    black_box((load_elapsed, query_elapsed, fulltext_rows, geo_rows));
}
