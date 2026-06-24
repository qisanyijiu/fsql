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
    Explain(Box<Statement>),
    ParsedOnly {
        kind: ParsedOnlyStatementKind,
        sql: String,
    },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParsedOnlyStatementKind {
    AlterTable,
    Analyze,
    Attach,
    CreateIndex,
    CreateTable,
    CreateTrigger,
    CreateView,
    CreateVirtualTable,
    Delete,
    Detach,
    DropIndex,
    DropTable,
    DropTrigger,
    DropView,
    Insert,
    Pragma,
    Reindex,
    Release,
    Replace,
    RollbackTo,
    Savepoint,
    Select,
    Update,
    Vacuum,
    Values,
    With,
}

impl ParsedOnlyStatementKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AlterTable => "ALTER TABLE",
            Self::Analyze => "ANALYZE",
            Self::Attach => "ATTACH",
            Self::CreateIndex => "CREATE INDEX",
            Self::CreateTable => "CREATE TABLE",
            Self::CreateTrigger => "CREATE TRIGGER",
            Self::CreateView => "CREATE VIEW",
            Self::CreateVirtualTable => "CREATE VIRTUAL TABLE",
            Self::Delete => "DELETE",
            Self::Detach => "DETACH",
            Self::DropIndex => "DROP INDEX",
            Self::DropTable => "DROP TABLE",
            Self::DropTrigger => "DROP TRIGGER",
            Self::DropView => "DROP VIEW",
            Self::Insert => "INSERT",
            Self::Pragma => "PRAGMA",
            Self::Reindex => "REINDEX",
            Self::Release => "RELEASE",
            Self::Replace => "REPLACE",
            Self::RollbackTo => "ROLLBACK TO",
            Self::Savepoint => "SAVEPOINT",
            Self::Select => "SELECT",
            Self::Update => "UPDATE",
            Self::Vacuum => "VACUUM",
            Self::Values => "VALUES",
            Self::With => "WITH",
        }
    }
}

impl ColumnType {
    /// - 解析列类型关键字为内部 `ColumnType` 枚举。
    /// - Parses a column type keyword into the internal `ColumnType` enum.
    /// - 输入会做首尾裁剪并按 ASCII 小写匹配；未知类型会报错。
    /// - Trims input and matches by ASCII lowercase; unknown types return an error.
    /// - 返回匹配到的列类型，或返回解析错误而不修改外部状态。
    /// - Returns the matched column type, or a parse error with no side effects.
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

    /// - 返回列类型的规范小写名称。
    /// - Returns the canonical lowercase name for the column type.
    /// - 输入为现有 `ColumnType` 变体；映射是固定且无分支副作用的。
    /// - Accepts an existing `ColumnType` variant; mapping is fixed and side-effect free.
    /// - 返回静态字符串切片，不会失败也不会分配。
    /// - Returns a static string slice with no failure and no allocation.
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
    /// - 判断语句是否会修改目录或表中数据。
    /// - Determines whether the statement mutates catalog or table data.
    /// - 输入为任意 `Statement`；仅对建表、建索引和写操作返回真。
    /// - Accepts any `Statement`; only DDL and write operations return true.
    /// - 返回布尔值，不抛错且不产生副作用。
    /// - Returns a boolean with no errors and no side effects.
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

    /// - 校验列类型字符串既能解析也能反向命名。
    /// - Verifies column type strings both parse and map back to names.
    /// - 覆盖别名输入与未知类型；失败时依赖断言报告。
    /// - Covers alias inputs and an unknown type; failures are reported by assertions.
    /// - 无返回值；测试会在断言失败时 panic。
    /// - Returns no value; the test panics on assertion failure.
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

    /// - 校验不同语句变体的变更分类逻辑。
    /// - Verifies mutation classification across statement variants.
    /// - 构造事务、DDL 与 DML 语句样本；依赖固定匹配规则。
    /// - Builds transaction, DDL, and DML samples; relies on fixed match rules.
    /// - 无返回值；测试只通过断言检查布尔结果。
    /// - Returns no value; the test only checks boolean results via assertions.
    #[test]
    fn classifies_mutating_statements() {
        assert!(!Statement::Begin.mutates_catalog());
        assert!(!Statement::Commit.mutates_catalog());
        assert!(!Statement::Rollback.mutates_catalog());
        assert!(
            !Statement::ParsedOnly {
                kind: ParsedOnlyStatementKind::Select,
                sql: "SELECT 1".into()
            }
            .mutates_catalog()
        );
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
        assert!(
            !Statement::ParsedOnly {
                kind: ParsedOnlyStatementKind::DropTable,
                sql: "DROP TABLE t".into()
            }
            .mutates_catalog()
        );
    }
}
