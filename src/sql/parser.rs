use crate::identifier::normalize_identifier;
use crate::logging::SqlDialect;
use crate::sql::ast::{
    Column, ColumnType, Filter, Order, ParsedOnlyStatementKind, Projection, Statement,
};
use crate::value::{Point, Value};
use crate::{Error, Result};

/// - 以默认 FSQL 方言解析 SQL 文本为语句 AST。
/// - Parses SQL text into a statement AST using the default FSQL dialect.
/// - 输入可包含首尾空白与结尾分号；空语句和不支持语句会报错。
/// - Input may include surrounding whitespace and a trailing semicolon; empty or unsupported statements return an error.
/// - 返回解析后的 `Statement`，或返回解析错误且无副作用。
/// - Returns the parsed `Statement`, or a parse error with no side effects.
#[cfg(test)]
pub(crate) fn parse_sql(sql: &str) -> Result<Statement> {
    parse_sql_with_dialect(sql, SqlDialect::Fsql)
}

/// - 按指定 SQL 方言解析顶层语句并分派到具体解析器。
/// - Parses a top-level statement under the given SQL dialect and dispatches to a concrete parser.
/// - 输入会先裁剪空白和结尾分号；仅支持当前实现覆盖的语句形状。
/// - Trims whitespace and trailing semicolons first; only statement shapes supported by the current implementation are accepted.
/// - 返回对应的 `Statement`，或在语法不匹配时返回 `Error::Parse`。
/// - Returns the corresponding `Statement`, or `Error::Parse` when syntax does not match.
pub(crate) fn parse_sql_with_dialect(sql: &str, dialect: SqlDialect) -> Result<Statement> {
    let sql = sql.trim().trim_end_matches(';').trim();
    if sql.is_empty() {
        return Err(Error::Parse("empty SQL".into()));
    }

    if starts_with_ci(sql, "EXPLAIN") {
        return parse_explain(sql, dialect);
    }
    if starts_with_ci(sql, "WITH") {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::With, "WITH");
    }
    if starts_with_ci(sql, "VALUES") {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::Values, "VALUES");
    }
    if is_begin(sql, dialect) {
        return Ok(Statement::Begin);
    }
    if is_commit(sql, dialect) {
        return Ok(Statement::Commit);
    }
    if is_rollback(sql, dialect) {
        return Ok(Statement::Rollback);
    }
    if starts_with_ci(sql, "INSERT OR ") {
        return parse_advanced_insert_kind(sql, ParsedOnlyStatementKind::Insert, "INSERT");
    }
    if starts_with_ci(sql, "UPDATE OR ") {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::Update, "UPDATE");
    }
    if starts_with_ci(sql, "ROLLBACK TO") {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::RollbackTo, "ROLLBACK TO");
    }
    if starts_with_ci(sql, "SAVEPOINT") {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::Savepoint, "SAVEPOINT");
    }
    if starts_with_ci(sql, "RELEASE") {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::Release, "RELEASE");
    }
    if starts_with_ci(sql, "ALTER TABLE") {
        return parse_alter_table(sql);
    }
    if starts_with_ci(sql, "ANALYZE") {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::Analyze, "ANALYZE");
    }
    if starts_with_ci(sql, "ATTACH") {
        return parse_attach_or_detach(sql, ParsedOnlyStatementKind::Attach, "ATTACH");
    }
    if starts_with_ci(sql, "DETACH") {
        return parse_attach_or_detach(sql, ParsedOnlyStatementKind::Detach, "DETACH");
    }
    if starts_with_ci(sql, "CREATE TABLE") {
        if is_advanced_create_table(sql) {
            return parse_parsed_only(sql, ParsedOnlyStatementKind::CreateTable, "CREATE TABLE");
        }
        return parse_create_table(sql);
    }
    if starts_with_ci(sql, "CREATE TEMP TABLE") || starts_with_ci(sql, "CREATE TEMPORARY TABLE") {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::CreateTable, "CREATE");
    }
    if starts_with_ci(sql, "CREATE VIEW")
        || starts_with_ci(sql, "CREATE TEMP VIEW")
        || starts_with_ci(sql, "CREATE TEMPORARY VIEW")
    {
        return parse_create_view(sql);
    }
    if starts_with_ci(sql, "CREATE TRIGGER")
        || starts_with_ci(sql, "CREATE TEMP TRIGGER")
        || starts_with_ci(sql, "CREATE TEMPORARY TRIGGER")
    {
        return parse_create_trigger(sql);
    }
    if starts_with_ci(sql, "CREATE VIRTUAL TABLE") {
        return parse_create_virtual_table(sql);
    }
    if starts_with_ci(sql, "CREATE UNIQUE INDEX")
        || starts_with_ci(sql, "CREATE INDEX IF NOT EXISTS")
        || starts_with_ci(sql, "CREATE UNIQUE INDEX IF NOT EXISTS")
    {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::CreateIndex, "CREATE");
    }
    if starts_with_ci(sql, "CREATE FULLTEXT INDEX") || starts_with_ci(sql, "CREATE INDEX") {
        if is_advanced_create_index(sql) {
            return parse_parsed_only(sql, ParsedOnlyStatementKind::CreateIndex, "CREATE");
        }
        return parse_create_index(sql);
    }
    if starts_with_ci(sql, "DROP INDEX") {
        return parse_drop(sql, ParsedOnlyStatementKind::DropIndex, "DROP INDEX");
    }
    if starts_with_ci(sql, "DROP TABLE") {
        return parse_drop(sql, ParsedOnlyStatementKind::DropTable, "DROP TABLE");
    }
    if starts_with_ci(sql, "DROP TRIGGER") {
        return parse_drop(sql, ParsedOnlyStatementKind::DropTrigger, "DROP TRIGGER");
    }
    if starts_with_ci(sql, "DROP VIEW") {
        return parse_drop(sql, ParsedOnlyStatementKind::DropView, "DROP VIEW");
    }
    if starts_with_ci(sql, "INSERT INTO") {
        if is_advanced_insert(sql) {
            return parse_advanced_insert(sql);
        }
        return parse_insert(sql);
    }
    if starts_with_ci(sql, "REPLACE INTO") {
        return parse_advanced_insert_kind(sql, ParsedOnlyStatementKind::Replace, "REPLACE INTO");
    }
    if starts_with_ci(sql, "SELECT") {
        if is_advanced_select(sql) {
            return parse_parsed_only(sql, ParsedOnlyStatementKind::Select, "SELECT");
        }
        return parse_select(sql);
    }
    if starts_with_ci(sql, "UPDATE") {
        if is_advanced_update(sql) {
            return parse_parsed_only(sql, ParsedOnlyStatementKind::Update, "UPDATE");
        }
        return parse_update(sql);
    }
    if starts_with_ci(sql, "DELETE FROM") {
        if is_advanced_delete(sql) {
            return parse_parsed_only(sql, ParsedOnlyStatementKind::Delete, "DELETE FROM");
        }
        return parse_delete(sql);
    }
    if starts_with_ci(sql, "PRAGMA") {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::Pragma, "PRAGMA");
    }
    if starts_with_ci(sql, "REINDEX") {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::Reindex, "REINDEX");
    }
    if starts_with_ci(sql, "VACUUM") {
        return parse_parsed_only(sql, ParsedOnlyStatementKind::Vacuum, "VACUUM");
    }

    Err(Error::Parse("unsupported SQL statement".into()))
}

