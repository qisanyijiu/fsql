use crate::value::{Point, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ColumnType {
    Integer,
    Float,
    Boolean,
    Text,
    Vector,
    Point,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Column {
    pub(crate) name: String,
    pub(crate) ty: ColumnType,
    pub(crate) primary_key: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Projection {
    All,
    Columns(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Filter {
    Equals(String, Value),
    FullText {
        column: String,
        query: String,
    },
    GeoWithin {
        column: String,
        point: Point,
        meters: f64,
        inclusive: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Order {
    VectorDistance {
        column: String,
        target: Vec<f32>,
        descending: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Statement {
    Begin,
    Commit,
    Rollback,
    CreateTable {
        name: String,
        columns: Vec<Column>,
    },
    CreateIndex {
        name: String,
        table: String,
        column: String,
        fulltext: bool,
    },
    Insert {
        table: String,
        columns: Option<Vec<String>>,
        values: Vec<Value>,
    },
    Select {
        table: String,
        projection: Projection,
        filter: Option<Filter>,
        order: Option<Order>,
        limit: Option<usize>,
    },
    Update {
        table: String,
        assignments: Vec<(String, Value)>,
        filter: Option<Filter>,
    },
    Delete {
        table: String,
        filter: Option<Filter>,
    },
}

impl ColumnType {
    pub(crate) fn parse(input: &str) -> crate::Result<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "int" | "integer" => Ok(Self::Integer),
            "float" | "real" | "double" => Ok(Self::Float),
            "bool" | "boolean" => Ok(Self::Boolean),
            "text" | "string" => Ok(Self::Text),
            "vector" => Ok(Self::Vector),
            "point" | "geo" | "geography" | "coordinate" => Ok(Self::Point),
            other => Err(crate::Error::Parse(format!("unknown column type {other}"))),
        }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Boolean => "boolean",
            Self::Text => "text",
            Self::Vector => "vector",
            Self::Point => "point",
        }
    }
}

impl Statement {
    pub(crate) fn mutates_catalog(&self) -> bool {
        matches!(
            self,
            Self::CreateTable { .. }
                | Self::CreateIndex { .. }
                | Self::Insert { .. }
                | Self::Update { .. }
                | Self::Delete { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_names_column_types() {
        let cases = [
            ("int", ColumnType::Integer, "integer"),
            ("real", ColumnType::Float, "float"),
            ("bool", ColumnType::Boolean, "boolean"),
            ("string", ColumnType::Text, "text"),
            ("vector", ColumnType::Vector, "vector"),
            ("geo", ColumnType::Point, "point"),
        ];
        for (input, expected, name) in cases {
            let parsed = ColumnType::parse(input).expect("type");
            assert_eq!(parsed, expected);
            assert_eq!(parsed.as_str(), name);
        }
        assert!(ColumnType::parse("blob").is_err());
    }

    #[test]
    fn classifies_mutating_statements() {
        assert!(!Statement::Begin.mutates_catalog());
        assert!(!Statement::Commit.mutates_catalog());
        assert!(!Statement::Rollback.mutates_catalog());
        assert!(
            Statement::CreateTable {
                name: "t".into(),
                columns: Vec::new()
            }
            .mutates_catalog()
        );
        assert!(
            Statement::CreateIndex {
                name: "i".into(),
                table: "t".into(),
                column: "c".into(),
                fulltext: false
            }
            .mutates_catalog()
        );
        assert!(
            Statement::Insert {
                table: "t".into(),
                columns: None,
                values: Vec::new()
            }
            .mutates_catalog()
        );
        assert!(
            Statement::Update {
                table: "t".into(),
                assignments: Vec::new(),
                filter: None
            }
            .mutates_catalog()
        );
        assert!(
            Statement::Delete {
                table: "t".into(),
                filter: None
            }
            .mutates_catalog()
        );
    }
}
