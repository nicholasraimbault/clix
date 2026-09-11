use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClixError {
    #[error("{0}")]
    Usage(String),
}

pub type Result<T> = std::result::Result<T, ClixError>;
