use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Parse(String),
    Execution(String),
    Io(String),
}

impl fmt::Display for Error {
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
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_all_error_variants() {
        assert_eq!(Error::Parse("x".into()).to_string(), "parse error: x");
        assert_eq!(
            Error::Execution("x".into()).to_string(),
            "execution error: x"
        );
        assert_eq!(Error::Io("x".into()).to_string(), "io error: x");
    }

    #[test]
    fn converts_io_errors() {
        let error = Error::from(std::io::Error::new(std::io::ErrorKind::Other, "disk"));
        assert_eq!(error, Error::Io("disk".into()));
    }
}
