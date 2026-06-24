use std::collections::BTreeMap;

use crate::sql::ast::{Column, ColumnType};
use crate::storage::codec::{decode_string, decode_value, encode_string, encode_value};
use crate::storage::{Table, TableRuntimeOptions};
use crate::value::Row;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Catalog {
    pub(crate) tables: BTreeMap<String, Table>,
}

impl Catalog {
    /// - 创建一个不包含任何表定义或数据行的空目录。
    /// - Creates an empty catalog with no table definitions or rows.
    /// - 仅用于初始化内存态结构，不依赖外部输入或运行时选项。
    /// - Used only to initialize in-memory state and does not depend on external input or runtime options.
    /// - 返回可变更的空 `Catalog` 实例且不产生错误或副作用。
    /// - Returns a mutable empty `Catalog` instance with no errors or side effects.
    pub(crate) fn empty() -> Self {
        Self {
            tables: BTreeMap::new(),
        }
    }

    /// - 将当前目录中的表、索引和行编码为持久化文本格式。
    /// - Encodes the tables, indexes, and rows in the catalog into the persisted text format.
    /// - 依赖内部表结构已处于一致状态，并按固定指令顺序输出内容。
    /// - Assumes internal table state is consistent and emits directives in a fixed order.
    /// - 返回完整数据库文件内容；会分配并拼接字符串但不修改目录。
    /// - Returns the full database file contents; allocates and concatenates strings without mutating the catalog.
    pub(crate) fn encode(&self) -> String {
        let mut out = String::from("FSQ1\n");
        for table in self.tables.values() {
            out.push_str("TABLE\t");
            out.push_str(&encode_string(&table.name));
            out.push('\n');

            for column in &table.columns {
                out.push_str("COLUMN\t");
                out.push_str(&encode_string(&column.name));
                out.push('\t');
                out.push_str(column.ty.as_str());
                out.push('\t');
                out.push_str(if column.primary_key { "1" } else { "0" });
                out.push('\n');
            }

            for (name, column) in table.indexes_for_encoding() {
                out.push_str("INDEX\t");
                out.push_str(&encode_string(name));
                out.push('\t');
                out.push_str(&encode_string(column));
                out.push('\n');
            }

            for (name, column) in table.fulltext_indexes_for_encoding() {
                out.push_str("FTS\t");
                out.push_str(&encode_string(name));
                out.push('\t');
                out.push_str(&encode_string(column));
                out.push('\n');
            }

            for (row_id, row) in &table.rows {
                out.push_str("ROW\t");
                out.push_str(&row_id.to_string());
                for (column, value) in row {
                    out.push('\t');
                    out.push_str(&encode_string(column));
                    out.push('=');
                    out.push_str(&encode_value(value));
                }
                out.push('\n');
            }
            out.push_str("ENDTABLE\n");
        }
        out
    }

    #[cfg(test)]
    /// - 为测试场景从编码文本恢复目录，使用默认运行时选项。
    /// - Restores a catalog from encoded text for tests using default runtime options.
    /// - 输入必须符合持久化文件格式；该辅助函数仅在 `#[cfg(test)]` 下可用。
    /// - The input must follow the persisted file format; this helper is available only under `#[cfg(test)]`.
    /// - 返回解码后的 `Catalog` 或格式错误；不会引入额外测试夹具副作用。
    /// - Returns the decoded `Catalog` or a format error with no extra fixture side effects.
    pub(crate) fn decode(input: &str) -> Result<Self> {
        Self::decode_with_options(input, TableRuntimeOptions::default())
    }

