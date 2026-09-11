use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ClixError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Json(String),
    #[error("no such tool: {tool}")]
    NoSuchTool { tool: String },
    #[error("{tool} is not added on {body}")]
    NotAdded { tool: String, body: String },
    #[error("{from} is not allowed to use {tool} on {body}")]
    NotAllowed {
        tool: String,
        from: String,
        body: String,
    },
    #[error("{tool} grant on {body} has expired")]
    Expired { tool: String, body: String },
    #[error("{tool} is not added on {body} at this time")]
    NotAddedAtTime { tool: String, body: String },
}

impl From<std::io::Error> for ClixError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<serde_json::Error> for ClixError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ClixError>;
