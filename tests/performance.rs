#![cfg(not(coverage))]

use std::hint::black_box;
use std::time::{Duration, Instant};

use fsql::{Database, Value};

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
