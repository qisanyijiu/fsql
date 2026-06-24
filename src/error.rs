use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Parse(String),
    Execution(String),
    Io(String),
}

impl fmt::Display for Error {
    /// - 将错误枚举格式化为用户可读文本。
    /// - Formats the error enum into user-facing text.
    /// - 输入为现有错误变体与格式化器；输出前缀取决于错误类别。
    /// - Accepts an existing error variant and formatter; the prefix depends on the error kind.
    /// - 返回 `fmt::Result`，仅向格式化器写入文本且不改变错误值。
    /// - Returns `fmt::Result`, only writing text to the formatter without mutating the error.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "parse error: {message}"),
            Self::Execution(message) => write!(f, "execution error: {message}"),
            Self::Io(message) => write!(f, "io error: {message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    /// - 将标准库 I/O 错误转换为本项目错误类型。
    /// - Converts a standard-library I/O error into the project error type.
    /// - 输入会被消费并使用其字符串表示；不会保留原始错误对象。
    /// - Consumes the input and uses its string form; it does not preserve the original error object.
    /// - 返回 `Error::Io` 变体，无失败路径且无额外副作用。
    /// - Returns an `Error::Io` variant with no failure path and no extra side effects.
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// - 校验所有错误变体的显示文本格式。
    /// - Verifies the display formatting for all error variants.
    /// - 构造固定消息样本；断言输出前缀与内容完全匹配。
    /// - Builds fixed message samples; asserts exact prefix and content matches.
    /// - 无返回值；测试在断言失败时 panic。
    /// - Returns no value; the test panics on assertion failure.
    #[test]
    fn displays_all_error_variants() {
        assert_eq!(Error::Parse("x".into()).to_string(), "parse error: x");
        assert_eq!(
            Error::Execution("x".into()).to_string(),
            "execution error: x"
        );
        assert_eq!(Error::Io("x".into()).to_string(), "io error: x");
    }

    /// - 校验标准 I/O 错误到项目错误的转换行为。
    /// - Verifies conversion from a standard I/O error into the project error.
    /// - 输入使用 `std::io::ErrorKind::Other` 示例；依赖字符串化结果。
    /// - Uses a `std::io::ErrorKind::Other` sample input; relies on the stringified message.
    /// - 无返回值；测试通过断言检查转换后的枚举值。
    /// - Returns no value; the test checks the converted enum via assertions.
    #[test]
    fn converts_io_errors() {
        let error = Error::from(std::io::Error::new(std::io::ErrorKind::Other, "disk"));
        assert_eq!(error, Error::Io("disk".into()));
    }
}