/// - 解析 `EXPLAIN` 或 `EXPLAIN QUERY PLAN` 包裹的子语句。
/// - Parses a child statement wrapped by `EXPLAIN` or `EXPLAIN QUERY PLAN`.
/// - 输入必须在 `EXPLAIN` 后提供一条当前语法层可识别的 SQL。
/// - Input must provide one SQL statement that the current language layer can recognize after `EXPLAIN`.
/// - 返回 `Statement::Explain`，或在缺失/子语句解析失败时返回解析错误。
/// - Returns `Statement::Explain`, or a parse error when the child is missing or cannot be parsed.
fn parse_explain(sql: &str, dialect: SqlDialect) -> Result<Statement> {
    let mut rest = sql["EXPLAIN".len()..].trim();
    if starts_with_ci(rest, "QUERY PLAN") {
        rest = rest["QUERY PLAN".len()..].trim();
    }
    if rest.is_empty() {
        return Err(Error::Parse("EXPLAIN requires a statement".into()));
    }

    let statement = parse_sql_with_dialect(rest, dialect)?;
    Ok(Statement::Explain(Box::new(statement)))
}

fn parse_parsed_only(sql: &str, kind: ParsedOnlyStatementKind, prefix: &str) -> Result<Statement> {
    let rest = sql[prefix.len()..].trim();
    if rest.is_empty() {
        return Err(Error::Parse(format!("{} requires a target", kind.as_str())));
    }
    Ok(Statement::ParsedOnly {
        kind,
        sql: sql.to_string(),
    })
}

fn parse_alter_table(sql: &str) -> Result<Statement> {
    let rest = sql["ALTER TABLE".len()..].trim();
    let lower = rest.to_ascii_lowercase();
    if rest.is_empty()
        || !(lower.contains(" add ")
            || lower.contains(" drop ")
            || lower.contains(" rename ")
            || lower.contains(" alter "))
    {
        return Err(Error::Parse(
            "ALTER TABLE requires ADD, DROP, RENAME, or ALTER action".into(),
        ));
    }
    Ok(Statement::ParsedOnly {
        kind: ParsedOnlyStatementKind::AlterTable,
        sql: sql.to_string(),
    })
}

fn parse_attach_or_detach(
    sql: &str,
    kind: ParsedOnlyStatementKind,
    prefix: &str,
) -> Result<Statement> {
    let rest = sql[prefix.len()..].trim();
    if rest.is_empty() {
        return Err(Error::Parse(format!(
            "{} requires a database",
            kind.as_str()
        )));
    }
    Ok(Statement::ParsedOnly {
        kind,
        sql: sql.to_string(),
    })
}

fn parse_create_view(sql: &str) -> Result<Statement> {
    if find_ci(sql, " AS ").is_none() {
        return Err(Error::Parse("CREATE VIEW requires AS SELECT".into()));
    }
    Ok(Statement::ParsedOnly {
        kind: ParsedOnlyStatementKind::CreateView,
        sql: sql.to_string(),
    })
}

fn parse_create_trigger(sql: &str) -> Result<Statement> {
    if find_ci(sql, " BEGIN ").is_none() || !sql.trim_end().to_ascii_lowercase().ends_with("end") {
        return Err(Error::Parse("CREATE TRIGGER requires BEGIN ... END".into()));
    }
    Ok(Statement::ParsedOnly {
        kind: ParsedOnlyStatementKind::CreateTrigger,
        sql: sql.to_string(),
    })
}

fn parse_create_virtual_table(sql: &str) -> Result<Statement> {
    if find_ci(sql, " USING ").is_none() {
        return Err(Error::Parse("CREATE VIRTUAL TABLE requires USING".into()));
    }
    Ok(Statement::ParsedOnly {
        kind: ParsedOnlyStatementKind::CreateVirtualTable,
        sql: sql.to_string(),
    })
}

fn parse_drop(sql: &str, kind: ParsedOnlyStatementKind, prefix: &str) -> Result<Statement> {
    let rest = sql[prefix.len()..].trim();
    if rest.is_empty() {
        return Err(Error::Parse(format!("{} requires a name", kind.as_str())));
    }
    Ok(Statement::ParsedOnly {
        kind,
        sql: sql.to_string(),
    })
}

fn parse_advanced_insert(sql: &str) -> Result<Statement> {
    parse_advanced_insert_kind(sql, ParsedOnlyStatementKind::Insert, "INSERT INTO")
}

fn parse_advanced_insert_kind(
    sql: &str,
    kind: ParsedOnlyStatementKind,
    prefix: &str,
) -> Result<Statement> {
    let rest = sql[prefix.len()..].trim();
    if rest.is_empty() {
        return Err(Error::Parse(format!("{} requires a target", kind.as_str())));
    }
    Ok(Statement::ParsedOnly {
        kind,
        sql: sql.to_string(),
    })
}

