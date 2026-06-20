use crate::identifier::normalize_identifier;
use crate::sql::ast::{Column, ColumnType, Filter, Order, Projection, Statement};
use crate::value::{Point, Value};
use crate::{Error, Result};

pub(crate) fn parse_sql(sql: &str) -> Result<Statement> {
    let sql = sql.trim().trim_end_matches(';').trim();
    if sql.is_empty() {
        return Err(Error::Parse("empty SQL".into()));
    }

    if sql.eq_ignore_ascii_case("BEGIN") || sql.eq_ignore_ascii_case("BEGIN TRANSACTION") {
        return Ok(Statement::Begin);
    }
    if sql.eq_ignore_ascii_case("COMMIT") {
        return Ok(Statement::Commit);
    }
    if sql.eq_ignore_ascii_case("ROLLBACK") {
        return Ok(Statement::Rollback);
    }
    if starts_with_ci(sql, "CREATE TABLE") {
        return parse_create_table(sql);
    }
    if starts_with_ci(sql, "CREATE FULLTEXT INDEX") || starts_with_ci(sql, "CREATE INDEX") {
        return parse_create_index(sql);
    }
    if starts_with_ci(sql, "INSERT INTO") {
        return parse_insert(sql);
    }
    if starts_with_ci(sql, "SELECT") {
        return parse_select(sql);
    }
    if starts_with_ci(sql, "UPDATE") {
        return parse_update(sql);
    }
    if starts_with_ci(sql, "DELETE FROM") {
        return parse_delete(sql);
    }

    Err(Error::Parse("unsupported SQL statement".into()))
}

fn parse_create_table(sql: &str) -> Result<Statement> {
    let rest = sql["CREATE TABLE".len()..].trim();
    let open = rest
        .find('(')
        .ok_or_else(|| Error::Parse("CREATE TABLE requires a column list".into()))?;
    let close = rest
        .rfind(')')
        .ok_or_else(|| Error::Parse("CREATE TABLE requires a closing parenthesis".into()))?;
    if close <= open {
        return Err(Error::Parse("invalid CREATE TABLE column list".into()));
    }

    let name = normalize_identifier(&rest[..open]);
    if name.is_empty() {
        return Err(Error::Parse("CREATE TABLE requires a table name".into()));
    }

    let mut columns = Vec::new();
    for definition in split_top_level(&rest[open + 1..close], ',') {
        let tokens = definition.split_whitespace().collect::<Vec<_>>();
        if tokens.len() < 2 {
            return Err(Error::Parse(
                "column definition requires name and type".into(),
            ));
        }
        columns.push(Column {
            name: normalize_identifier(tokens[0]),
            ty: ColumnType::parse(tokens[1])?,
            primary_key: definition.to_ascii_lowercase().contains("primary key"),
        });
    }

    Ok(Statement::CreateTable { name, columns })
}

fn parse_create_index(sql: &str) -> Result<Statement> {
    let fulltext = starts_with_ci(sql, "CREATE FULLTEXT INDEX");
    let rest = if fulltext {
        sql["CREATE FULLTEXT INDEX".len()..].trim()
    } else {
        sql["CREATE INDEX".len()..].trim()
    };

    let on_pos = find_ci(rest, " ON ")
        .ok_or_else(|| Error::Parse("CREATE INDEX requires ON table(column)".into()))?;
    let target = rest[on_pos + " ON ".len()..].trim();
    let open = target
        .find('(')
        .ok_or_else(|| Error::Parse("CREATE INDEX requires a column".into()))?;
    let close = target
        .rfind(')')
        .ok_or_else(|| Error::Parse("CREATE INDEX requires a closing parenthesis".into()))?;

    Ok(Statement::CreateIndex {
        name: normalize_identifier(&rest[..on_pos]),
        table: normalize_identifier(&target[..open]),
        column: normalize_identifier(&target[open + 1..close]),
        fulltext,
    })
}

