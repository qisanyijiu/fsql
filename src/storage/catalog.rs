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
    pub(crate) fn empty() -> Self {
        Self {
            tables: BTreeMap::new(),
        }
    }

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
    pub(crate) fn decode(input: &str) -> Result<Self> {
        Self::decode_with_options(input, TableRuntimeOptions::default())
    }

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

fn table_mut<'a>(current: &'a mut Option<Table>, message: &str) -> Result<&'a mut Table> {
    current
        .as_mut()
        .ok_or_else(|| Error::Execution(message.into()))
}

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
    fn decodes_empty_input_as_empty_catalog() {
        assert!(Catalog::decode("").unwrap().tables.is_empty());
        assert!(Catalog::decode("FSQ1\n\n").unwrap().tables.is_empty());
    }

    #[test]
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