fn is_advanced_create_table(sql: &str) -> bool {
    let lower = sql.to_ascii_lowercase();
    lower.contains(" if not exists")
        || lower.contains(" without rowid")
        || lower.ends_with(" strict")
        || lower.contains(" as select")
        || lower.contains(" constraint ")
        || lower.contains(" foreign key")
        || lower.contains(" primary key (")
        || lower.contains(" check ")
        || lower.contains(" check(")
        || lower.contains(" unique")
        || lower.contains(" default ")
        || lower.contains(" not null")
        || lower.contains(" references ")
        || lower.contains(" generated ")
        || lower.contains(" collate ")
        || lower.contains(" autoincrement")
}

fn is_advanced_create_index(sql: &str) -> bool {
    let lower = sql.to_ascii_lowercase();
    if lower.contains(" if not exists")
        || lower.contains(" where ")
        || lower.contains(" collate ")
        || lower.contains(" desc")
        || lower.contains(" asc")
    {
        return true;
    }

    let Some(open) = sql.find('(') else {
        return false;
    };
    let Some(close) = sql.rfind(')') else {
        return false;
    };
    let inner = &sql[open + 1..close];
    split_top_level(inner, ',').len() != 1
        || inner.contains('(')
        || inner.split_whitespace().count() > 1
}

fn is_advanced_insert(sql: &str) -> bool {
    is_advanced_insert_rest(sql["INSERT INTO".len()..].trim())
}

fn is_advanced_insert_rest(rest: &str) -> bool {
    if rest.is_empty() {
        return false;
    }
    let lower = rest.to_ascii_lowercase();
    if lower.contains(" default values")
        || lower.ends_with("default values")
        || lower.contains(" on conflict")
        || lower.contains(" returning ")
    {
        return true;
    }

    let Some(values_pos) = find_ci(rest, "VALUES") else {
        return starts_with_ci(rest, "SELECT")
            || starts_with_ci(rest, "WITH")
            || find_ci(rest, " SELECT ").is_some()
            || find_ci(rest, " WITH ").is_some();
    };
    has_multiple_top_level_parenthesized_groups(rest[values_pos + "VALUES".len()..].trim())
}

fn is_advanced_select(sql: &str) -> bool {
    let rest = sql["SELECT".len()..].trim();
    if rest.is_empty() {
        return false;
    }
    let lower = rest.to_ascii_lowercase();
    if starts_with_ci(rest, "DISTINCT")
        || starts_with_ci(rest, "ALL")
        || lower.contains(" union ")
        || lower.contains(" intersect ")
        || lower.contains(" except ")
        || lower.contains(" window ")
    {
        return true;
    }

    let Some(from_pos) = find_ci(rest, " FROM ") else {
        return is_supported_fromless_select(rest);
    };
    if is_advanced_projection(&rest[..from_pos]) {
        return true;
    }

    let from_rest = rest[from_pos + " FROM ".len()..].trim();
    if find_ci(from_rest, " GROUP BY ").is_some()
        || find_ci(from_rest, " HAVING ").is_some()
        || find_ci(from_rest, " OFFSET ").is_some()
        || find_ci(from_rest, " WINDOW ").is_some()
    {
        return true;
    }

    let where_pos = find_ci(from_rest, " WHERE ");
    let order_pos = find_ci(from_rest, " ORDER BY ");
    let limit_pos = find_ci(from_rest, " LIMIT ");
    let table_end = [where_pos, order_pos, limit_pos]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(from_rest.len());
    if is_advanced_from(&from_rest[..table_end]) {
        return true;
    }

    if let Some(pos) = where_pos {
        let start = pos + " WHERE ".len();
        let end = [order_pos, limit_pos]
            .into_iter()
            .flatten()
            .filter(|candidate| *candidate > pos)
            .min()
            .unwrap_or(from_rest.len());
        if is_advanced_where_clause(from_rest[start..end].trim()) {
            return true;
        }
    }

    if let Some(pos) = order_pos {
        let start = pos + " ORDER BY ".len();
        let end = limit_pos
            .filter(|candidate| *candidate > pos)
            .unwrap_or(from_rest.len());
        if is_advanced_order_clause(from_rest[start..end].trim()) {
            return true;
        }
    }

    false
}

fn is_advanced_update(sql: &str) -> bool {
    let rest = sql["UPDATE".len()..].trim();
    if rest.is_empty() {
        return false;
    }
    let lower = rest.to_ascii_lowercase();
    if lower.contains(" from ")
        || lower.contains(" returning ")
        || lower.contains(" order by ")
        || lower.contains(" limit ")
    {
        return true;
    }

    let Some(set_pos) = find_ci(rest, " SET ") else {
        return false;
    };
    let after_set = rest[set_pos + " SET ".len()..].trim();
    let where_pos = find_ci(after_set, " WHERE ");
    let assignments_part = where_pos
        .map(|pos| &after_set[..pos])
        .unwrap_or(after_set)
        .trim();
    if assignments_part.is_empty() {
        return false;
    }
    for assignment in split_top_level(assignments_part, ',') {
        let Some((_, value)) = split_once_top_level(&assignment, '=') else {
            return false;
        };
        if parse_value(value).is_err() {
            return true;
        }
    }

    where_pos
        .map(|pos| is_advanced_where_clause(after_set[pos + " WHERE ".len()..].trim()))
        .unwrap_or(false)
}

fn is_advanced_delete(sql: &str) -> bool {
    let rest = sql["DELETE FROM".len()..].trim();
    if rest.is_empty() {
        return false;
    }
    let lower = rest.to_ascii_lowercase();
    if lower.contains(" returning ")
        || lower.contains(" order by ")
        || lower.contains(" limit ")
        || lower.contains(" using ")
    {
        return true;
    }

    let where_pos = find_ci(rest, " WHERE ");
    let table = where_pos.map(|pos| &rest[..pos]).unwrap_or(rest).trim();
    if is_advanced_from(table) {
        return true;
    }
    where_pos
        .map(|pos| is_advanced_where_clause(rest[pos + " WHERE ".len()..].trim()))
        .unwrap_or(false)
}