    /// - 按给定运行时选项解析数据库文本并重建目录与索引状态。
    /// - Parses database text with the provided runtime options and rebuilds catalog and index state.
    /// - 输入必须使用支持的 `FSQ1` 指令格式，且表块、列定义和行字段需要保持完整一致。
    /// - The input must use the supported `FSQ1` directive format, and table blocks, column definitions, and row fields must remain internally consistent.
    /// - 返回完整 `Catalog`；遇到格式错误、重复主键或索引重建失败时返回执行错误。
    /// - Returns the fully reconstructed `Catalog`; yields execution errors for malformed input, duplicate primary keys, or index rebuild failures.
    pub(crate) fn decode_with_options(input: &str, options: TableRuntimeOptions) -> Result<Self> {
        let mut lines = input.lines();
        match lines.next() {
            Some("FSQ1") => {}
            Some(_) => return Err(Error::Execution("unsupported database file format".into())),
            None => return Ok(Self::empty()),
        }

        let mut catalog = Self::empty();
        let mut current: Option<Table> = None;

        for line in lines {
            if line.is_empty() {
                continue;
            }

            let parts = line.split('\t').collect::<Vec<_>>();
            match parts[0] {
                "TABLE" => {
                    if current.is_some() {
                        return Err(Error::Execution(
                            "nested table block in database file".into(),
                        ));
                    }
                    let name = decode_string(required_part(&parts, 1, "table name")?)?;
                    current = Some(Table::new(name, Vec::new()).expect("empty table is valid"));
                }
                "COLUMN" => {
                    let table = table_mut(&mut current, "column outside table block")?;
                    let name = decode_string(required_part(&parts, 1, "column name")?)?;
                    if table.columns.iter().any(|column| column.name == name) {
                        return Err(Error::Execution("duplicate column in file".into()));
                    }
                    let primary_key = required_part(&parts, 3, "primary flag")? == "1";
                    if primary_key && table.primary_key.is_some() {
                        return Err(Error::Execution("multiple primary keys in file".into()));
                    }
                    if primary_key {
                        table.primary_key = Some(name.clone());
                    }
                    table.columns.push(Column {
                        name,
                        ty: ColumnType::parse(required_part(&parts, 2, "column type")?)?,
                        primary_key,
                    });
                }
                "INDEX" => {
                    let table = table_mut(&mut current, "index outside table block")?;
                    table.add_persisted_index(
                        decode_string(required_part(&parts, 1, "index name")?)?,
                        decode_string(required_part(&parts, 2, "index column")?)?,
                    );
                }
                "FTS" => {
                    let table = table_mut(&mut current, "fts outside table block")?;
                    table.add_persisted_fulltext_index(
                        decode_string(required_part(&parts, 1, "fts name")?)?,
                        decode_string(required_part(&parts, 2, "fts column")?)?,
                    );
                }
                "ROW" => {
                    let table = table_mut(&mut current, "row outside table block")?;
                    let row_id = required_part(&parts, 1, "row id")?
                        .parse::<crate::storage::RowId>()
                        .map_err(|_| Error::Execution("invalid row id in file".into()))?;
                    let mut row = Row::new();
                    for field in parts.iter().skip(2) {
                        let (column, value) = field
                            .split_once('=')
                            .ok_or_else(|| Error::Execution("invalid row field in file".into()))?;
                        row.insert(decode_string(column)?, decode_value(value)?);
                    }
                    table.next_row_id = table.next_row_id.max(row_id + 1);
                    table.rows.insert(row_id, row);
                }
                "ENDTABLE" => {
                    let mut table = current
                        .take()
                        .ok_or_else(|| Error::Execution("endtable without table".into()))?;
                    table.rebuild_indexes_with_options(options)?;
                    catalog.tables.insert(table.name.clone(), table);
                }
                other => {
                    return Err(Error::Execution(format!(
                        "unknown database file directive {other}"
                    )));
                }
            }
        }

        if current.is_some() {
            return Err(Error::Execution("unterminated table block".into()));
        }
        Ok(catalog)
    }
}

/// - 取得当前正在解码的表，以便向其追加列、索引或行。
/// - Retrieves the table currently being decoded so callers can append columns, indexes, or rows.
/// - `current` 必须包含活动表；否则使用调用方提供的消息构造执行错误。
/// - `current` must contain an active table; otherwise the caller-provided message is turned into an execution error.
/// - 返回对当前表的可变引用，或在缺失表上下文时返回错误。
/// - Returns a mutable reference to the current table, or an error when no table context exists.
fn table_mut<'a>(current: &'a mut Option<Table>, message: &str) -> Result<&'a mut Table> {
    current
        .as_mut()
        .ok_or_else(|| Error::Execution(message.into()))
}