fn parse_insert(sql: &str) -> Result<Statement> {
    let rest = sql["INSERT INTO".len()..].trim();
    let values_pos =
        find_ci(rest, "VALUES").ok_or_else(|| Error::Parse("INSERT requires VALUES".into()))?;
    let target = rest[..values_pos].trim();
    let values_part = rest[values_pos + "VALUES".len()..].trim();

    let (table, columns) = if let Some(open) = target.find('(') {
        let close = target
            .rfind(')')
            .ok_or_else(|| Error::Parse("INSERT column list is not closed".into()))?;
        (
            normalize_identifier(&target[..open]),
            Some(
                split_top_level(&target[open + 1..close], ',')
                    .into_iter()
                    .map(|column| normalize_identifier(&column))
                    .collect(),
            ),
        )
    } else {
        (normalize_identifier(target), None)
    };

    let values = split_top_level(parenthesized_body(values_part, "INSERT VALUES")?, ',')
        .into_iter()
        .map(|value| parse_value(&value))
        .collect::<Result<Vec<_>>>()?;

    Ok(Statement::Insert {
        table,
        columns,
        values,
    })
}

fn parse_select(sql: &str) -> Result<Statement> {
    let rest = sql["SELECT".len()..].trim();
    let from_pos =
        find_ci(rest, " FROM ").ok_or_else(|| Error::Parse("SELECT requires FROM".into()))?;
    let projection = parse_projection(&rest[..from_pos])?;
    let from_rest = rest[from_pos + " FROM ".len()..].trim();

    let where_pos = find_ci(from_rest, " WHERE ");
    let order_pos = find_ci(from_rest, " ORDER BY ");
    let limit_pos = find_ci(from_rest, " LIMIT ");
    let table_end = [where_pos, order_pos, limit_pos]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(from_rest.len());

    let filter = if let Some(pos) = where_pos {
        let start = pos + " WHERE ".len();
        let end = [order_pos, limit_pos]
            .into_iter()
            .flatten()
            .filter(|candidate| *candidate > pos)
            .min()
            .unwrap_or(from_rest.len());
        Some(parse_filter(from_rest[start..end].trim())?)
    } else {
        None
    };

    let order = if let Some(pos) = order_pos {
        let start = pos + " ORDER BY ".len();
        let end = limit_pos
            .filter(|candidate| *candidate > pos)
            .unwrap_or(from_rest.len());
        Some(parse_order(from_rest[start..end].trim())?)
    } else {
        None
    };

    let limit = if let Some(pos) = limit_pos {
        Some(
            from_rest[pos + " LIMIT ".len()..]
                .trim()
                .parse::<usize>()
                .map_err(|_| Error::Parse("LIMIT requires a non-negative integer".into()))?,
        )
    } else {
        None
    };

    Ok(Statement::Select {
        table: normalize_identifier(&from_rest[..table_end]),
        projection,
        filter,
        order,
        limit,
    })
}

