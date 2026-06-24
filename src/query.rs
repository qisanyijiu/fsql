use crate::value::Row;

#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    pub rows: Vec<Row>,
    pub affected_rows: usize,
    pub message: String,
}

impl QueryResult {
    /// - 构造仅包含消息的查询结果。
    /// - Builds a query result that only carries a message.
    /// - 输入消息可转换为 `String`；不会填充行数据或影响行数。
    /// - Accepts any message convertible to `String`; it does not populate rows or affected count.
    /// - 返回空 `rows`、零 `affected_rows` 的结果对象，无错误。
    /// - Returns a result with empty `rows` and zero `affected_rows`, with no errors.
    pub(crate) fn message(message: impl Into<String>) -> Self {
        Self {
            rows: Vec::new(),
            affected_rows: 0,
            message: message.into(),
        }
    }

    /// - 构造包含影响行数与消息的查询结果。
    /// - Builds a query result with affected-row count and message.
    /// - 输入影响行数与可转字符串消息；不会附带结果行。
    /// - Accepts an affected-row count and message convertible to string; it does not attach rows.
    /// - 返回空 `rows` 的结果对象，并保留给定影响行数。
    /// - Returns a result with empty `rows` while preserving the provided affected-row count.
    pub(crate) fn affected(affected_rows: usize, message: impl Into<String>) -> Self {
        Self {
            rows: Vec::new(),
            affected_rows,
            message: message.into(),
        }
    }

    /// - 构造包含结果行的查询结果并生成默认消息。
    /// - Builds a query result with rows and generates a default message.
    /// - 输入为完整 `Row` 列表；消息格式固定为 `<n> row(s)`。
    /// - Accepts a full `Row` list; the message format is fixed as `<n> row(s)`.
    /// - 返回 `affected_rows = 0` 的结果对象，不会失败。
    /// - Returns a result with `affected_rows = 0` and cannot fail.
    pub(crate) fn rows(rows: Vec<Row>) -> Self {
        let message = format!("{} row(s)", rows.len());
        Self {
            rows,
            affected_rows: 0,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// - 校验三个构造器生成的结果形状。
    /// - Verifies the shapes produced by the three constructors.
    /// - 覆盖消息、影响行数与行结果路径；依赖精确字段断言。
    /// - Covers message, affected-row, and row-result paths; relies on exact field assertions.
    /// - 无返回值；测试在断言失败时 panic。
    /// - Returns no value; the test panics on assertion failure.
    #[test]
    fn constructors_shape_results() {
        assert_eq!(QueryResult::message("ok").message, "ok");
        assert_eq!(QueryResult::affected(3, "done").affected_rows, 3);
        assert_eq!(QueryResult::rows(vec![Row::new()]).message, "1 row(s)");
    }
}