fn is_supported_fromless_select(rest: &str) -> bool {
    let rest = rest.trim();
    !rest.is_empty() && !(rest.starts_with('*') && rest != "*")
}

fn is_advanced_projection(input: &str) -> bool {
    let input = input.trim();
    if input.is_empty() || input == "*" {
        return false;
    }
    let lower = input.to_ascii_lowercase();
    lower.contains(" as ")
        || lower.contains('(')
        || lower.contains(" + ")
        || lower.contains(" - ")
        || lower.contains(" * ")
        || lower.contains(" / ")
        || lower.contains(" || ")
        || lower.contains(" case ")
}

fn is_advanced_from(input: &str) -> bool {
    let input = input.trim();
    if input.is_empty() {
        return false;
    }
    let lower = input.to_ascii_lowercase();
    lower.contains(" join ")
        || lower.contains(',')
        || lower.contains(" as ")
        || lower.contains('(')
        || input.split_whitespace().count() > 1
}

fn is_advanced_where_clause(input: &str) -> bool {
    let input = input.trim();
    if input.is_empty() || parse_filter(input).is_ok() {
        return false;
    }
    if starts_with_ci(input, "MATCH") || starts_with_ci(input, "GEO_DISTANCE") {
        return false;
    }
    let lower = format!(" {} ", input.to_ascii_lowercase());
    lower.contains(" and ")
        || lower.contains(" or ")
        || lower.contains(" in ")
        || lower.contains(" is ")
        || lower.contains(" between ")
        || lower.contains(" like ")
        || lower.contains(" glob ")
        || lower.contains(" regexp ")
        || lower.contains(" exists ")
        || lower.contains(" not ")
        || input.contains('>')
        || input.contains('<')
        || input.contains("!=")
        || input.contains("<>")
        || split_once_top_level(input, '=').is_some()
}

fn is_advanced_order_clause(input: &str) -> bool {
    let input = input.trim();
    if input.is_empty() || parse_order(input).is_ok() {
        return false;
    }
    !starts_with_ci(input, "VECTOR_DISTANCE")
}

fn has_multiple_top_level_parenthesized_groups(input: &str) -> bool {
    let mut quote = None;
    let mut parens = 0usize;
    let mut groups = 0usize;

    for ch in input.chars() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' if parens == 0 => {
                groups += 1;
                if groups > 1 {
                    return true;
                }
                parens += 1;
            }
            '(' => parens += 1,
            ')' => parens = parens.saturating_sub(1),
            _ => {}
        }
    }

    false
}

/// - 判断文本是否表示事务开始语句。
/// - Checks whether the text represents a transaction-begin statement.
/// - 输入比较受方言影响，支持标准别名及部分 SQLite/PostgreSQL 变体。
/// - Comparison depends on dialect, covering standard aliases plus selected SQLite/PostgreSQL variants.
/// - 返回布尔值，不分配结果对象也不报错。
/// - Returns a boolean without allocating result objects or producing errors.
fn is_begin(sql: &str, dialect: SqlDialect) -> bool {
    sql.eq_ignore_ascii_case("BEGIN")
        || sql.eq_ignore_ascii_case("BEGIN TRANSACTION")
        || sql.eq_ignore_ascii_case("BEGIN WORK")
        || matches!(dialect, SqlDialect::Sqlite)
            && (sql.eq_ignore_ascii_case("BEGIN IMMEDIATE")
                || sql.eq_ignore_ascii_case("BEGIN EXCLUSIVE")
                || sql.eq_ignore_ascii_case("BEGIN DEFERRED"))
        || matches!(dialect, SqlDialect::PostgreSql) && sql.eq_ignore_ascii_case("BEGIN WORK")
}

/// - 判断文本是否表示事务提交语句。
/// - Checks whether the text represents a transaction-commit statement.
/// - 输入比较受方言影响，支持 `COMMIT` 及部分方言别名。
/// - Comparison depends on dialect, covering `COMMIT` and selected dialect aliases.
/// - 返回布尔值，不会失败且无副作用。
/// - Returns a boolean with no failure path and no side effects.
fn is_commit(sql: &str, dialect: SqlDialect) -> bool {
    sql.eq_ignore_ascii_case("COMMIT")
        || sql.eq_ignore_ascii_case("COMMIT TRANSACTION")
        || matches!(dialect, SqlDialect::Sqlite) && sql.eq_ignore_ascii_case("END")
        || matches!(dialect, SqlDialect::PostgreSql) && sql.eq_ignore_ascii_case("COMMIT WORK")
}

/// - 判断文本是否表示事务回滚语句。
/// - Checks whether the text represents a transaction-rollback statement.
/// - 输入比较受方言影响，当前支持标准写法和 PostgreSQL `ROLLBACK WORK`。
/// - Comparison depends on dialect, currently supporting the standard form and PostgreSQL `ROLLBACK WORK`.
/// - 返回布尔值，不会抛出解析错误。
/// - Returns a boolean and never raises a parse error.
fn is_rollback(sql: &str, dialect: SqlDialect) -> bool {
    sql.eq_ignore_ascii_case("ROLLBACK")
        || sql.eq_ignore_ascii_case("ROLLBACK TRANSACTION")
        || matches!(dialect, SqlDialect::PostgreSql) && sql.eq_ignore_ascii_case("ROLLBACK WORK")
}

/// - 解析 `CREATE TABLE` 语句中的表名与列定义。
/// - Parses the table name and column definitions from a `CREATE TABLE` statement.
/// - 输入必须包含成对括号、表名以及每列的名称和类型。
/// - Input must include matched parentheses, a table name, and a name/type pair for each column.
/// - 返回 `Statement::CreateTable`，或在结构非法时返回解析错误。
/// - Returns `Statement::CreateTable`, or a parse error when the structure is invalid.
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