fn parse_update(sql: &str) -> Result<Statement> {
    let rest = sql["UPDATE".len()..].trim();
    let set_pos =
        find_ci(rest, " SET ").ok_or_else(|| Error::Parse("UPDATE requires SET".into()))?;
    let after_set = rest[set_pos + " SET ".len()..].trim();
    let where_pos = find_ci(after_set, " WHERE ");
    let assignments_part = where_pos
        .map(|pos| &after_set[..pos])
        .unwrap_or(after_set)
        .trim();
    let filter = if let Some(pos) = where_pos {
        Some(parse_filter(after_set[pos + " WHERE ".len()..].trim())?)
    } else {
        None
    };

    let assignments = split_top_level(assignments_part, ',')
        .into_iter()
        .map(|assignment| {
            let (column, value) = split_once_top_level(&assignment, '=')
                .ok_or_else(|| Error::Parse("assignment requires =".into()))?;
            Ok((normalize_identifier(column), parse_value(value)?))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Statement::Update {
        table: normalize_identifier(&rest[..set_pos]),
        assignments,
        filter,
    })
}

fn parse_delete(sql: &str) -> Result<Statement> {
    let rest = sql["DELETE FROM".len()..].trim();
    let where_pos = find_ci(rest, " WHERE ");
    let filter = if let Some(pos) = where_pos {
        Some(parse_filter(rest[pos + " WHERE ".len()..].trim())?)
    } else {
        None
    };

    Ok(Statement::Delete {
        table: normalize_identifier(where_pos.map(|pos| &rest[..pos]).unwrap_or(rest)),
        filter,
    })
}

fn parse_projection(input: &str) -> Result<Projection> {
    if input.trim() == "*" {
        Ok(Projection::All)
    } else {
        let columns = split_top_level(input, ',')
            .into_iter()
            .map(|column| normalize_identifier(&column))
            .collect::<Vec<_>>();
        if columns.iter().any(|column| column.is_empty()) {
            return Err(Error::Parse("empty projection column".into()));
        }
        Ok(Projection::Columns(columns))
    }
}

fn parse_filter(input: &str) -> Result<Filter> {
    if starts_with_ci(input, "MATCH") {
        let parts = split_top_level(parenthesized_body(&input["MATCH".len()..], "MATCH")?, ',');
        if parts.len() != 2 {
            return Err(Error::Parse("MATCH requires column and query".into()));
        }
        let query = match parse_value(&parts[1])? {
            Value::Text(value) => value,
            _ => return Err(Error::Parse("MATCH query must be a string".into())),
        };
        return Ok(Filter::FullText {
            column: normalize_identifier(&parts[0]),
            query,
        });
    }

    if starts_with_ci(input, "GEO_DISTANCE") {
        let (function, meters, inclusive) = if let Some(pos) = input.find("<=") {
            (&input[..pos], &input[pos + 2..], true)
        } else if let Some(pos) = input.find('<') {
            (&input[..pos], &input[pos + 1..], false)
        } else {
            return Err(Error::Parse("GEO_DISTANCE requires < or <=".into()));
        };
        let parts = split_top_level(
            parenthesized_body(&function["GEO_DISTANCE".len()..], "GEO_DISTANCE")?,
            ',',
        );
        if parts.len() != 2 {
            return Err(Error::Parse(
                "GEO_DISTANCE requires column and point".into(),
            ));
        }
        let point = match parse_value(&parts[1])? {
            Value::Point(point) => point,
            _ => return Err(Error::Parse("GEO_DISTANCE target must be POINT".into())),
        };
        return Ok(Filter::GeoWithin {
            column: normalize_identifier(&parts[0]),
            point,
            meters: meters
                .trim()
                .parse::<f64>()
                .map_err(|_| Error::Parse("GEO_DISTANCE threshold must be numeric".into()))?,
            inclusive,
        });
    }

    if let Some((column, value)) = split_once_top_level(input, '=') {
        return Ok(Filter::Equals(
            normalize_identifier(column),
            parse_value(value)?,
        ));
    }

    Err(Error::Parse("unsupported WHERE clause".into()))
}

fn parse_order(input: &str) -> Result<Order> {
    let (input, descending) = if let Some(stripped) = strip_suffix_ci(input, " DESC") {
        (stripped.trim(), true)
    } else if let Some(stripped) = strip_suffix_ci(input, " ASC") {
        (stripped.trim(), false)
    } else {
        (input.trim(), false)
    };

    if !starts_with_ci(input, "VECTOR_DISTANCE") {
        return Err(Error::Parse(
            "only VECTOR_DISTANCE ordering is currently supported".into(),
        ));
    }

    let parts = split_top_level(
        parenthesized_body(&input["VECTOR_DISTANCE".len()..], "VECTOR_DISTANCE")?,
        ',',
    );
    if parts.len() != 2 {
        return Err(Error::Parse(
            "VECTOR_DISTANCE requires column and vector".into(),
        ));
    }
    let target = match parse_value(&parts[1])? {
        Value::Vector(vector) => vector,
        _ => {
            return Err(Error::Parse(
                "VECTOR_DISTANCE target must be a vector".into(),
            ));
        }
    };

    Ok(Order::VectorDistance {
        column: normalize_identifier(&parts[0]),
        target,
        descending,
    })
}

fn parse_value(input: &str) -> Result<Value> {
    let input = input.trim();
    if input.eq_ignore_ascii_case("NULL") {
        return Ok(Value::Null);
    }
    if input.eq_ignore_ascii_case("TRUE") {
        return Ok(Value::Boolean(true));
    }
    if input.eq_ignore_ascii_case("FALSE") {
        return Ok(Value::Boolean(false));
    }
    if let Some(value) = parse_quoted_string(input) {
        return Ok(Value::Text(value));
    }
    if starts_with_ci(input, "POINT") {
        let parts = split_top_level(parenthesized_body(&input["POINT".len()..], "POINT")?, ',');
        if parts.len() != 2 {
            return Err(Error::Parse("POINT requires lon and lat".into()));
        }
        return Ok(Value::Point(Point {
            lon: parse_f64(&parts[0])?,
            lat: parse_f64(&parts[1])?,
        }));
    }
    if input.starts_with('[') && input.ends_with(']') {
        let body = &input[1..input.len() - 1];
        let vector = if body.trim().is_empty() {
            Vec::new()
        } else {
            split_top_level(body, ',')
                .into_iter()
                .map(|item| parse_f32(&item))
                .collect::<Result<Vec<_>>>()?
        };
        return Ok(Value::Vector(vector));
    }
    if input.contains('.') || input.contains('e') || input.contains('E') {
        return Ok(Value::Float(parse_f64(input)?));
    }
    input
        .parse::<i64>()
        .map(Value::Integer)
        .map_err(|_| Error::Parse(format!("could not parse value {input}")))
}

fn parse_quoted_string(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let quote = bytes[0];
    if (quote != b'\'' && quote != b'"') || bytes[bytes.len() - 1] != quote {
        return None;
    }
    let inner = &input[1..input.len() - 1];
    let doubled = format!("{}{}", quote as char, quote as char);
    Some(inner.replace(&doubled, &(quote as char).to_string()))
}

fn parse_f64(input: &str) -> Result<f64> {
    let value = input
        .trim()
        .parse::<f64>()
        .map_err(|_| Error::Parse(format!("invalid float {}", input.trim())))?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::Parse("float must be finite".into()))
    }
}

