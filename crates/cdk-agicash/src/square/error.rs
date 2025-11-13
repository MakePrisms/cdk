//! Error types for Square integration

use thiserror::Error;

/// Square error type
#[derive(Debug, Error)]
pub enum Error {
    /// Square HTTP error
    #[error("Square HTTP error: {0}")]
    SquareHttp(String),

    /// Square configuration error
    #[error("Square configuration error: {0}")]
    SquareConfig(String),

    /// Database error
    #[error("Database error: {0}")]
    Database(#[from] cdk_common::database::Error),

    /// Database connection error
    #[error("Database connection error: {0}")]
    DatabaseConnection(String),

    /// Database query error
    #[error("Database query error: {0}")]
    DatabaseQuery(String),

    /// Bolt11 parsing error
    #[error("Bolt11 parsing error: {0}")]
    Bolt11Parse(String),
}

impl From<tokio_postgres::Error> for Error {
    fn from(err: tokio_postgres::Error) -> Self {
        Error::DatabaseQuery(err.to_string())
    }
}

impl From<Error> for cdk_common::database::Error {
    fn from(err: Error) -> Self {
        cdk_common::database::Error::Database(err.to_string().into())
    }
}
