use std::collections::{BTreeMap, BTreeSet};

use crate::identifier::normalize_identifier;
use crate::logging::{FullTextTokenizer, GeoCoordinateSystem, VectorIndexOptions, VectorMetric};
use crate::sql::ast::{Column, ColumnType, Filter, Order, Projection};
use crate::storage::RowId;
use crate::storage::codec::encode_string;
use crate::value::{Point, Row, Value};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Index {
    column: String,
    map: BTreeMap<String, BTreeSet<RowId>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FullTextIndex {
    column: String,
    map: BTreeMap<String, BTreeSet<RowId>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AccessPath {
    TableScan,
    PrimaryKey,
    SecondaryIndex { index_name: String },
    FullTextIndex { index_name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TableRuntimeOptions {
    pub(crate) fulltext_tokenizer: FullTextTokenizer,
    pub(crate) vector_index: VectorIndexOptions,
    pub(crate) geo_coordinate_system: GeoCoordinateSystem,
    pub(crate) worker_threads: usize,
}

impl Default for TableRuntimeOptions {
    /// - 返回表运行时配置的默认值集合。
    /// - Returns the default set of table runtime options.
    /// - 默认配置使用简单分词、默认向量索引参数、WGS84 坐标系和单线程执行。
    /// - The defaults use simple tokenization, default vector index settings, WGS84 coordinates, and single-threaded execution.
    /// - 返回稳定的 `TableRuntimeOptions`，不读取外部状态也不产生副作用。
    /// - Returns a stable `TableRuntimeOptions` value without reading external state or causing side effects.
    fn default() -> Self {
        Self {
            fulltext_tokenizer: FullTextTokenizer::Simple,
            vector_index: VectorIndexOptions::default(),
            geo_coordinate_system: GeoCoordinateSystem::Wgs84,
            worker_threads: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Table {
    pub(crate) name: String,
    pub(crate) columns: Vec<Column>,
    pub(crate) rows: BTreeMap<RowId, Row>,
    pub(crate) next_row_id: RowId,
    pub(crate) primary_key: Option<String>,
    pub(crate) primary: BTreeMap<String, RowId>,
    indexes: BTreeMap<String, Index>,
    fulltext_indexes: BTreeMap<String, FullTextIndex>,
}

impl Table {
    /// - 创建表定义并初始化主键与索引元数据容器。
    /// - Creates a table definition and initializes primary-key and index metadata containers.
    /// - 列名必须唯一，且当前实现最多只允许一个主键列。
    /// - Column names must be unique, and the current implementation allows at most one primary-key column.
    /// - 返回空行集的 `Table`；重复列或多个主键会返回执行错误。
    /// - Returns a `Table` with no rows; duplicate columns or multiple primary keys yield execution errors.
    pub(crate) fn new(name: String, columns: Vec<Column>) -> Result<Self> {
        let mut seen = BTreeSet::new();
        let mut primary_key = None;

        for column in &columns {
            if !seen.insert(column.name.clone()) {
                return Err(Error::Execution(format!(
                    "duplicate column {}",
                    column.name
                )));
            }
            if column.primary_key {
                if primary_key.is_some() {
                    return Err(Error::Execution("only one primary key is supported".into()));
                }
                primary_key = Some(column.name.clone());
            }
        }

        Ok(Self {
            name,
            columns,
            rows: BTreeMap::new(),
            next_row_id: 1,
            primary_key,
            primary: BTreeMap::new(),
            indexes: BTreeMap::new(),
            fulltext_indexes: BTreeMap::new(),
        })
    }

    /// - 在指定列上注册普通索引并立即重建索引内容。
    /// - Registers a secondary index on the given column and immediately rebuilds its contents.
    /// - 索引名在普通索引和全文索引命名空间内都必须唯一，目标列必须存在。
    /// - The index name must be unique across both secondary and full-text indexes, and the target column must exist.
    /// - 成功时更新索引状态；列不存在、名称冲突或重建失败时返回执行错误。
    /// - Updates index state on success; yields execution errors for missing columns, name conflicts, or rebuild failures.
    pub(crate) fn create_index(&mut self, name: String, column: String) -> Result<()> {
        self.column(&column)?;
        if self.indexes.contains_key(&name) || self.fulltext_indexes.contains_key(&name) {
            return Err(Error::Execution(format!("index {name} already exists")));
        }
        self.indexes.insert(
            name,
            Index {
                column,
                map: BTreeMap::new(),
            },
        );
        self.rebuild_indexes()
    }

    #[cfg(test)]
    /// - 为测试夹具在文本列上创建全文索引。
    /// - Creates a full-text index on a text column for test fixtures.
    /// - 该辅助函数仅在测试中可用，并使用默认运行时选项。
    /// - This helper is available only in tests and uses default runtime options.
    /// - 返回索引创建结果，便于测试场景直接断言成功或失败。
    /// - Returns the index creation result so tests can assert success or failure directly.
    pub(crate) fn create_fulltext_index(&mut self, name: String, column: String) -> Result<()> {
        self.create_fulltext_index_with_options(name, column, TableRuntimeOptions::default())
    }

    /// - 在文本列上注册全文索引并使用给定运行时选项重建词项映射。
    /// - Registers a full-text index on a text column and rebuilds token mappings with the provided runtime options.
    /// - 目标列必须存在且类型为 `TEXT`，索引名也不能与已有普通或全文索引冲突。
    /// - The target column must exist and be `TEXT`, and the index name must not conflict with existing secondary or full-text indexes.
    /// - 成功时更新全文索引状态；列类型错误、名称冲突或重建失败时返回执行错误。
    /// - Updates full-text index state on success; yields execution errors for wrong column type, name conflicts, or rebuild failures.
    pub(crate) fn create_fulltext_index_with_options(
        &mut self,
        name: String,
        column: String,
        options: TableRuntimeOptions,
    ) -> Result<()> {
        let column_ref = self.column(&column)?;
        if column_ref.ty != ColumnType::Text {
            return Err(Error::Execution(
                "full-text indexes can only be created on text columns".into(),
            ));
        }
        if self.indexes.contains_key(&name) || self.fulltext_indexes.contains_key(&name) {
            return Err(Error::Execution(format!("index {name} already exists")));
        }
        self.fulltext_indexes.insert(
            name,
            FullTextIndex {
                column,
                map: BTreeMap::new(),
            },
        );
        self.rebuild_indexes_with_options(options)
    }

    #[cfg(test)]
    /// - 为测试夹具向表中插入一行数据。
    /// - Inserts one row into the table for test fixtures.
    /// - 该辅助函数仅在测试中可用，并使用默认运行时选项校验列和值。
    /// - This helper is available only in tests and validates columns and values with default runtime options.
    /// - 返回插入结果，便于测试覆盖成功与失败分支。
    /// - Returns the insert result so tests can cover success and failure paths.
    pub(crate) fn insert(
        &mut self,
        columns: Option<Vec<String>>,
        values: Vec<Value>,
    ) -> Result<()> {
        self.insert_with_options(columns, values, TableRuntimeOptions::default())
    }

    /// - 按列映射和运行时选项校验后插入一行数据。
    /// - Inserts a row after validating the column mapping and runtime options.
    /// - 指定列集时其数量必须与值数量一致，值类型、主键唯一性和向量维度都必须满足约束。
    /// - When explicit columns are supplied, their count must match the values, and type checks, primary-key uniqueness, and vector dimensions must all pass.
    /// - 成功时写入行、推进 `next_row_id` 并重建索引；任一校验失败时返回执行错误。
    /// - On success it stores the row, advances `next_row_id`, and rebuilds indexes; any validation failure yields an execution error.
    pub(crate) fn insert_with_options(
        &mut self,
        columns: Option<Vec<String>>,
        values: Vec<Value>,
        options: TableRuntimeOptions,
    ) -> Result<()> {
        let mut row = self
            .columns
            .iter()
            .map(|column| (column.name.clone(), Value::Null))
            .collect::<Row>();
        let target_columns = columns.unwrap_or_else(|| {
            self.columns
                .iter()
                .map(|column| column.name.clone())
                .collect()
        });

        if target_columns.len() != values.len() {
            return Err(Error::Execution(format!(
                "expected {} value(s), got {}",
                target_columns.len(),
                values.len()
            )));
        }

        for (column_name, value) in target_columns.into_iter().zip(values) {
            let column = self.column(&column_name)?;
            row.insert(
                column.name.clone(),
                Self::validate_value_with_options(column, value, options)?,
            );
        }

        self.validate_primary_insert(&row)?;
        let row_id = self.next_row_id;
        self.next_row_id += 1;
        self.rows.insert(row_id, row);
        self.rebuild_indexes_with_options(options)
    }

    #[cfg(test)]
    /// - 为测试场景执行选择查询并返回结果行。
    /// - Executes a select query for test scenarios and returns result rows.
    /// - 该辅助函数仅在测试中可用，并使用默认运行时选项处理过滤、排序和限制。
    /// - This helper is available only in tests and uses default runtime options for filtering, ordering, and limits.
    /// - 返回投影后的结果集，供测试断言查询行为。
    /// - Returns projected result rows for query behavior assertions in tests.
    pub(crate) fn select(
        &self,
        projection: Projection,
        filter: Option<Filter>,
        order: Option<Order>,
        limit: Option<usize>,
    ) -> Result<Vec<Row>> {
        self.select_with_options(
            projection,
            filter,
            order,
            limit,
            TableRuntimeOptions::default(),
        )
    }

    /// - 根据投影、过滤、排序和限制读取匹配行。
    /// - Reads matching rows according to projection, filter, ordering, and limit settings.
    /// - 投影列必须存在；向量排序要求目标列为向量且查询向量维度满足运行时配置。
    /// - Projected columns must exist; vector ordering requires a vector column and a query vector that matches runtime dimension settings.
    /// - 返回投影后的结果行；过滤归一化、排序计算或类型检查失败时返回执行错误。
    /// - Returns projected result rows; yields execution errors when filter normalization, scoring, or type checks fail.
    pub(crate) fn select_with_options(
        &self,
        projection: Projection,
        filter: Option<Filter>,
        order: Option<Order>,
        limit: Option<usize>,
        options: TableRuntimeOptions,
    ) -> Result<Vec<Row>> {
        self.validate_projection(&projection)?;
        let filter = self.normalize_filter(filter, options)?;
        let mut row_ids = self.matching_row_ids(filter.as_ref(), options)?.0;

        if let Some(Order::VectorDistance {
            column,
            target,
            descending,
        }) = order
        {
            let column_ref = self.column(&column)?;
            if column_ref.ty != ColumnType::Vector {
                return Err(Error::Execution(
                    "vector distance ordering requires a vector column".into(),
                ));
            }

            if let Some(expected) = options.vector_index.dimensions {
                if target.len() != expected {
                    return Err(Error::Execution(format!(
                        "query vector dimensions must be {expected}"
                    )));
                }
            }
            let mut scored = vector_scores(&self.rows, &row_ids, &column, &target, options)?;
            scored.sort_by(|left, right| {
                let distance = left
                    .0
                    .total_cmp(&right.0)
                    .then_with(|| left.1.cmp(&right.1));
                if descending {
                    distance.reverse()
                } else {
                    distance
                }
            });
            row_ids = scored.into_iter().map(|(_, row_id)| row_id).collect();
        }

        if let Some(limit) = limit {
            row_ids.truncate(limit);
        }

        Ok(row_ids
            .into_iter()
            .filter_map(|row_id| {
                self.rows
                    .get(&row_id)
                    .map(|row| project_row(row, &projection))
            })
            .collect())
    }

    #[cfg(test)]
    /// - 为测试场景更新匹配行并返回更新数量。
    /// - Updates matching rows for test scenarios and returns the number of affected rows.
    /// - 该辅助函数仅在测试中可用，并使用默认运行时选项执行赋值校验与过滤。
    /// - This helper is available only in tests and uses default runtime options for assignment validation and filtering.
    /// - 返回更新行数，便于测试断言修改结果。
    /// - Returns the updated row count for test assertions.
    pub(crate) fn update(
        &mut self,
        assignments: Vec<(String, Value)>,
        filter: Option<Filter>,
    ) -> Result<usize> {
        self.update_with_options(assignments, filter, TableRuntimeOptions::default())
    }

    /// - 按条件批量更新行并在失败时回滚到原始数据集。
    /// - Batch-updates rows matching a filter and rolls back to the original data set on failure.
    /// - 赋值列必须存在且值类型合法；重建索引时仍需满足主键和索引一致性约束。
    /// - Assigned columns must exist and receive valid values; index rebuilds must still satisfy primary-key and index consistency constraints.
    /// - 返回成功更新的行数；若重建失败则恢复原状态并返回执行错误。
    /// - Returns the number of updated rows; if rebuilding fails, restores the original state and returns an execution error.
    pub(crate) fn update_with_options(
        &mut self,
        assignments: Vec<(String, Value)>,
        filter: Option<Filter>,
        options: TableRuntimeOptions,
    ) -> Result<usize> {
        let filter = self.normalize_filter(filter, options)?;
        let row_ids = self.matching_row_ids(filter.as_ref(), options)?.0;

        let mut normalized = Vec::new();
        for (column_name, value) in assignments {
            let column = self.column(&column_name)?;
            normalized.push((
                column.name.clone(),
                Self::validate_value_with_options(column, value, options)?,
            ));
        }

        let original_rows = self.rows.clone();
        let mut updated_rows = self.rows.clone();
        for row_id in &row_ids {
            let row = updated_rows.get_mut(row_id).expect("matched row id exists");
            for (column, value) in &normalized {
                row.insert(column.clone(), value.clone());
            }
        }

        self.rows = updated_rows;
        if let Err(error) = self.rebuild_indexes_with_options(options) {
            self.rows = original_rows;
            let _ = self.rebuild_indexes_with_options(options);
            return Err(error);
        }
        Ok(row_ids.len())
    }

    #[cfg(test)]
    /// - 为测试场景删除匹配行并返回删除数量。
    /// - Deletes rows matching a filter for test scenarios and returns the affected count.
    /// - 该辅助函数仅在测试中可用，并使用默认运行时选项处理过滤逻辑。
    /// - This helper is available only in tests and uses default runtime options for filter handling.
    /// - 返回删除行数，供测试断言删除行为。
    /// - Returns the deleted row count for deletion assertions in tests.
    pub(crate) fn delete(&mut self, filter: Option<Filter>) -> Result<usize> {
        self.delete_with_options(filter, TableRuntimeOptions::default())
    }

    /// - 删除满足过滤条件的行并重建索引。
    /// - Deletes rows that satisfy the filter and rebuilds indexes afterward.
    /// - 过滤条件会先按运行时选项归一化；删除过程依赖内部行标识仍然有效。
    /// - The filter is normalized first with runtime options, and deletion assumes internal row identifiers remain valid.
    /// - 返回删除行数；过滤归一化或索引重建失败时返回执行错误。
    /// - Returns the number of removed rows; yields execution errors when filter normalization or index rebuilding fails.
    pub(crate) fn delete_with_options(
        &mut self,
        filter: Option<Filter>,
        options: TableRuntimeOptions,
    ) -> Result<usize> {
        let filter = self.normalize_filter(filter, options)?;
        let row_ids = self.matching_row_ids(filter.as_ref(), options)?.0;
        for row_id in &row_ids {
            self.rows.remove(row_id);
        }
        self.rebuild_indexes_with_options(options)?;
        Ok(row_ids.len())
    }

    /// - 注册从持久化文件恢复出的普通索引元数据。
    /// - Registers secondary-index metadata restored from persisted storage.
    /// - 该函数只写入索引定义，不校验列存在性，也不会立刻填充索引映射。
    /// - This function only records the index definition, does not validate column existence, and does not populate the index map immediately.
    /// - 产生的副作用是向索引集合插入空映射项。
    /// - Its side effect is inserting an empty index entry into the index collection.
    pub(crate) fn add_persisted_index(&mut self, name: String, column: String) {
        self.indexes.insert(
            name,
            Index {
                column,
                map: BTreeMap::new(),
            },
        );
    }

    /// - 注册从持久化文件恢复出的全文索引元数据。
    /// - Registers full-text index metadata restored from persisted storage.
    /// - 该函数仅记录名称和列，不做类型检查，也不会立刻分词填充映射。
    /// - This function records only the name and column, performs no type checks, and does not tokenize rows immediately.
    /// - 产生的副作用是向全文索引集合插入空映射项。
    /// - Its side effect is inserting an empty entry into the full-text index collection.
    pub(crate) fn add_persisted_fulltext_index(&mut self, name: String, column: String) {
        self.fulltext_indexes.insert(
            name,
            FullTextIndex {
                column,
                map: BTreeMap::new(),
            },
        );
    }

    /// - 导出普通索引的名称与列名，供持久化编码使用。
    /// - Exports secondary-index names and column names for persistence encoding.
    /// - 返回值只反映索引元数据，不包含索引映射中的行内容。
    /// - The return value reflects only index metadata and omits row contents from the index maps.
    /// - 返回借用切片对集合，不修改表状态且不会失败。
    /// - Returns borrowed name/column pairs without mutating table state and cannot fail.
    pub(crate) fn indexes_for_encoding(&self) -> Vec<(&String, &String)> {
        self.indexes
            .iter()
            .map(|(name, index)| (name, &index.column))
            .collect()
    }

    /// - 导出全文索引的名称与列名，供持久化编码使用。
    /// - Exports full-text index names and column names for persistence encoding.
    /// - 返回值只包含索引定义信息，不包含词项到行号的倒排映射。
    /// - The return value contains only index definitions and omits token-to-row inverted mappings.
    /// - 返回借用对集合且不修改表状态。
    /// - Returns borrowed pairs and does not mutate table state.
    pub(crate) fn fulltext_indexes_for_encoding(&self) -> Vec<(&String, &String)> {
        self.fulltext_indexes
            .iter()
            .map(|(name, index)| (name, &index.column))
            .collect()
    }

    /// - 使用默认运行时选项重建主键、普通索引和全文索引。
    /// - Rebuilds primary, secondary, and full-text indexes using default runtime options.
    /// - 该入口主要供内部和测试复用，要求现有行数据满足主键与索引约束。
    /// - This entry point is reused by internals and tests and requires existing rows to satisfy primary-key and index constraints.
    /// - 成功时刷新所有索引映射；数据损坏时返回执行错误。
    /// - Refreshes all index maps on success; yields execution errors when stored rows are inconsistent.
    pub(crate) fn rebuild_indexes(&mut self) -> Result<()> {
        self.rebuild_indexes_with_options(TableRuntimeOptions::default())
    }

    /// - 按运行时选项为现有行重新计算主键、普通索引和全文索引内容。
    /// - Recomputes primary, secondary, and full-text index contents for existing rows using runtime options.
    /// - 所有行必须包含有效主键值，且被索引的值必须可编码；全文索引使用指定分词器。
    /// - Every row must contain a valid primary key when required, indexed values must be encodable, and full-text indexes use the configured tokenizer.
    /// - 成功时清空后重建全部索引映射；发现空主键、重复主键或坏索引值时返回执行错误。
    /// - Clears and rebuilds all index maps on success; yields execution errors for null primary keys, duplicates, or invalid indexed values.
    pub(crate) fn rebuild_indexes_with_options(
        &mut self,
        options: TableRuntimeOptions,
    ) -> Result<()> {
        self.primary.clear();
        for index in self.indexes.values_mut() {
            index.map.clear();
        }
        for index in self.fulltext_indexes.values_mut() {
            index.map.clear();
        }

        let primary_key = self.primary_key.clone();
        let index_columns = self
            .indexes
            .iter()
            .map(|(name, index)| (name.clone(), index.column.clone()))
            .collect::<Vec<_>>();
        let fulltext_columns = self
            .fulltext_indexes
            .iter()
            .map(|(name, index)| (name.clone(), index.column.clone()))
            .collect::<Vec<_>>();

        for (row_id, row) in &self.rows {
            if let Some(primary_key) = &primary_key {
                let value = row
                    .get(primary_key)
                    .ok_or_else(|| Error::Execution("primary key value missing".into()))?;
                if matches!(value, Value::Null) {
                    return Err(Error::Execution("primary key cannot be null".into()));
                }
                if self.primary.insert(index_key(value)?, *row_id).is_some() {
                    return Err(Error::Execution("duplicate primary key".into()));
                }
            }

            for (index_name, column) in &index_columns {
                if let Some(value) = row.get(column) {
                    let key = index_key(value)?;
                    let index = self.indexes.get_mut(index_name).expect("index exists");
                    index.map.entry(key).or_default().insert(*row_id);
                }
            }

            for (index_name, column) in &fulltext_columns {
                if let Some(Value::Text(text)) = row.get(column) {
                    let fulltext = self
                        .fulltext_indexes
                        .get_mut(index_name)
                        .expect("full-text index exists");
                    for token in tokenize(text, options.fulltext_tokenizer) {
                        fulltext.map.entry(token).or_default().insert(*row_id);
                    }
                }
            }
        }

        Ok(())
    }

    /// - 返回当前表主键列名的只读视图。
    /// - Returns a read-only view of the current primary-key column name.
    /// - 仅当表定义中声明了主键时才返回值。
    /// - Produces a value only when the table definition declares a primary key.
    /// - 返回借用的列名切片或 `None`，不产生副作用。
    /// - Returns a borrowed column-name slice or `None` with no side effects.
    pub(crate) fn primary_key_column(&self) -> Option<&str> {
        self.primary_key.as_deref()
    }

    /// - 校验待插入行是否满足主键非空且唯一的约束。
    /// - Validates that a pending insert row satisfies non-null and unique primary-key constraints.
    /// - 仅在表定义存在主键时执行检查，并依赖当前主键索引反映已提交行状态。
    /// - Checks run only when the table defines a primary key and rely on the current primary index reflecting committed rows.
    /// - 约束满足时返回 `Ok(())`；缺失、空值或重复主键会返回执行错误。
    /// - Returns `Ok(())` when constraints hold; missing, null, or duplicate keys yield execution errors.
    fn validate_primary_insert(&self, row: &Row) -> Result<()> {
        if let Some(primary_key) = &self.primary_key {
            let value = row
                .get(primary_key)
                .ok_or_else(|| Error::Execution("primary key value missing".into()))?;
            if matches!(value, Value::Null) {
                return Err(Error::Execution("primary key cannot be null".into()));
            }
            if self.primary.contains_key(&index_key(value)?) {
                return Err(Error::Execution("duplicate primary key".into()));
            }
        }
        Ok(())
    }

    /// - 校验查询投影引用的列是否都存在于表定义中。
    /// - Validates that every column referenced by a projection exists in the table definition.
    /// - `Projection::All` 直接通过，显式列清单则逐项做名称归一化和查找。
    /// - `Projection::All` passes immediately, while explicit column lists are normalized and looked up one by one.
    /// - 返回 `Ok(())` 或未知列错误，不修改表状态。
    /// - Returns `Ok(())` or an unknown-column error without mutating table state.
    fn validate_projection(&self, projection: &Projection) -> Result<()> {
        if let Projection::Columns(columns) = projection {
            for column in columns {
                self.column(column)?;
            }
        }
        Ok(())
    }

    /// - 将过滤条件中的列名和值规范化为表内部使用的表示。
    /// - Normalizes filter column names and values into the representation used internally by the table.
    /// - 等值、全文和地理过滤都会验证列存在性及类型约束，并在需要时应用运行时选项。
    /// - Equality, full-text, and geo filters validate column existence and type constraints and apply runtime options when needed.
    /// - 返回归一化后的过滤条件；列不存在、类型不匹配或值非法时返回执行错误。
    /// - Returns the normalized filter; yields execution errors for missing columns, type mismatches, or invalid values.
    fn normalize_filter(
        &self,
        filter: Option<Filter>,
        options: TableRuntimeOptions,
    ) -> Result<Option<Filter>> {
        match filter {
            Some(Filter::Equals(column_name, value)) => {
                let column = self.column(&column_name)?;
                Ok(Some(Filter::Equals(
                    column.name.clone(),
                    Self::validate_value_with_options(column, value, options)?,
                )))
            }
            Some(Filter::FullText { column, query }) => {
                let column_ref = self.column(&column)?;
                if column_ref.ty != ColumnType::Text {
                    return Err(Error::Execution("MATCH requires a text column".into()));
                }
                Ok(Some(Filter::FullText {
                    column: column_ref.name.clone(),
                    query,
                }))
            }
            Some(Filter::GeoWithin {
                column,
                point,
                meters,
                inclusive,
            }) => {
                let column_ref = self.column(&column)?;
                if column_ref.ty != ColumnType::Point {
                    return Err(Error::Execution(
                        "GEO_DISTANCE requires a point column".into(),
                    ));
                }
                Ok(Some(Filter::GeoWithin {
                    column: column_ref.name.clone(),
                    point,
                    meters,
                    inclusive,
                }))
            }
            None => Ok(None),
        }
    }

    /// - 预测给定过滤条件会采用的访问路径。
    /// - Predicts which access path a given filter will use.
    /// - 过滤条件会先按运行时选项归一化，因此列类型和名称必须先通过校验。
    /// - The filter is normalized first with runtime options, so column names and types must validate up front.
    /// - 返回表扫描、主键或索引访问路径；归一化失败时返回执行错误。
    /// - Returns a table-scan, primary-key, or index access path; yields an execution error when normalization fails.
    pub(crate) fn explain_filter(
        &self,
        filter: Option<Filter>,
        options: TableRuntimeOptions,
    ) -> Result<AccessPath> {
        let filter = self.normalize_filter(filter, options)?;
        Ok(self.matching_row_ids(filter.as_ref(), options)?.1)
    }

    /// - 获取满足过滤条件的行标识列表，供锁管理或上层协调逻辑使用。
    /// - Collects row identifiers matching a filter for locking or higher-level coordination.
    /// - 结果顺序遵循内部访问路径产生的顺序，并依赖过滤条件已经可被规范化。
    /// - Result ordering follows the chosen internal access path and depends on the filter being normalizable.
    /// - 返回匹配的 `RowId` 列表；过滤无效时返回执行错误。
    /// - Returns the matching `RowId` list; yields an execution error for invalid filters.
    pub(crate) fn matching_row_ids_for_lock(
        &self,
        filter: Option<Filter>,
        options: TableRuntimeOptions,
    ) -> Result<Vec<RowId>> {
        let filter = self.normalize_filter(filter, options)?;
        Ok(self.matching_row_ids(filter.as_ref(), options)?.0)
    }

    /// - 根据过滤条件计算匹配行及其实际采用的访问路径。
    /// - Computes matching rows for a filter together with the actual access path used.
    /// - 会优先使用主键索引、普通索引或全文索引；无法利用索引时回退到表扫描。
    /// - Prefers primary, secondary, or full-text indexes when available and falls back to table scans otherwise.
    /// - 返回 `RowId` 集合与访问路径；索引键编码或距离计算失败时返回执行错误。
    /// - Returns the `RowId` set plus the access path; yields execution errors when index-key encoding or distance checks fail.
    fn matching_row_ids(
        &self,
        filter: Option<&Filter>,
        options: TableRuntimeOptions,
    ) -> Result<(Vec<RowId>, AccessPath)> {
        match filter {
            None => Ok((self.rows.keys().copied().collect(), AccessPath::TableScan)),
            Some(Filter::Equals(column, value)) => {
                let key = index_key(value)?;
                if self.primary_key.as_ref() == Some(column) {
                    return Ok((
                        self.primary.get(&key).copied().into_iter().collect(),
                        AccessPath::PrimaryKey,
                    ));
                }

                if let Some((index_name, index)) = self
                    .indexes
                    .iter()
                    .find(|(_, index)| &index.column == column)
                {
                    return Ok((
                        index
                            .map
                            .get(&key)
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .collect(),
                        AccessPath::SecondaryIndex {
                            index_name: index_name.clone(),
                        },
                    ));
                }

                Ok((
                    self.rows
                        .iter()
                        .filter_map(|(row_id, row)| {
                            (row.get(column) == Some(value)).then_some(*row_id)
                        })
                        .collect(),
                    AccessPath::TableScan,
                ))
            }
            Some(Filter::FullText { column, query }) => self.match_fulltext(column, query, options),
            Some(Filter::GeoWithin {
                column,
                point,
                meters,
                inclusive,
            }) => Ok((
                self.rows
                    .iter()
                    .filter_map(|(row_id, row)| match row.get(column) {
                        Some(Value::Point(candidate)) => {
                            let distance =
                                geo_distance(*candidate, *point, options.geo_coordinate_system);
                            ((*inclusive && distance <= *meters)
                                || (!*inclusive && distance < *meters))
                                .then_some(*row_id)
                        }
                        _ => None,
                    })
                    .collect(),
                AccessPath::TableScan,
            )),
        }
    }

    /// - 使用全文索引或回退扫描计算匹配全文查询的行。
    /// - Resolves rows matching a full-text query using an index or a fallback scan.
    /// - 查询会先分词；空查询直接返回空结果，而命中索引时需要所有词项都存在。
    /// - The query is tokenized first; an empty query returns no rows, and indexed matches require every token to be present.
    /// - 返回匹配行和访问路径；不会修改表状态。
    /// - Returns matching rows and the chosen access path without mutating table state.
    fn match_fulltext(
        &self,
        column: &str,
        query: &str,
        options: TableRuntimeOptions,
    ) -> Result<(Vec<RowId>, AccessPath)> {
        let tokens = tokenize(query, options.fulltext_tokenizer);
        if tokens.is_empty() {
            return Ok((Vec::new(), AccessPath::TableScan));
        }

        if let Some((index_name, index)) = self
            .fulltext_indexes
            .iter()
            .find(|(_, index)| index.column == column)
        {
            let mut sets = tokens
                .iter()
                .map(|token| index.map.get(token).cloned().unwrap_or_default());
            let first = sets.next().unwrap_or_default();
            let result = sets.fold(first, |acc, set| {
                acc.intersection(&set).copied().collect::<BTreeSet<_>>()
            });
            return Ok((
                result.into_iter().collect(),
                AccessPath::FullTextIndex {
                    index_name: index_name.clone(),
                },
            ));
        }

        Ok((
            self.rows
                .iter()
                .filter_map(|(row_id, row)| match row.get(column) {
                    Some(Value::Text(text)) => {
                        let row_tokens = tokenize(text, options.fulltext_tokenizer)
                            .into_iter()
                            .collect::<BTreeSet<_>>();
                        tokens
                            .iter()
                            .all(|token| row_tokens.contains(token))
                            .then_some(*row_id)
                    }
                    _ => None,
                })
                .collect(),
            AccessPath::TableScan,
        ))
    }

    /// - 按规范化后的列名查找表定义中的列元数据。
    /// - Looks up column metadata in the table definition by normalized column name.
    /// - 输入名称会先做标识符规范化，因此调用方可传入大小写或格式不同的等价名称。
    /// - The input name is normalized first, so callers may pass equivalent names with different case or formatting.
    /// - 返回列引用；列不存在时返回执行错误。
    /// - Returns a column reference; yields an execution error when the column is unknown.
    fn column(&self, name: &str) -> Result<&Column> {
        let normalized = normalize_identifier(name);
        self.columns
            .iter()
            .find(|column| column.name == normalized)
            .ok_or_else(|| Error::Execution(format!("unknown column {normalized}")))
    }

    /// - 按列类型和运行时选项校验并规范化单个值。
    /// - Validates and normalizes a single value against a column type and runtime options.
    /// - 浮点、向量和点值要求有限数；向量列还可能要求固定维度。
    /// - Float, vector, and point values must be finite, and vector columns may enforce a fixed dimension count.
    /// - 返回可写入行中的规范化值；类型不匹配或维度错误时返回执行错误。
    /// - Returns the normalized value ready for storage; yields execution errors for type mismatches or dimension errors.
    fn validate_value_with_options(
        column: &Column,
        value: Value,
        options: TableRuntimeOptions,
    ) -> Result<Value> {
        match (&column.ty, value) {
            (_, Value::Null) => Ok(Value::Null),
            (ColumnType::Integer, Value::Integer(value)) => Ok(Value::Integer(value)),
            (ColumnType::Float, Value::Float(value)) if value.is_finite() => {
                Ok(Value::Float(value))
            }
            (ColumnType::Float, Value::Integer(value)) => Ok(Value::Float(value as f64)),
            (ColumnType::Boolean, Value::Boolean(value)) => Ok(Value::Boolean(value)),
            (ColumnType::Text, Value::Text(value)) => Ok(Value::Text(value)),
            (ColumnType::Vector, Value::Vector(value))
                if value.iter().all(|item| item.is_finite()) =>
            {
                match options.vector_index.dimensions {
                    Some(expected) if value.len() != expected => {
                        vector_dimension_error(&column.name, expected)
                    }
                    _ => Ok(Value::Vector(value)),
                }
            }
            (ColumnType::Point, Value::Point(value))
                if value.lon.is_finite() && value.lat.is_finite() =>
            {
                Ok(Value::Point(value))
            }
            (expected, actual) => Err(Error::Execution(format!(
                "value {actual:?} does not match column {} type {}",
                column.name,
                expected.as_str()
            ))),
        }
    }
}

/// - 按查询投影从原始行构造输出行。
/// - Builds an output row from a source row according to the requested projection.
/// - `Projection::All` 会克隆整行，显式列投影只保留存在的键值对。
/// - `Projection::All` clones the full row, while explicit projections keep only existing key/value pairs.
/// - 返回新的 `Row`，不修改输入行。
/// - Returns a new `Row` without mutating the input row.
fn project_row(row: &Row, projection: &Projection) -> Row {
    match projection {
        Projection::All => row.clone(),
        Projection::Columns(columns) => columns
            .iter()
            .filter_map(|column| {
                row.get(column)
                    .cloned()
                    .map(|value| (column.clone(), value))
            })
            .collect(),
    }
}

/// - 将可索引值转换为索引映射使用的稳定键字符串。
/// - Converts an indexable value into the stable key string used by index maps.
/// - 仅支持可稳定编码的有限值；非有限浮点、向量或点不能进入索引。
/// - Only finitely encodable values are supported; non-finite floats, vectors, or points cannot be indexed.
/// - 返回索引键；遇到不可索引值时返回执行错误。
/// - Returns the index key; yields an execution error for non-indexable values.
pub(crate) fn index_key(value: &Value) -> Result<String> {
    let key = match value {
        Value::Null => "N".to_string(),
        Value::Integer(value) => format!("I:{value}"),
        Value::Float(value) if value.is_finite() => format!("F:{:016x}", value.to_bits()),
        Value::Boolean(value) => format!("B:{}", if *value { 1 } else { 0 }),
        Value::Text(value) => format!("T:{}", encode_string(value)),
        Value::Vector(values) if values.iter().all(|value| value.is_finite()) => {
            let values = values
                .iter()
                .map(|value| format!("{:08x}", value.to_bits()))
                .collect::<Vec<_>>()
                .join(",");
            format!("V:{values}")
        }
        Value::Point(point) if point.lon.is_finite() && point.lat.is_finite() => {
            format!(
                "P:{:016x},{:016x}",
                point.lon.to_bits(),
                point.lat.to_bits()
            )
        }
        _ => {
            return Err(Error::Execution(
                "non-finite value cannot be indexed".into(),
            ));
        }
    };
    Ok(key)
}

/// - 为候选行批量计算向量距离分数，必要时并行分片执行。
/// - Computes vector-distance scores for candidate rows, using parallel chunks when warranted.
/// - 仅当 `worker_threads` 大于 1 且候选行足够多时才并行；距离计算遵循配置的向量度量。
/// - Parallelism is used only when `worker_threads` exceeds 1 and enough candidate rows exist, and scoring follows the configured vector metric.
/// - 返回 `(distance, row_id)` 列表；工作线程失败或距离计算错误时返回执行错误。
/// - Returns a list of `(distance, row_id)` pairs; yields execution errors for worker failures or distance calculation errors.
fn vector_scores(
    rows: &BTreeMap<RowId, Row>,
    row_ids: &[RowId],
    column: &str,
    target: &[f32],
    options: TableRuntimeOptions,
) -> Result<Vec<(f64, RowId)>> {
    if options.worker_threads <= 1 || row_ids.len() < 2 {
        return vector_scores_chunk(rows, row_ids, column, target, options.vector_index.metric);
    }

    let workers = options.worker_threads.min(row_ids.len());
    let chunk_size = (row_ids.len() + workers - 1) / workers;
    std::thread::scope(|scope| {
        let handles = row_ids
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    vector_scores_chunk(rows, chunk, column, target, options.vector_index.metric)
                })
            })
            .collect::<Vec<_>>();
        let mut scored = Vec::new();
        for handle in handles {
            scored.extend(handle.join().expect("vector worker panicked")?);
        }
        Ok(scored)
    })
}

/// - 对单个候选分片顺序计算向量距离分数。
/// - Sequentially computes vector-distance scores for one candidate chunk.
/// - 只会为指定列中确实包含向量值的行生成分数。
/// - Generates scores only for rows whose requested column actually contains a vector value.
/// - 返回当前分片的分数列表；距离计算失败时返回执行错误。
/// - Returns the score list for the chunk; yields an execution error when distance computation fails.
fn vector_scores_chunk(
    rows: &BTreeMap<RowId, Row>,
    row_ids: &[RowId],
    column: &str,
    target: &[f32],
    metric: VectorMetric,
) -> Result<Vec<(f64, RowId)>> {
    let mut scored = Vec::new();
    for row_id in row_ids {
        let vector = rows.get(row_id).and_then(|row| row.get(column));
        if let Some(Value::Vector(vector)) = vector {
            scored.push((vector_distance(vector, target, metric)?, *row_id));
        }
    }
    Ok(scored)
}

/// - 构造向量维度不匹配时使用的统一错误结果。
/// - Builds the canonical error result for vector dimension mismatches.
/// - 仅用于向量列校验路径，`expected` 表示必须满足的维度数量。
/// - Used only in vector-column validation paths, where `expected` is the required dimension count.
/// - 始终返回执行错误，不会产生有效值。
/// - Always returns an execution error and never produces a valid value.
fn vector_dimension_error(column: &str, expected: usize) -> Result<Value> {
    let message = format!("vector column {column} requires {expected} dimension(s)");
    Err(Error::Execution(message))
}

/// - 按指定分词策略将文本转换为全文搜索词项序列。
/// - Converts text into a token sequence for full-text search using the selected tokenizer.
/// - 分词结果统一转为小写；`Exact` 会先裁剪首尾空白并可能返回空序列。
/// - Tokens are lowercased uniformly; `Exact` trims surrounding whitespace first and may return an empty sequence.
/// - 返回词项列表，不保留额外状态也不会失败。
/// - Returns the token list, keeps no extra state, and cannot fail.
fn tokenize(input: &str, tokenizer: FullTextTokenizer) -> Vec<String> {
    match tokenizer {
        FullTextTokenizer::Simple => input
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(|token| token.to_lowercase())
            .collect(),
        FullTextTokenizer::Whitespace => input
            .split_whitespace()
            .filter(|token| !token.is_empty())
            .map(|token| token.to_lowercase())
            .collect(),
        FullTextTokenizer::Exact => {
            let token = input.trim().to_lowercase();
            if token.is_empty() {
                Vec::new()
            } else {
                vec![token]
            }
        }
    }
}

/// - 根据指定度量计算两个向量之间的距离或排序分数。
/// - Computes the distance or ordering score between two vectors using the chosen metric.
/// - 两个向量必须维度一致；余弦距离还要求两侧都不是零向量。
/// - The vectors must have matching dimensions, and cosine distance also requires both sides to be non-zero.
/// - 返回 `f64` 分数；维度不匹配或余弦输入非法时返回执行错误。
/// - Returns an `f64` score; yields execution errors for dimension mismatches or invalid cosine inputs.
fn vector_distance(left: &[f32], right: &[f32], metric: VectorMetric) -> Result<f64> {
    if left.len() != right.len() {
        return Err(Error::Execution("vector dimensions do not match".into()));
    }
    match metric {
        VectorMetric::Euclidean => Ok(left
            .iter()
            .zip(right)
            .map(|(left, right)| {
                let delta = f64::from(*left) - f64::from(*right);
                delta * delta
            })
            .sum::<f64>()
            .sqrt()),
        VectorMetric::Cosine => {
            let dot = dot_product(left, right);
            let left_norm = dot_product(left, left).sqrt();
            let right_norm = dot_product(right, right).sqrt();
            if left_norm == 0.0 || right_norm == 0.0 {
                return Err(Error::Execution(
                    "cosine vector distance requires non-zero vectors".into(),
                ));
            }
            Ok(1.0 - dot / (left_norm * right_norm))
        }
        VectorMetric::DotProduct => Ok(-dot_product(left, right)),
    }
}

/// - 计算两个向量的点积作为向量距离实现的基础操作。
/// - Computes the dot product of two vectors as a primitive for vector-distance calculations.
/// - 调用方应保证切片长度兼容；函数本身按最短共同长度逐项累加。
/// - Callers should ensure compatible slice lengths; the function itself accumulates over the shared zipped length.
/// - 返回点积结果，不分配内存也不会失败。
/// - Returns the dot-product value without allocations and without failure.
fn dot_product(left: &[f32], right: &[f32]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| f64::from(*left) * f64::from(*right))
        .sum()
}

/// - 根据配置坐标系计算两个点之间的地理距离。
/// - Computes the geographic distance between two points using the configured coordinate system.
/// - `Wgs84` 使用球面近似，`Cartesian` 使用平面欧氏距离。
/// - `Wgs84` uses a spherical approximation, while `Cartesian` uses planar Euclidean distance.
/// - 返回距离标量，不修改输入点且不会失败。
/// - Returns the distance scalar without mutating the input points and cannot fail.
fn geo_distance(left: Point, right: Point, coordinate_system: GeoCoordinateSystem) -> f64 {
    match coordinate_system {
        GeoCoordinateSystem::Wgs84 => haversine_meters(left, right),
        GeoCoordinateSystem::Cartesian => {
            let delta_x = right.lon - left.lon;
            let delta_y = right.lat - left.lat;
            (delta_x * delta_x + delta_y * delta_y).sqrt()
        }
    }
}

/// - 使用 Haversine 公式估算两点在地球表面上的米级距离。
/// - Estimates the surface distance between two points in meters using the Haversine formula.
/// - 输入经纬度按度数解释并转换为弧度，采用固定地球半径常量。
/// - Input longitude and latitude are interpreted in degrees, converted to radians, and evaluated with a fixed Earth-radius constant.
/// - 返回 WGS84 近似距离值，不产生副作用。
/// - Returns the approximate WGS84 distance value with no side effects.
fn haversine_meters(left: Point, right: Point) -> f64 {
    let earth_radius_meters = 6_371_000.0_f64;
    let lat1 = left.lat.to_radians();
    let lat2 = right.lat.to_radians();
    let delta_lat = (right.lat - left.lat).to_radians();
    let delta_lon = (right.lon - left.lon).to_radians();
    let a =
        (delta_lat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (delta_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    earth_radius_meters * c
}

#[cfg(test)]
mod tests {
    use super::*;

    /// - 构造测试使用的列定义辅助函数。
    /// - Builds a column definition helper for tests.
    /// - 夹具只关心列名、类型和主键标记三个输入。
    /// - The fixture cares only about the column name, type, and primary-key flag inputs.
    /// - 返回可直接拼装进测试表定义的 `Column`。
    /// - Returns a `Column` ready to assemble into test table definitions.
    fn column(name: &str, ty: ColumnType, primary_key: bool) -> Column {
        Column {
            name: name.into(),
            ty,
            primary_key,
        }
    }

    /// - 构造覆盖多种列类型的 `users` 测试表夹具。
    /// - Builds the `users` test table fixture covering multiple column types.
    /// - 夹具固定包含主键、文本、数值、向量和点列，便于复用到多数场景。
    /// - The fixture always includes primary-key, text, numeric, vector, and point columns for broad scenario reuse.
    /// - 返回空数据表，供后续测试自行插入样例行。
    /// - Returns an empty table so each test can insert its own sample rows.
    fn users() -> Table {
        Table::new(
            "users".into(),
            vec![
                column("id", ColumnType::Integer, true),
                column("name", ColumnType::Text, false),
                column("age", ColumnType::Integer, false),
                column("score", ColumnType::Float, false),
                column("active", ColumnType::Boolean, false),
                column("embedding", ColumnType::Vector, false),
                column("place", ColumnType::Point, false),
            ],
        )
        .unwrap()
    }

    #[test]
    /// - 验证非法表定义会在建表阶段被拒绝。
    /// - Verifies invalid table definitions are rejected during table creation.
    /// - 场景覆盖重复列名和多个主键两类结构错误。
    /// - The scenario covers duplicate column names and multiple primary keys.
    fn rejects_invalid_table_definitions() {
        assert!(
            Table::new(
                "bad".into(),
                vec![
                    column("id", ColumnType::Integer, true),
                    column("id", ColumnType::Integer, false)
                ]
            )
            .is_err()
        );
        assert!(
            Table::new(
                "bad".into(),
                vec![
                    column("id", ColumnType::Integer, true),
                    column("other", ColumnType::Integer, true)
                ]
            )
            .is_err()
        );
    }

    #[test]
    /// - 验证基础增删改查与普通/全文索引协同工作。
    /// - Verifies core CRUD operations work together with secondary and full-text indexes.
    /// - 场景断言插入、查询、更新和删除后的结果数量与内容正确。
    /// - The scenario asserts correct counts and contents after insert, select, update, and delete flows.
    fn insert_select_update_delete_and_indexes() {
        let mut table = users();
        table.create_index("age_idx".into(), "age".into()).unwrap();
        table
            .create_fulltext_index("name_fts".into(), "name".into())
            .unwrap();
        table
            .insert(
                None,
                vec![
                    Value::Integer(1),
                    Value::Text("Ada Lovelace".into()),
                    Value::Integer(36),
                    Value::Integer(10),
                    Value::Boolean(true),
                    Value::Vector(vec![0.0, 0.0]),
                    Value::Point(Point { lon: 0.0, lat: 0.0 }),
                ],
            )
            .unwrap();
        table
            .insert(
                Some(vec!["id".into(), "name".into(), "age".into()]),
                vec![
                    Value::Integer(2),
                    Value::Text("Grace Hopper".into()),
                    Value::Integer(85),
                ],
            )
            .unwrap();

        let rows = table
            .select(
                Projection::Columns(vec!["name".into()]),
                Some(Filter::Equals("age".into(), Value::Integer(36))),
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            rows[0].get("name"),
            Some(&Value::Text("Ada Lovelace".into()))
        );

        assert_eq!(
            table
                .select(
                    Projection::All,
                    Some(Filter::FullText {
                        column: "name".into(),
                        query: "grace hopper".into()
                    }),
                    None,
                    None
                )
                .unwrap()
                .len(),
            1
        );

        assert_eq!(
            table
                .update(
                    vec![("age".into(), Value::Integer(37))],
                    Some(Filter::Equals("id".into(), Value::Integer(1)))
                )
                .unwrap(),
            1
        );
        assert_eq!(
            table
                .delete(Some(Filter::Equals("age".into(), Value::Integer(37))))
                .unwrap(),
            1
        );
    }

    #[test]
    /// - 验证向量排序与地理范围过滤返回预期记录。
    /// - Verifies vector ordering and geo-within filtering return the expected records.
    /// - 场景重点检查最近向量命中和给定距离阈值下的地理匹配。
    /// - The scenario focuses on nearest-vector ranking and geo matches under a distance threshold.
    fn vector_and_geo_queries_work() {
        let mut table = users();
        for (id, vector, point) in [
            (
                1,
                vec![10.0, 10.0],
                Point {
                    lon: 10.0,
                    lat: 10.0,
                },
            ),
            (2, vec![0.1, 0.1], Point { lon: 0.0, lat: 0.0 }),
        ] {
            table
                .insert(
                    Some(vec![
                        "id".into(),
                        "name".into(),
                        "embedding".into(),
                        "place".into(),
                    ]),
                    vec![
                        Value::Integer(id),
                        Value::Text(format!("u{id}")),
                        Value::Vector(vector),
                        Value::Point(point),
                    ],
                )
                .unwrap();
        }
        let rows = table
            .select(
                Projection::Columns(vec!["id".into()]),
                None,
                Some(Order::VectorDistance {
                    column: "embedding".into(),
                    target: vec![0.0, 0.0],
                    descending: false,
                }),
                Some(1),
            )
            .unwrap();
        assert_eq!(rows[0].get("id"), Some(&Value::Integer(2)));
        assert_eq!(
            table
                .select(
                    Projection::All,
                    Some(Filter::GeoWithin {
                        column: "place".into(),
                        point: Point { lon: 0.0, lat: 0.0 },
                        meters: 1.0,
                        inclusive: true
                    }),
                    None,
                    None
                )
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    /// - 验证运行时选项会改变全文、向量和地理查询行为。
    /// - Verifies runtime options change full-text, vector, and geo query behavior.
    /// - 场景覆盖精确分词、点积排序、维度校验和笛卡尔距离语义。
    /// - The scenario covers exact tokenization, dot-product ranking, dimension checks, and Cartesian distance semantics.
    fn runtime_options_change_fulltext_vector_and_geo_behavior() {
        let mut table = users();
        let exact_options = TableRuntimeOptions {
            fulltext_tokenizer: FullTextTokenizer::Exact,
            ..TableRuntimeOptions::default()
        };
        table
            .create_fulltext_index_with_options("name_exact".into(), "name".into(), exact_options)
            .unwrap();
        table
            .insert_with_options(
                Some(vec![
                    "id".into(),
                    "name".into(),
                    "embedding".into(),
                    "place".into(),
                ]),
                vec![
                    Value::Integer(1),
                    Value::Text("Ada Lovelace".into()),
                    Value::Vector(vec![10.0, 0.0]),
                    Value::Point(Point { lon: 0.0, lat: 2.0 }),
                ],
                exact_options,
            )
            .unwrap();
        table
            .insert_with_options(
                Some(vec![
                    "id".into(),
                    "name".into(),
                    "embedding".into(),
                    "place".into(),
                ]),
                vec![
                    Value::Integer(2),
                    Value::Text("Grace Hopper".into()),
                    Value::Vector(vec![0.0, 1.0]),
                    Value::Point(Point { lon: 0.0, lat: 0.0 }),
                ],
                exact_options,
            )
            .unwrap();

        assert!(
            table
                .select_with_options(
                    Projection::All,
                    Some(Filter::FullText {
                        column: "name".into(),
                        query: "Ada".into(),
                    }),
                    None,
                    None,
                    exact_options,
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            table
                .select_with_options(
                    Projection::All,
                    Some(Filter::FullText {
                        column: "name".into(),
                        query: "Ada Lovelace".into(),
                    }),
                    None,
                    None,
                    exact_options,
                )
                .unwrap()
                .len(),
            1
        );

        let vector_options = TableRuntimeOptions {
            vector_index: VectorIndexOptions {
                metric: VectorMetric::DotProduct,
                dimensions: Some(2),
                ..VectorIndexOptions::default()
            },
            worker_threads: 2,
            ..TableRuntimeOptions::default()
        };
        let rows = table
            .select_with_options(
                Projection::Columns(vec!["id".into()]),
                None,
                Some(Order::VectorDistance {
                    column: "embedding".into(),
                    target: vec![1.0, 0.0],
                    descending: false,
                }),
                Some(1),
                vector_options,
            )
            .unwrap();
        assert_eq!(rows[0].get("id"), Some(&Value::Integer(1)));
        assert!(
            table
                .insert_with_options(
                    Some(vec!["id".into(), "embedding".into()]),
                    vec![Value::Integer(3), Value::Vector(vec![1.0])],
                    vector_options,
                )
                .is_err()
        );
        let mut vector_only = Table::new(
            "vectors".into(),
            vec![column("embedding", ColumnType::Vector, false)],
        )
        .unwrap();
        assert!(
            vector_only
                .insert_with_options(None, vec![Value::Vector(vec![1.0])], vector_options,)
                .is_err()
        );
        assert!(
            table
                .select_with_options(
                    Projection::All,
                    None,
                    Some(Order::VectorDistance {
                        column: "embedding".into(),
                        target: vec![1.0],
                        descending: false,
                    }),
                    None,
                    vector_options,
                )
                .is_err()
        );

        let geo_options = TableRuntimeOptions {
            geo_coordinate_system: GeoCoordinateSystem::Cartesian,
            ..TableRuntimeOptions::default()
        };
        assert_eq!(
            table
                .select_with_options(
                    Projection::All,
                    Some(Filter::GeoWithin {
                        column: "place".into(),
                        point: Point { lon: 0.0, lat: 0.0 },
                        meters: 2.5,
                        inclusive: true,
                    }),
                    None,
                    None,
                    geo_options,
                )
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    /// - 验证多类非法操作都会被表层校验拒绝。
    /// - Verifies a range of invalid operations are rejected by table-level validation.
    /// - 场景覆盖坏插入、坏索引、坏过滤和坏向量排序请求。
    /// - The scenario covers invalid inserts, indexes, filters, and vector-order requests.
    fn rejects_invalid_operations() {
        let mut table = users();
        table.insert(None, vec![Value::Integer(1)]).unwrap_err();
        table
            .insert(
                Some(vec!["id".into(), "name".into()]),
                vec![Value::Null, Value::Text("x".into())],
            )
            .unwrap_err();
        assert!(table.validate_primary_insert(&Row::new()).is_err());
        table
            .insert(
                Some(vec!["id".into(), "age".into()]),
                vec![Value::Integer(1), Value::Text("x".into())],
            )
            .unwrap_err();
        table
            .insert(
                Some(vec!["id".into(), "name".into()]),
                vec![Value::Integer(1), Value::Text("x".into())],
            )
            .unwrap();
        table
            .insert(
                Some(vec!["id".into(), "name".into()]),
                vec![Value::Integer(1), Value::Text("dupe".into())],
            )
            .unwrap_err();
        table
            .create_index("bad".into(), "missing".into())
            .unwrap_err();
        table.create_index("age_idx".into(), "age".into()).unwrap();
        table
            .create_index("age_idx".into(), "age".into())
            .unwrap_err();
        table
            .create_fulltext_index("age_idx".into(), "name".into())
            .unwrap_err();
        table
            .create_fulltext_index("age_fts".into(), "age".into())
            .unwrap_err();
        table
            .select(
                Projection::Columns(vec!["missing".into()]),
                None,
                None,
                None,
            )
            .unwrap_err();
        table
            .select(
                Projection::All,
                Some(Filter::FullText {
                    column: "age".into(),
                    query: "x".into(),
                }),
                None,
                None,
            )
            .unwrap_err();
        table
            .select(
                Projection::All,
                Some(Filter::GeoWithin {
                    column: "age".into(),
                    point: Point { lon: 0.0, lat: 0.0 },
                    meters: 1.0,
                    inclusive: false,
                }),
                None,
                None,
            )
            .unwrap_err();
        table
            .select(
                Projection::All,
                None,
                Some(Order::VectorDistance {
                    column: "age".into(),
                    target: vec![0.0],
                    descending: false,
                }),
                None,
            )
            .unwrap_err();
    }

    #[test]
    /// - 验证索引缺失时的回退扫描以及空全文查询行为。
    /// - Verifies fallback scans when indexes are absent and the behavior of empty full-text queries.
    /// - 场景断言等值、全文、地理和向量排序在边界输入下仍返回稳定结果。
    /// - The scenario asserts stable equality, full-text, geo, and vector-order results under edge inputs.
    fn fallback_scans_and_empty_fulltext_queries_work() {
        let mut table = users();
        table
            .insert(
                Some(vec![
                    "id".into(),
                    "name".into(),
                    "age".into(),
                    "score".into(),
                    "embedding".into(),
                    "place".into(),
                ]),
                vec![
                    Value::Integer(1),
                    Value::Text("Ada Lovelace".into()),
                    Value::Integer(36),
                    Value::Float(9.5),
                    Value::Vector(vec![1.0, 1.0]),
                    Value::Point(Point { lon: 0.0, lat: 0.0 }),
                ],
            )
            .unwrap();
        table
            .insert(
                Some(vec!["id".into(), "age".into()]),
                vec![Value::Integer(3), Value::Integer(99)],
            )
            .unwrap();
        table
            .insert(
                Some(vec![
                    "id".into(),
                    "name".into(),
                    "age".into(),
                    "embedding".into(),
                    "place".into(),
                ]),
                vec![
                    Value::Integer(2),
                    Value::Text("Grace Hopper".into()),
                    Value::Integer(85),
                    Value::Vector(vec![5.0, 5.0]),
                    Value::Point(Point { lon: 1.0, lat: 1.0 }),
                ],
            )
            .unwrap();

        assert_eq!(
            table
                .select(
                    Projection::All,
                    Some(Filter::Equals("age".into(), Value::Integer(85))),
                    None,
                    None
                )
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            table
                .select(
                    Projection::All,
                    Some(Filter::FullText {
                        column: "name".into(),
                        query: "ada".into()
                    }),
                    None,
                    None
                )
                .unwrap()
                .len(),
            1
        );
        assert!(
            table
                .select(
                    Projection::All,
                    Some(Filter::FullText {
                        column: "name".into(),
                        query: "   ".into()
                    }),
                    None,
                    None
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            table
                .select(
                    Projection::All,
                    Some(Filter::GeoWithin {
                        column: "place".into(),
                        point: Point { lon: 0.0, lat: 0.0 },
                        meters: 100.0,
                        inclusive: true
                    }),
                    None,
                    None
                )
                .unwrap()
                .len()
                < 3
        );
        assert!(
            table
                .select(
                    Projection::All,
                    Some(Filter::GeoWithin {
                        column: "place".into(),
                        point: Point { lon: 0.0, lat: 0.0 },
                        meters: 0.0,
                        inclusive: false
                    }),
                    None,
                    None
                )
                .unwrap()
                .is_empty()
        );

        let rows = table
            .select(
                Projection::Columns(vec!["id".into()]),
                None,
                Some(Order::VectorDistance {
                    column: "embedding".into(),
                    target: vec![0.0, 0.0],
                    descending: true,
                }),
                Some(1),
            )
            .unwrap();
        assert_eq!(rows[0].get("id"), Some(&Value::Integer(2)));

        let mut tie_table = users();
        for (id, vector) in [(1, vec![1.0, 0.0]), (2, vec![-1.0, 0.0])] {
            tie_table
                .insert(
                    Some(vec!["id".into(), "name".into(), "embedding".into()]),
                    vec![
                        Value::Integer(id),
                        Value::Text(format!("tie{id}")),
                        Value::Vector(vector),
                    ],
                )
                .unwrap();
        }
        let rows = tie_table
            .select(
                Projection::Columns(vec!["id".into()]),
                None,
                Some(Order::VectorDistance {
                    column: "embedding".into(),
                    target: vec![0.0, 0.0],
                    descending: false,
                }),
                None,
            )
            .unwrap();
        assert_eq!(rows[0].get("id"), Some(&Value::Integer(1)));
    }

    #[test]
    /// - 验证过滤解释器会报告正确的索引或扫描路径。
    /// - Verifies the filter explainer reports the correct index or scan path.
    /// - 场景分别覆盖主键、普通索引、全文索引和表扫描选择。
    /// - The scenario covers primary-key, secondary-index, full-text-index, and table-scan choices.
    fn explain_filter_reports_index_and_scan_choices() {
        let mut table = users();
        table.create_index("age_idx".into(), "age".into()).unwrap();
        table
            .create_fulltext_index("name_fts".into(), "name".into())
            .unwrap();
        table
            .insert(
                Some(vec!["id".into(), "name".into(), "age".into()]),
                vec![
                    Value::Integer(1),
                    Value::Text("Ada Lovelace".into()),
                    Value::Integer(36),
                ],
            )
            .unwrap();

        assert_eq!(
            table
                .explain_filter(
                    Some(Filter::Equals("id".into(), Value::Integer(1))),
                    TableRuntimeOptions::default(),
                )
                .unwrap(),
            AccessPath::PrimaryKey,
        );
        assert_eq!(
            table
                .explain_filter(
                    Some(Filter::Equals("age".into(), Value::Integer(36))),
                    TableRuntimeOptions::default(),
                )
                .unwrap(),
            AccessPath::SecondaryIndex {
                index_name: "age_idx".into(),
            },
        );
        assert_eq!(
            table
                .explain_filter(
                    Some(Filter::FullText {
                        column: "name".into(),
                        query: "ada".into(),
                    }),
                    TableRuntimeOptions::default(),
                )
                .unwrap(),
            AccessPath::FullTextIndex {
                index_name: "name_fts".into(),
            },
        );
        assert_eq!(
            table
                .explain_filter(
                    Some(Filter::Equals(
                        "name".into(),
                        Value::Text("Ada Lovelace".into()),
                    )),
                    TableRuntimeOptions::default(),
                )
                .unwrap(),
            AccessPath::TableScan,
        );
    }

    #[test]
    /// - 验证更新触发约束失败时会恢复原始行和索引状态。
    /// - Verifies failed updates restore the original rows and index state.
    /// - 场景通过制造重复主键来断言回滚后的可见结果不变。
    /// - The scenario forces a duplicate primary key to assert visible state remains unchanged after rollback.
    fn update_failure_restores_rows_and_indexes() {
        let mut table = users();
        table
            .insert(
                Some(vec!["id".into(), "name".into()]),
                vec![Value::Integer(1), Value::Text("one".into())],
            )
            .unwrap();
        table
            .insert(
                Some(vec!["id".into(), "name".into()]),
                vec![Value::Integer(2), Value::Text("two".into())],
            )
            .unwrap();
        assert!(
            table
                .update(
                    vec![("id".into(), Value::Integer(1))],
                    Some(Filter::Equals("id".into(), Value::Integer(2)))
                )
                .is_err()
        );
        assert_eq!(
            table
                .select(
                    Projection::All,
                    Some(Filter::Equals("id".into(), Value::Integer(2))),
                    None,
                    None
                )
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    /// - 验证索引重建能识别内部行损坏并容忍部分幽灵索引场景。
    /// - Verifies index rebuilding detects internal row corruption while tolerating selected ghost-index scenarios.
    /// - 场景覆盖缺失主键、空主键、重复主键和持久化索引残留等边界。
    /// - The scenario covers missing keys, null keys, duplicate keys, and leftover persisted-index metadata.
    fn rebuild_indexes_reports_internal_row_corruption() {
        let mut table = users();
        table.rows.insert(1, Row::new());
        assert!(table.rebuild_indexes().is_err());

        let mut table = users();
        let mut row = Row::new();
        row.insert("id".into(), Value::Null);
        table.rows.insert(1, row);
        assert!(table.rebuild_indexes().is_err());

        let mut table = users();
        let mut first = Row::new();
        first.insert("id".into(), Value::Integer(1));
        let mut second = Row::new();
        second.insert("id".into(), Value::Integer(1));
        table.rows.insert(1, first);
        table.rows.insert(2, second);
        assert!(table.rebuild_indexes().is_err());

        let mut table = users();
        let mut row = Row::new();
        row.insert("id".into(), Value::Integer(1));
        table.rows.insert(1, row);
        table.add_persisted_index("ghost".into(), "ghost".into());
        table.add_persisted_fulltext_index("ghost_fts".into(), "ghost".into());
        table.rebuild_indexes().unwrap();

        let mut table = users();
        let mut row = Row::new();
        row.insert("id".into(), Value::Integer(1));
        row.insert("name".into(), Value::Text("Indexed Text".into()));
        table.rows.insert(1, row);
        table.add_persisted_fulltext_index("name_fts".into(), "name".into());
        table.rebuild_indexes().unwrap();

        let mut table = users();
        let mut row = Row::new();
        row.insert("id".into(), Value::Integer(1));
        row.insert("name".into(), Value::Integer(10));
        table.rows.insert(1, row);
        table.add_persisted_fulltext_index("name_fts".into(), "name".into());
        table.rebuild_indexes().unwrap();

        let mut table = Table::new(
            "logs".into(),
            vec![column("message", ColumnType::Text, false)],
        )
        .unwrap();
        table
            .insert(None, vec![Value::Text("no primary".into())])
            .unwrap();
    }

    #[test]
    /// - 覆盖若干底层辅助函数和数据结构的边界行为。
    /// - Covers edge behavior for several low-level helpers and data structures.
    /// - 场景检查分词、距离计算、索引键编码、投影和地理距离等细节。
    /// - The scenario checks tokenization, distance math, index-key encoding, projection, and geo-distance details.
    fn helper_functions_cover_edge_cases() {
        let table = users();
        assert_eq!(table.clone(), table);
        let index = Index {
            column: "id".into(),
            map: BTreeMap::new(),
        };
        assert_eq!(index.clone(), index);
        let fulltext = FullTextIndex {
            column: "name".into(),
            map: BTreeMap::new(),
        };
        assert_eq!(fulltext.clone(), fulltext);
        assert_eq!(
            tokenize("Rust, SQL! rust", FullTextTokenizer::Simple),
            vec!["rust", "sql", "rust"]
        );
        assert_eq!(
            tokenize("Rust-SQL rust", FullTextTokenizer::Whitespace),
            vec!["rust-sql", "rust"]
        );
        assert!(tokenize("   ", FullTextTokenizer::Exact).is_empty());
        assert_eq!(
            vector_distance(&[3.0, 4.0], &[0.0, 0.0], VectorMetric::Euclidean).unwrap(),
            5.0
        );
        assert_eq!(
            vector_distance(&[1.0, 0.0], &[1.0, 0.0], VectorMetric::Cosine).unwrap(),
            0.0
        );
        assert!(vector_distance(&[0.0, 0.0], &[1.0, 0.0], VectorMetric::Cosine).is_err());
        assert!(vector_distance(&[1.0], &[1.0, 2.0], VectorMetric::Euclidean).is_err());
        assert_eq!(index_key(&Value::Null).unwrap(), "N");
        assert!(index_key(&Value::Float(1.0)).unwrap().starts_with("F:"));
        assert_eq!(index_key(&Value::Boolean(true)).unwrap(), "B:1");
        assert!(
            index_key(&Value::Text("x".into()))
                .unwrap()
                .starts_with("T:")
        );
        assert!(
            index_key(&Value::Vector(vec![1.0, 2.0]))
                .unwrap()
                .starts_with("V:")
        );
        assert!(
            index_key(&Value::Point(Point { lon: 1.0, lat: 2.0 }))
                .unwrap()
                .starts_with("P:")
        );
        assert!(index_key(&Value::Float(f64::NAN)).is_err());
        assert!(index_key(&Value::Vector(vec![f32::NAN])).is_err());
        assert!(
            index_key(&Value::Point(Point {
                lon: f64::NAN,
                lat: 0.0
            }))
            .is_err()
        );
        assert_eq!(
            project_row(
                &Row::from([("a".into(), Value::Integer(1))]),
                &Projection::All
            )
            .get("a"),
            Some(&Value::Integer(1))
        );
        assert!(
            haversine_meters(Point { lon: 0.0, lat: 0.0 }, Point { lon: 0.0, lat: 0.0 }) < 0.001
        );
    }
}
