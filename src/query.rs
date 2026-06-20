use crate::value::Row;

#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    pub rows: Vec<Row>,
    pub affected_rows: usize,
    pub message: String,
}

impl QueryResult {
    pub(crate) fn message(message: impl Into<String>) -> Self {
        Self {
            rows: Vec::new(),
            affected_rows: 0,
            message: message.into(),
        }
    }

    pub(crate) fn affected(affected_rows: usize, message: impl Into<String>) -> Self {
        Self {
            rows: Vec::new(),
            affected_rows,
            message: message.into(),
        }
    }

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

    #[test]
    fn constructors_shape_results() {
        assert_eq!(QueryResult::message("ok").message, "ok");
        assert_eq!(QueryResult::affected(3, "done").affected_rows, 3);
        assert_eq!(QueryResult::rows(vec![Row::new()]).message, "1 row(s)");
    }
}