fn parse_f32(input: &str) -> Result<f32> {
    let value = input
        .trim()
        .parse::<f32>()
        .map_err(|_| Error::Parse(format!("invalid float {}", input.trim())))?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::Parse("float must be finite".into()))
    }
}

fn parenthesized_body<'a>(input: &'a str, context: &str) -> Result<&'a str> {
    let input = input.trim();
    if !input.starts_with('(') || !input.ends_with(')') {
        return Err(Error::Parse(format!("{context} requires parentheses")));
    }
    Ok(&input[1..input.len() - 1])
}

fn split_once_top_level(input: &str, delimiter: char) -> Option<(&str, &str)> {
    let mut quote = None;
    let mut parens = 0usize;
    let mut brackets = 0usize;

    for (index, ch) in input.char_indices() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => parens += 1,
            ')' => parens = parens.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            _ if ch == delimiter && parens == 0 && brackets == 0 => {
                return Some((&input[..index], &input[index + ch.len_utf8()..]));
            }
            _ => {}
        }
    }

    None
}

fn split_top_level(input: &str, delimiter: char) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    let mut parens = 0usize;
    let mut brackets = 0usize;

    for (index, ch) in input.char_indices() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => parens += 1,
            ')' => parens = parens.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            _ if ch == delimiter && parens == 0 && brackets == 0 => {
                result.push(input[start..index].trim().to_string());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }

    result.push(input[start..].trim().to_string());
    result
}

fn starts_with_ci(input: &str, prefix: &str) -> bool {
    input
        .get(..prefix.len())
        .is_some_and(|value| value.eq_ignore_ascii_case(prefix))
}

