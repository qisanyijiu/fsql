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

    #[test]
    fn normalizes_quotes_spaces_and_case() {
        assert_eq!(normalize_identifier(" `MixedCase` "), "mixedcase");
        assert_eq!(normalize_identifier("\"Name\""), "name");
        assert_eq!(normalize_identifier("'Name'"), "name");
    }
}