/// - 读取并校验一条持久化指令中的必需字段。
/// - Reads and validates a required field from a persisted directive.
/// - `index` 必须落在切分后的字段范围内；`label` 仅用于构造清晰错误信息。
/// - `index` must be within the split field range; `label` is used only to build a clear error message.
/// - 返回对应字段切片；缺失时返回带标签的执行错误。
/// - Returns the requested field slice; yields a labeled execution error when it is missing.
fn required_part<'a>(parts: &'a [&str], index: usize, label: &str) -> Result<&'a str> {
    parts
        .get(index)
        .copied()
        .ok_or_else(|| Error::Execution(format!("missing {label} in database file")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    /// - 构造一个带样例数据与索引的 `users` 测试夹具。
    /// - Builds a `users` fixture with sample rows and indexes.
    /// - 供编解码往返场景复用。
    /// - Reused by encode/decode round-trip scenarios.
    fn table() -> Table {
        let mut table = Table::new(
            "users".into(),
            vec![
                Column {
                    name: "id".into(),
                    ty: ColumnType::Integer,
                    primary_key: true,
                },
                Column {
                    name: "name".into(),
                    ty: ColumnType::Text,
                    primary_key: false,
                },
            ],
        )
        .unwrap();
        table
            .insert(None, vec![Value::Integer(1), Value::Text("Ada".into())])
            .unwrap();
        table
            .create_index("users_name".into(), "name".into())
            .unwrap();
        table
            .create_fulltext_index("users_name_fts".into(), "name".into())
            .unwrap();
        table
    }

    #[test]
    /// - 验证目录编解码往返后仍保留查询结果。
    /// - Verifies catalog round-tripping preserves query results.
    /// - 场景聚焦已插入行与索引查询可用性。
    /// - The scenario focuses on inserted rows and indexed query availability.
    fn round_trips_catalog() {
        let mut catalog = Catalog::empty();
        catalog.tables.insert("users".into(), table());
        let decoded = Catalog::decode(&catalog.encode()).unwrap();
        assert_eq!(decoded.tables["users"].rows.len(), 1);
        assert!(
            decoded.tables["users"]
                .select(
                    crate::sql::ast::Projection::All,
                    Some(crate::sql::ast::Filter::Equals(
                        "name".into(),
                        Value::Text("Ada".into())
                    )),
                    None,
                    None
                )
                .unwrap()
                .len()
                == 1
        );
    }

    #[test]
    /// - 验证空输入会解码为空目录。
    /// - Verifies empty inputs decode into an empty catalog.
    /// - 场景只检查不会生成任何表项。
    /// - The scenario checks only that no tables are produced.
    fn decodes_empty_input_as_empty_catalog() {
        assert!(Catalog::decode("").unwrap().tables.is_empty());
        assert!(Catalog::decode("FSQ1\n\n").unwrap().tables.is_empty());
    }

    #[test]
    /// - 验证损坏的目录文件会被拒绝。
    /// - Verifies malformed catalog files are rejected.
    /// - 场景覆盖坏文件头、缺失字段和非法行记录。
    /// - The scenario covers bad headers, missing fields, and invalid row records.
    fn rejects_malformed_catalog_files() {
        let malformed = [
            "BAD\n",
            "FSQ1\nTABLE\t7573657273\nTABLE\t6f74686572\n",
            "FSQ1\nCOLUMN\t6964\tinteger\t1\n",
            "FSQ1\nINDEX\t69\t63\n",
            "FSQ1\nFTS\t69\t63\n",
            "FSQ1\nROW\t1\n",
            "FSQ1\nENDTABLE\n",
            "FSQ1\nWHAT\n",
            "FSQ1\nTABLE\t7573657273\n",
            "FSQ1\nTABLE\n",
            "FSQ1\nTABLE\t7573657273\nCOLUMN\t6964\tinteger\n",
            "FSQ1\nTABLE\t7573657273\nCOLUMN\t6964\tinteger\t1\nCOLUMN\t6964\tinteger\t0\n",
            "FSQ1\nTABLE\t7573657273\nCOLUMN\t6964\tinteger\t1\nCOLUMN\t6f74686572\tinteger\t1\n",
            "FSQ1\nTABLE\t7573657273\nCOLUMN\t6964\tbad\t0\n",
            "FSQ1\nTABLE\t7573657273\nROW\tno\n",
            "FSQ1\nTABLE\t7573657273\nROW\t1\tbadfield\n",
            "FSQ1\nTABLE\t7573657273\nROW\t1\t6964=X:1\n",
            "FSQ1\nTABLE\t7573657273\nINDEX\t69\n",
            "FSQ1\nTABLE\t7573657273\nFTS\t69\n",
        ];
        for input in malformed {
            assert!(Catalog::decode(input).is_err(), "{input:?}");
        }
    }
}