fn find_ci(input: &str, needle: &str) -> Option<usize> {
    input
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn strip_suffix_ci<'a>(input: &'a str, suffix: &str) -> Option<&'a str> {
    input
        .get(input.len().checked_sub(suffix.len())?..)
        .filter(|value| value.eq_ignore_ascii_case(suffix))
        .map(|_| &input[..input.len() - suffix.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_transaction_statements() {
        assert_eq!(parse_sql(" begin transaction ;").unwrap(), Statement::Begin);
        assert_eq!(parse_sql("COMMIT").unwrap(), Statement::Commit);
        assert_eq!(parse_sql("rollback").unwrap(), Statement::Rollback);
    }

    #[test]
    fn parses_create_table_and_indexes() {
        let table = parse_sql(
            "CREATE TABLE Docs (Id INTEGER PRIMARY KEY, Body TEXT, Embedding VECTOR, Place POINT)",
        )
        .unwrap();
        assert_eq!(
            table,
            Statement::CreateTable {
                name: "docs".into(),
                columns: vec![
                    Column {
                        name: "id".into(),
                        ty: ColumnType::Integer,
                        primary_key: true
                    },
                    Column {
                        name: "body".into(),
                        ty: ColumnType::Text,
                        primary_key: false
                    },
                    Column {
                        name: "embedding".into(),
                        ty: ColumnType::Vector,
                        primary_key: false
                    },
                    Column {
                        name: "place".into(),
                        ty: ColumnType::Point,
                        primary_key: false
                    }
                ],
            }
        );
        assert_eq!(
            parse_sql("CREATE INDEX docs_body ON Docs(Body)").unwrap(),
            Statement::CreateIndex {
                name: "docs_body".into(),
                table: "docs".into(),
                column: "body".into(),
                fulltext: false,
            }
        );
        assert_eq!(
            parse_sql("CREATE FULLTEXT INDEX docs_fts ON Docs(Body)").unwrap(),
            Statement::CreateIndex {
                name: "docs_fts".into(),
                table: "docs".into(),
                column: "body".into(),
                fulltext: true,
            }
        );
    }

    #[test]
    fn parses_insert_values() {
        let statement = parse_sql(
            "INSERT INTO docs (id, body, score, ok, embedding, place)
             VALUES (1, 'it''s sql', 1.5, true, [1.0,2.0], POINT(121.0, 31.0))",
        )
        .unwrap();
        assert_eq!(
            statement,
            Statement::Insert {
                table: "docs".into(),
                columns: Some(vec![
                    "id".into(),
                    "body".into(),
                    "score".into(),
                    "ok".into(),
                    "embedding".into(),
                    "place".into()
                ]),
                values: vec![
                    Value::Integer(1),
                    Value::Text("it's sql".into()),
                    Value::Float(1.5),
                    Value::Boolean(true),
                    Value::Vector(vec![1.0, 2.0]),
                    Value::Point(Point {
                        lon: 121.0,
                        lat: 31.0
                    })
                ],
            }
        );
    }

    #[test]
    fn parses_select_update_and_delete() {
        assert_eq!(
            parse_sql(
                "SELECT title FROM docs WHERE MATCH(body, 'rust db')
                 ORDER BY VECTOR_DISTANCE(embedding, [0.0, 1.0]) DESC LIMIT 5"
            )
            .unwrap(),
            Statement::Select {
                table: "docs".into(),
                projection: Projection::Columns(vec!["title".into()]),
                filter: Some(Filter::FullText {
                    column: "body".into(),
                    query: "rust db".into()
                }),
                order: Some(Order::VectorDistance {
                    column: "embedding".into(),
                    target: vec![0.0, 1.0],
                    descending: true,
                }),
                limit: Some(5),
            }
        );
        assert_eq!(
            parse_sql("UPDATE docs SET title = \"x\", score = 2 WHERE id = 1").unwrap(),
            Statement::Update {
                table: "docs".into(),
                assignments: vec![
                    ("title".into(), Value::Text("x".into())),
                    ("score".into(), Value::Integer(2))
                ],
                filter: Some(Filter::Equals("id".into(), Value::Integer(1))),
            }
        );
        assert_eq!(
            parse_sql("DELETE FROM docs WHERE GEO_DISTANCE(place, POINT(0.0, 0.0)) <= 10").unwrap(),
            Statement::Delete {
                table: "docs".into(),
                filter: Some(Filter::GeoWithin {
                    column: "place".into(),
                    point: Point { lon: 0.0, lat: 0.0 },
                    meters: 10.0,
                    inclusive: true,
                }),
            }
        );
    }

    #[test]
    fn parser_helpers_handle_nested_delimiters_and_identifiers() {
        assert_eq!(
            split_top_level("a, POINT(1, 2), [3,4], 'x,y'", ','),
            vec!["a", "POINT(1, 2)", "[3,4]", "'x,y'"]
        );
        assert_eq!(split_once_top_level("a = POINT(1,2)", '=').unwrap().0, "a ");
        assert_eq!(
            split_once_top_level("a = ['x=y']", '=').unwrap().1.trim(),
            "['x=y']"
        );
        assert!(split_once_top_level("'x=y'", '=').is_none());
        assert!(split_once_top_level("[x=y]", '=').is_none());
        assert!(split_once_top_level("(x=y)", '=').is_none());
        assert!(split_once_top_level("POINT(1,2)", '=').is_none());
        assert!(starts_with_ci("Select", "select"));
        assert!(!starts_with_ci("Sel", "select"));
        assert_eq!(find_ci("a ORDER BY b", " order by "), Some(1));
        assert_eq!(strip_suffix_ci("x DESC", " desc"), Some("x"));
        assert_eq!(strip_suffix_ci("x", " desc"), None);
        assert_eq!(normalize_identifier(" `Mixed` "), "mixed");
        assert_eq!(parse_value("NULL").unwrap(), Value::Null);
        assert_eq!(parse_value("FALSE").unwrap(), Value::Boolean(false));
        assert_eq!(parse_value("[]").unwrap(), Value::Vector(Vec::new()));
        assert!(parse_value("1e9999").is_err());
        assert!(parse_value("[1e9999]").is_err());
        assert_eq!(
            parse_order("VECTOR_DISTANCE(v, [1.0]) ASC").unwrap(),
            Order::VectorDistance {
                column: "v".into(),
                target: vec![1.0],
                descending: false,
            }
        );
    }

    #[test]
    fn rejects_invalid_sql_shapes() {
        let invalid = [
            "",
            "ALTER TABLE t",
            "CREATE TABLE t",
            "CREATE TABLE t)",
            "CREATE TABLE t (id INTEGER",
            "CREATE TABLE t)(",
            "CREATE TABLE (id INTEGER)",
            "CREATE TABLE t (id)",
            "CREATE TABLE t (id BLOB)",
            "CREATE INDEX i t(c)",
            "CREATE INDEX i ON t",
            "CREATE INDEX i ON t(c",
            "INSERT INTO t",
            "INSERT INTO t (id VALUES (1)",
            "INSERT INTO t VALUES 1",
            "SELECT * t",
            "SELECT , FROM t",
            "SELECT * FROM t LIMIT no",
            "UPDATE t",
            "UPDATE t SET a",
            "DELETE FROM t WHERE a > 1",
            "SELECT * FROM t WHERE MATCH(a)",
            "SELECT * FROM t WHERE MATCH(a, 1)",
            "SELECT * FROM t WHERE GEO_DISTANCE(a, POINT(0,0))",
            "SELECT * FROM t WHERE GEO_DISTANCE(a) < 1",
            "SELECT * FROM t WHERE GEO_DISTANCE(a, 1) < 1",
            "SELECT * FROM t WHERE GEO_DISTANCE(a, POINT(0,0)) < x",
            "SELECT * FROM t ORDER BY title",
            "SELECT * FROM t ORDER BY VECTOR_DISTANCE(a)",
            "SELECT * FROM t ORDER BY VECTOR_DISTANCE(a, 1)",
            "INSERT INTO t VALUES (POINT(1))",
            "INSERT INTO t VALUES (POINT(x, 0))",
            "INSERT INTO t VALUES (1.2.3)",
            "INSERT INTO t VALUES ([x])",
            "INSERT INTO t VALUES ([nan])",
            "INSERT INTO t VALUES (nan)",
            "INSERT INTO t VALUES (abc)",
        ];
        for sql in invalid {
            assert!(parse_sql(sql).is_err(), "{sql}");
        }
    }
}