/// - 解析普通或全文 `CREATE INDEX` 语句。
/// - Parses a regular or full-text `CREATE INDEX` statement.
/// - 输入必须包含 `ON table(column)` 结构；前缀决定 `fulltext` 标记。
/// - Input must include an `ON table(column)` shape; the prefix decides the `fulltext` flag.
/// - 返回 `Statement::CreateIndex`，或在目标结构缺失时返回错误。
/// - Returns `Statement::CreateIndex`, or an error when the target structure is missing.
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

/// - 解析 `INSERT INTO ... VALUES ...` 语句。
/// - Parses an `INSERT INTO ... VALUES ...` statement.
/// - 输入支持可选列清单，值列表必须位于单组外层括号中。
/// - Supports an optional column list, and the value list must be inside a single outer parenthesized group.
/// - 返回 `Statement::Insert`，或在列/值结构非法时返回解析错误。
/// - Returns `Statement::Insert`, or a parse error when the column or value structure is invalid.
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

/// - 解析 `SELECT` 语句中的投影、表名和可选子句。
/// - Parses projection, table name, and optional clauses from a `SELECT` statement.
/// - 输入要求包含 `FROM`，并按当前实现支持 `WHERE`、`ORDER BY` 与 `LIMIT`。
/// - Input requires `FROM`, and the current implementation supports `WHERE`, `ORDER BY`, and `LIMIT`.
/// - 返回 `Statement::Select`，或在任一子句不合法时返回解析错误。
/// - Returns `Statement::Select`, or a parse error when any clause is invalid.
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

/// - 解析 `UPDATE` 语句中的表名、赋值列表和可选过滤条件。
/// - Parses the table name, assignment list, and optional filter from an `UPDATE` statement.
/// - 输入必须包含 `SET`，赋值项需使用顶层 `=` 分隔并支持可选 `WHERE`。
/// - Input must include `SET`; assignments must use a top-level `=` separator and may include `WHERE`.
/// - 返回 `Statement::Update`，或在赋值/过滤结构非法时返回错误。
/// - Returns `Statement::Update`, or an error when assignment or filter structure is invalid.
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

/// - 解析 `DELETE FROM` 语句及其可选过滤条件。
/// - Parses a `DELETE FROM` statement and its optional filter.
/// - 输入支持可选 `WHERE`，未提供时表示删除整表匹配集。
/// - Supports an optional `WHERE`; when omitted, it represents deleting the full table match set.
/// - 返回 `Statement::Delete`，或在过滤条件非法时返回错误。
/// - Returns `Statement::Delete`, or an error when the filter clause is invalid.
fn parse_delete(sql: &str) -> Result<Statement> {
    let rest = sql["DELETE FROM".len()..].trim();
    let where_pos = find_ci(rest, " WHERE ");
    let table = normalize_identifier(where_pos.map(|pos| &rest[..pos]).unwrap_or(rest));
    if table.is_empty() {
        return Err(Error::Parse("DELETE requires a table name".into()));
    }
    let filter = if let Some(pos) = where_pos {
        Some(parse_filter(rest[pos + " WHERE ".len()..].trim())?)
    } else {
        None
    };

    Ok(Statement::Delete { table, filter })
}

/// - 解析 `SELECT` 投影部分为全列或列名列表。
/// - Parses the `SELECT` projection into all-columns or an explicit column list.
/// - 输入 `*` 直接表示全列，否则按顶层逗号拆分并规范化标识符。
/// - Input `*` means all columns directly; otherwise it is split by top-level commas and identifiers are normalized.
/// - 返回 `Projection`，或在出现空列名时返回解析错误。
/// - Returns a `Projection`, or a parse error when an empty column name appears.
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

/// - 解析当前支持的 `WHERE` 过滤表达式。
/// - Parses the currently supported `WHERE` filter expressions.
/// - 输入仅支持 `MATCH`、`GEO_DISTANCE` 和顶层等值比较三类模式。
/// - Input only supports `MATCH`, `GEO_DISTANCE`, and top-level equality comparison patterns.
/// - 返回对应 `Filter`，或在模式不支持/参数非法时返回错误。
/// - Returns the corresponding `Filter`, or an error for unsupported patterns or invalid arguments.
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

/// - 解析当前支持的 `ORDER BY` 排序表达式。
/// - Parses the currently supported `ORDER BY` sort expression.
/// - 输入仅支持 `VECTOR_DISTANCE(column, vector)`，并可附带 `ASC`/`DESC`。
/// - Input only supports `VECTOR_DISTANCE(column, vector)` and may include `ASC` or `DESC`.
/// - 返回 `Order::VectorDistance`，或在参数类型/形状非法时返回错误。
/// - Returns `Order::VectorDistance`, or an error when argument types or shape are invalid.
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

/// - 将字面量文本解析为内部 `Value`。
/// - Parses literal text into the internal `Value` type.
/// - 输入支持空值、布尔、字符串、点、向量、浮点与整数，匹配顺序固定。
/// - Supports null, booleans, strings, points, vectors, floats, and integers, with a fixed matching order.
/// - 返回对应值对象，或在格式非法时返回解析错误。
/// - Returns the corresponding value object, or a parse error when the format is invalid.
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

/// - 解析成对引号包裹的字符串字面量。
/// - Parses a string literal wrapped in matching quotes.
/// - 输入必须以同类单引号或双引号包裹；内部双写引号会被还原。
/// - Input must be wrapped by matching single or double quotes; doubled inner quotes are unescaped.
/// - 返回去壳后的字符串，或在不是合法引号字符串时返回 `None`。
/// - Returns the unwrapped string, or `None` when the input is not a valid quoted string.
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

/// - 解析并校验有限 `f64` 浮点数。
/// - Parses and validates a finite `f64` floating-point number.
/// - 输入会先裁剪空白；`NaN` 和无穷值会被拒绝。
/// - Trims the input first; `NaN` and infinite values are rejected.
/// - 返回有限浮点值，或返回解析错误。
/// - Returns a finite floating-point value, or a parse error.
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

