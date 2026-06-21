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
    pub(crate) fn create_fulltext_index(&mut self, name: String, column: String) -> Result<()> {
        self.create_fulltext_index_with_options(name, column, TableRuntimeOptions::default())
    }

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
    pub(crate) fn insert(
        &mut self,
        columns: Option<Vec<String>>,
        values: Vec<Value>,
    ) -> Result<()> {
        self.insert_with_options(columns, values, TableRuntimeOptions::default())
    }

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
    pub(crate) fn update(
        &mut self,
        assignments: Vec<(String, Value)>,
        filter: Option<Filter>,
    ) -> Result<usize> {
        self.update_with_options(assignments, filter, TableRuntimeOptions::default())
    }

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
    pub(crate) fn delete(&mut self, filter: Option<Filter>) -> Result<usize> {
        self.delete_with_options(filter, TableRuntimeOptions::default())
    }

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

    pub(crate) fn add_persisted_index(&mut self, name: String, column: String) {
        self.indexes.insert(
            name,
            Index {
                column,
                map: BTreeMap::new(),
            },
        );
    }

    pub(crate) fn add_persisted_fulltext_index(&mut self, name: String, column: String) {
        self.fulltext_indexes.insert(
            name,
            FullTextIndex {
                column,
                map: BTreeMap::new(),
            },
        );
    }

    pub(crate) fn indexes_for_encoding(&self) -> Vec<(&String, &String)> {
        self.indexes
            .iter()
            .map(|(name, index)| (name, &index.column))
            .collect()
    }

    pub(crate) fn fulltext_indexes_for_encoding(&self) -> Vec<(&String, &String)> {
        self.fulltext_indexes
            .iter()
            .map(|(name, index)| (name, &index.column))
            .collect()
    }

    pub(crate) fn rebuild_indexes(&mut self) -> Result<()> {
        self.rebuild_indexes_with_options(TableRuntimeOptions::default())
    }

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

    fn validate_projection(&self, projection: &Projection) -> Result<()> {
        if let Projection::Columns(columns) = projection {
            for column in columns {
                self.column(column)?;
            }
        }
        Ok(())
    }

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

    pub(crate) fn explain_filter(
        &self,
        filter: Option<Filter>,
        options: TableRuntimeOptions,
    ) -> Result<AccessPath> {
        let filter = self.normalize_filter(filter, options)?;
        Ok(self.matching_row_ids(filter.as_ref(), options)?.1)
    }

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
                            ((*inclusive && distance <= *meters) || (!*inclusive && distance < *meters))
                                .then_some(*row_id)
                        }
                        _ => None,
                    })
                    .collect(),
                AccessPath::TableScan,
            )),
        }
    }

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

    fn column(&self, name: &str) -> Result<&Column> {
        let normalized = normalize_identifier(name);
        self.columns
            .iter()
            .find(|column| column.name == normalized)
            .ok_or_else(|| Error::Execution(format!("unknown column {normalized}")))
    }

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

fn index_key(value: &Value) -> Result<String> {
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

fn vector_dimension_error(column: &str, expected: usize) -> Result<Value> {
    let message = format!("vector column {column} requires {expected} dimension(s)");
    Err(Error::Execution(message))
}

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

fn dot_product(left: &[f32], right: &[f32]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| f64::from(*left) * f64::from(*right))
        .sum()
}

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

    fn column(name: &str, ty: ColumnType, primary_key: bool) -> Column {
        Column {
            name: name.into(),
            ty,
            primary_key,
        }
    }

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
