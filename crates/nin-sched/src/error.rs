use std::fmt;

#[derive(Debug)]
pub enum SchedError {
    Parse(String),
    Io(std::io::Error),
    Duplicate(String),
    NotFound(String),
    Unsupported(String),
    Os(String),
    Toml(String),
}

impl fmt::Display for SchedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchedError::Parse(m) => write!(f, "parse error: {m}"),
            SchedError::Io(e) => write!(f, "I/O error: {e}"),
            SchedError::Duplicate(id) => write!(f, "duplicate task id: {id}"),
            SchedError::NotFound(id) => write!(f, "task not found: {id}"),
            SchedError::Unsupported(m) => write!(f, "unsupported: {m}"),
            SchedError::Os(m) => write!(f, "OS scheduler error: {m}"),
            SchedError::Toml(m) => write!(f, "schedule file error: {m}"),
        }
    }
}

impl std::error::Error for SchedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SchedError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SchedError {
    fn from(e: std::io::Error) -> Self {
        SchedError::Io(e)
    }
}