/// - 解析并校验有限 `f32` 浮点数。
/// - Parses and validates a finite `f32` floating-point number.
/// - 输入会先裁剪空白；用于向量元素并拒绝 `NaN` 与无穷值。
/// - Trims the input first; used for vector elements and rejects `NaN` and infinite values.
/// - 返回有限浮点值，或返回解析错误。
/// - Returns a finite floating-point value, or a parse error.
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

/// - 提取外层括号包裹内容的内部文本。
/// - Extracts the inner text from an outer parenthesized body.
/// - 输入必须整体以一对圆括号包裹；错误消息会携带调用上下文名。
/// - Input must be fully wrapped by one pair of parentheses; error messages include the caller context name.
/// - 返回括号内部的切片，或在缺少括号时返回解析错误。
/// - Returns the slice inside the parentheses, or a parse error when they are missing.
fn parenthesized_body<'a>(input: &'a str, context: &str) -> Result<&'a str> {
    let input = input.trim();
    if !input.starts_with('(') || !input.ends_with(')') {
        return Err(Error::Parse(format!("{context} requires parentheses")));
    }
    Ok(&input[1..input.len() - 1])
}

/// - 在顶层作用域查找首个分隔符并拆成两段。
/// - Finds the first delimiter at top level and splits the input into two parts.
/// - 会跳过引号、圆括号和方括号内部的分隔符；仅识别最外层字符。
/// - Skips delimiters inside quotes, parentheses, and brackets; only outermost characters are recognized.
/// - 返回左右切片元组，未找到时返回 `None`，无分配副作用。
/// - Returns a tuple of left/right slices, or `None` when not found, with no allocation side effects.
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

/// - 按顶层分隔符拆分字符串为多个片段。
/// - Splits a string into multiple segments by a top-level delimiter.
/// - 会忽略引号、圆括号和方括号内部的分隔符，并裁剪每段首尾空白。
/// - Ignores delimiters inside quotes, parentheses, and brackets, and trims each segment.
/// - 返回新分配的字符串列表；即使输入为空也会产生一个片段。
/// - Returns a newly allocated string list; even empty input yields one segment.
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

/// - 以 ASCII 忽略大小写方式判断前缀匹配。
/// - Checks prefix matching with ASCII case insensitivity.
/// - 输入前缀长度必须不超过原串长度；比较仅覆盖相同长度前缀。
/// - The prefix length must not exceed the input length; comparison only covers the same-length prefix.
/// - 返回布尔值，不分配新字符串也不报错。
/// - Returns a boolean without allocating new strings or producing errors.
fn starts_with_ci(input: &str, prefix: &str) -> bool {
    input
        .get(..prefix.len())
        .is_some_and(|value| value.eq_ignore_ascii_case(prefix))
}

