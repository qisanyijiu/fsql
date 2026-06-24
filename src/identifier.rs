/// - 规范化 SQL 标识符的空白、引用与大小写。
/// - Normalizes SQL identifier whitespace, quoting, and casing.
/// - 输入会裁剪首尾空白并移除常见包裹引号；仅处理 ASCII 小写转换。
/// - Trims surrounding whitespace and strips common wrapping quotes; only ASCII lowercasing is applied.
/// - 返回规范化后的新字符串，不会报错且无其他副作用。
/// - Returns a new normalized string with no errors and no other side effects.
pub(crate) fn normalize_identifier(input: &str) -> String {
    input
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// - 校验标识符规范化会处理引号、空白与大小写。
    /// - Verifies identifier normalization handles quotes, whitespace, and casing.
    /// - 输入覆盖反引号、双引号与单引号形式；输出应统一为小写。
    /// - Covers backtick, double-quote, and single-quote forms; output should be uniformly lowercase.
    /// - 无返回值；测试在断言失败时 panic。
    /// - Returns no value; the test panics on assertion failure.
    #[test]
    fn normalizes_quotes_spaces_and_case() {
        assert_eq!(normalize_identifier(" `MixedCase` "), "mixedcase");
        assert_eq!(normalize_identifier("\"Name\""), "name");
        assert_eq!(normalize_identifier("'Name'"), "name");
    }
}