/// - 以 ASCII 忽略大小写方式查找子串位置。
/// - Finds a substring position with ASCII case-insensitive matching.
/// - 当前实现会对原串和目标串生成小写副本后再查找。
/// - The current implementation lowercases both haystack and needle before searching.
/// - 返回首个匹配起始下标，未命中时返回 `None`。
/// - Returns the first match start index, or `None` when no match exists.
fn find_ci(input: &str, needle: &str) -> Option<usize> {
    input
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

/// - 以 ASCII 忽略大小写方式剥离后缀。
/// - Strips a suffix using ASCII case-insensitive matching.
/// - 输入后缀必须完整落在原串尾部；成功时保留原始前缀切片。
/// - The suffix must fully occupy the end of the input; on success the original prefix slice is preserved.
/// - 返回去除后缀后的切片，未匹配时返回 `None`。
/// - Returns the slice without the suffix, or `None` when it does not match.
fn strip_suffix_ci<'a>(input: &'a str, suffix: &str) -> Option<&'a str> {
    input
        .get(input.len().checked_sub(suffix.len())?..)
        .filter(|value| value.eq_ignore_ascii_case(suffix))
        .map(|_| &input[..input.len() - suffix.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// - 校验事务相关语句能解析为基础语句变体。
    /// - Verifies transaction statements parse into the basic statement variants.
    /// - 覆盖空白与大小写差异；使用默认 FSQL 方言。
    /// - Covers whitespace and casing differences while using the default FSQL dialect.
    /// - 无返回值；测试在解析或断言失败时 panic。
    /// - Returns no value; the test panics on parse or assertion failure.
    #[test]
    fn parses_transaction_statements() {
        assert_eq!(parse_sql(" begin transaction ;").unwrap(), Statement::Begin);
        assert_eq!(parse_sql("COMMIT").unwrap(), Statement::Commit);
        assert_eq!(parse_sql("rollback").unwrap(), Statement::Rollback);
    }

    /// - 校验不同方言的事务别名解析行为。
    /// - Verifies dialect-specific transaction alias parsing behavior.
    /// - 覆盖 SQLite 与 PostgreSQL 变体，并确认 FSQL 会拒绝不支持别名。
    /// - Covers SQLite and PostgreSQL variants and confirms FSQL rejects unsupported aliases.
    /// - 无返回值；测试依赖断言检查解析结果与错误路径。
    /// - Returns no value; the test relies on assertions for successful and error paths.
    #[test]
    fn parses_dialect_transaction_aliases() {
        assert_eq!(
            parse_sql_with_dialect("BEGIN IMMEDIATE", SqlDialect::Sqlite).unwrap(),
            Statement::Begin
        );
        assert_eq!(
            parse_sql_with_dialect("END", SqlDialect::Sqlite).unwrap(),
            Statement::Commit
        );
        assert_eq!(
            parse_sql_with_dialect("BEGIN WORK", SqlDialect::PostgreSql).unwrap(),
            Statement::Begin
        );
        assert_eq!(
            parse_sql_with_dialect("COMMIT WORK", SqlDialect::PostgreSql).unwrap(),
            Statement::Commit
        );
        assert_eq!(
            parse_sql_with_dialect("ROLLBACK WORK", SqlDialect::PostgreSql).unwrap(),
            Statement::Rollback
        );
        assert!(parse_sql_with_dialect("END", SqlDialect::Fsql).is_err());
    }

    /// - 校验建表与建索引语句的解析结果。
    /// - Verifies parsing results for create-table and create-index statements.
    /// - 覆盖主键列、普通索引与全文索引三条路径。
    /// - Covers primary-key columns, regular indexes, and full-text indexes.
    /// - 无返回值；测试通过结构化断言比较完整 AST。
    /// - Returns no value; the test compares full AST structures via assertions.
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

    /// - 校验 `INSERT` 值列表中多种字面量类型的解析。
    /// - Verifies parsing of multiple literal kinds in an `INSERT` value list.
    /// - 覆盖列清单、转义字符串、布尔、浮点、向量与地理点。
    /// - Covers a column list, escaped strings, booleans, floats, vectors, and geographic points.
    /// - 无返回值；测试通过完整语句断言验证结果。
    /// - Returns no value; the test validates the result via full statement assertions.
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

    /// - 校验查询、更新和删除语句的组合解析路径。
    /// - Verifies combined parsing paths for select, update, and delete statements.
    /// - 覆盖全文过滤、向量排序、`EXPLAIN`、赋值列表与地理过滤。
    /// - Covers full-text filtering, vector ordering, `EXPLAIN`, assignment lists, and geo filters.
    /// - 无返回值；测试通过精确 AST 断言验证每条语句。
    /// - Returns no value; the test validates each statement with exact AST assertions.
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
            parse_sql("EXPLAIN SELECT * FROM docs WHERE id = 1").unwrap(),
            Statement::Explain(Box::new(Statement::Select {
                table: "docs".into(),
                projection: Projection::All,
                filter: Some(Filter::Equals("id".into(), Value::Integer(1))),
                order: None,
                limit: None,
            }))
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

    fn assert_parsed_only(sql: &str, expected: ParsedOnlyStatementKind) {
        match parse_sql(sql).unwrap() {
            Statement::ParsedOnly { kind, sql: stored } => {
                assert_eq!(kind, expected);
                assert_eq!(stored, sql.trim().trim_end_matches(';').trim());
            }
            other => panic!("expected ParsedOnly for {sql}, got {other:?}"),
        }
    }

    /// - 校验 SQLite/标准 SQL 常见语句族已经能进入语法层。
    /// - Verifies that common SQLite/standard SQL statement families enter the language layer.
    /// - 覆盖 DDL、DML、CTE、事务保存点、PRAGMA、复杂查询和 UPSERT 等语句形状。
    /// - Covers DDL, DML, CTEs, savepoints, PRAGMA, complex queries, and UPSERT-like statement shapes.
    /// - 无返回值；暂未执行的语义以 `ParsedOnly` 明确记录原始 SQL。
    /// - Returns no value; not-yet-executed semantics are explicitly represented as `ParsedOnly` with original SQL.
    #[test]
    fn parses_advanced_sql_statement_families_as_parsed_only() {
        let parsed_only = [
            (
                "ALTER TABLE users ADD COLUMN email TEXT",
                ParsedOnlyStatementKind::AlterTable,
            ),
            ("ANALYZE main", ParsedOnlyStatementKind::Analyze),
            (
                "ATTACH DATABASE 'tenant.db' AS tenant",
                ParsedOnlyStatementKind::Attach,
            ),
            ("DETACH DATABASE tenant", ParsedOnlyStatementKind::Detach),
            (
                "CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY)",
                ParsedOnlyStatementKind::CreateTable,
            ),
            (
                "CREATE TEMP TABLE scratch (id INTEGER)",
                ParsedOnlyStatementKind::CreateTable,
            ),
            (
                "CREATE TABLE audit (id INTEGER, CONSTRAINT audit_id CHECK(id > 0))",
                ParsedOnlyStatementKind::CreateTable,
            ),
            (
                "CREATE TABLE child (id INTEGER, FOREIGN KEY(id) REFERENCES parent(id))",
                ParsedOnlyStatementKind::CreateTable,
            ),
            (
                "CREATE TABLE generated (name TEXT NOT NULL DEFAULT 'x' COLLATE nocase)",
                ParsedOnlyStatementKind::CreateTable,
            ),
            (
                "CREATE TABLE strict_t (id INTEGER) STRICT",
                ParsedOnlyStatementKind::CreateTable,
            ),
            (
                "CREATE TABLE report AS SELECT * FROM users",
                ParsedOnlyStatementKind::CreateTable,
            ),
            (
                "CREATE VIEW active_users AS SELECT * FROM users WHERE active = true",
                ParsedOnlyStatementKind::CreateView,
            ),
            (
                "CREATE TRIGGER users_ai AFTER INSERT ON users BEGIN SELECT 1; END",
                ParsedOnlyStatementKind::CreateTrigger,
            ),
            (
                "CREATE VIRTUAL TABLE docs USING fts5(body)",
                ParsedOnlyStatementKind::CreateVirtualTable,
            ),
            (
                "CREATE UNIQUE INDEX users_email_uq ON users(email)",
                ParsedOnlyStatementKind::CreateIndex,
            ),
            (
                "CREATE INDEX IF NOT EXISTS users_age ON users(age)",
                ParsedOnlyStatementKind::CreateIndex,
            ),
            (
                "CREATE INDEX users_name_lower ON users(lower(name))",
                ParsedOnlyStatementKind::CreateIndex,
            ),
            (
                "CREATE INDEX users_name_age ON users(name, age)",
                ParsedOnlyStatementKind::CreateIndex,
            ),
            (
                "CREATE INDEX users_active ON users(active) WHERE active = true",
                ParsedOnlyStatementKind::CreateIndex,
            ),
            (
                "DROP TABLE IF EXISTS users",
                ParsedOnlyStatementKind::DropTable,
            ),
            (
                "DROP INDEX IF EXISTS users_age",
                ParsedOnlyStatementKind::DropIndex,
            ),
            (
                "DROP TRIGGER IF EXISTS users_ai",
                ParsedOnlyStatementKind::DropTrigger,
            ),
            (
                "DROP VIEW IF EXISTS active_users",
                ParsedOnlyStatementKind::DropView,
            ),
            (
                "INSERT OR IGNORE INTO users VALUES (1)",
                ParsedOnlyStatementKind::Insert,
            ),
            (
                "INSERT INTO users DEFAULT VALUES",
                ParsedOnlyStatementKind::Insert,
            ),
            (
                "INSERT INTO users VALUES (1, 'Ada'), (2, 'Grace')",
                ParsedOnlyStatementKind::Insert,
            ),
            (
                "INSERT INTO users SELECT * FROM old_users",
                ParsedOnlyStatementKind::Insert,
            ),
            (
                "INSERT INTO users(id) VALUES (1) ON CONFLICT(id) DO NOTHING",
                ParsedOnlyStatementKind::Insert,
            ),
            (
                "REPLACE INTO users VALUES (1, 'Ada')",
                ParsedOnlyStatementKind::Replace,
            ),
            (
                "SELECT DISTINCT name FROM users",
                ParsedOnlyStatementKind::Select,
            ),
            (
                "SELECT count(*) FROM users",
                ParsedOnlyStatementKind::Select,
            ),
            ("SELECT 1", ParsedOnlyStatementKind::Select),
            ("SELECT * FROM users AS u", ParsedOnlyStatementKind::Select),
            (
                "SELECT * FROM users JOIN orgs ON users.org_id = orgs.id",
                ParsedOnlyStatementKind::Select,
            ),
            (
                "SELECT * FROM users WHERE age > 18 ORDER BY name",
                ParsedOnlyStatementKind::Select,
            ),
            (
                "SELECT * FROM users WHERE name LIKE 'A%'",
                ParsedOnlyStatementKind::Select,
            ),
            (
                "SELECT * FROM users GROUP BY active HAVING count(*) > 1",
                ParsedOnlyStatementKind::Select,
            ),
            (
                "SELECT * FROM users UNION SELECT * FROM archived_users",
                ParsedOnlyStatementKind::Select,
            ),
            (
                "UPDATE OR REPLACE users SET name = 'Ada' WHERE id = 1",
                ParsedOnlyStatementKind::Update,
            ),
            (
                "UPDATE users SET age = age + 1 WHERE age > 18 RETURNING id",
                ParsedOnlyStatementKind::Update,
            ),
            (
                "UPDATE users SET name = other_name WHERE id = 1",
                ParsedOnlyStatementKind::Update,
            ),
            (
                "DELETE FROM users WHERE age > 18 RETURNING id",
                ParsedOnlyStatementKind::Delete,
            ),
            (
                "DELETE FROM users AS u WHERE u.age > 18",
                ParsedOnlyStatementKind::Delete,
            ),
            ("PRAGMA journal_mode = WAL", ParsedOnlyStatementKind::Pragma),
            ("REINDEX users_age", ParsedOnlyStatementKind::Reindex),
            ("SAVEPOINT batch", ParsedOnlyStatementKind::Savepoint),
            ("RELEASE batch", ParsedOnlyStatementKind::Release),
            ("ROLLBACK TO batch", ParsedOnlyStatementKind::RollbackTo),
            ("VACUUM main", ParsedOnlyStatementKind::Vacuum),
            ("VALUES (1), (2)", ParsedOnlyStatementKind::Values),
            (
                "WITH recent AS (SELECT 1) SELECT * FROM recent",
                ParsedOnlyStatementKind::With,
            ),
        ];

        for (sql, kind) in parsed_only {
            assert_parsed_only(sql, kind);
        }

        assert_eq!(
            parse_sql("EXPLAIN QUERY PLAN SELECT * FROM users WHERE age > 18").unwrap(),
            Statement::Explain(Box::new(Statement::ParsedOnly {
                kind: ParsedOnlyStatementKind::Select,
                sql: "SELECT * FROM users WHERE age > 18".into(),
            }))
        );
        assert_eq!(
            parse_sql("EXPLAIN INSERT INTO users VALUES (1)").unwrap(),
            Statement::Explain(Box::new(Statement::Insert {
                table: "users".into(),
                columns: None,
                values: vec![Value::Integer(1)],
            }))
        );
    }

    /// - 校验解析辅助函数对嵌套分隔符和边缘字面量的处理。
    /// - Verifies parser helpers against nested delimiters and edge literal cases.
    /// - 覆盖顶层拆分、大小写比较、后缀剥离、标识符规范化和值解析。
    /// - Covers top-level splitting, case-insensitive matching, suffix stripping, identifier normalization, and value parsing.
    /// - 无返回值；测试通过多条断言验证辅助逻辑。
    /// - Returns no value; the test validates helper behavior through multiple assertions.
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

    /// - 校验不合法 SQL 形状会被解析器拒绝。
    /// - Verifies invalid SQL shapes are rejected by the parser.
    /// - 输入覆盖空语句、缺失关键子句、坏括号和错误字面量等场景。
    /// - Covers empty statements, missing clauses, bad parentheses, and malformed literals.
    /// - 无返回值；测试通过逐条断言 `is_err()` 验证失败路径。
    /// - Returns no value; the test validates failure paths with per-case `is_err()` assertions.
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
            "DELETE FROM",
            "SELECT * FROM t WHERE MATCH(a)",
            "SELECT * FROM t WHERE MATCH(a, 1)",
            "SELECT * FROM t WHERE GEO_DISTANCE(a, POINT(0,0))",
            "SELECT * FROM t WHERE GEO_DISTANCE(a) < 1",
            "SELECT * FROM t WHERE GEO_DISTANCE(a, 1) < 1",
            "SELECT * FROM t WHERE GEO_DISTANCE(a, POINT(0,0)) < x",
            "SELECT * FROM t ORDER BY VECTOR_DISTANCE(a)",
            "SELECT * FROM t ORDER BY VECTOR_DISTANCE(a, 1)",
            "INSERT INTO t VALUES (POINT(1))",
            "INSERT INTO t VALUES (POINT(x, 0))",
            "INSERT INTO t VALUES (1.2.3)",
            "EXPLAIN",
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
