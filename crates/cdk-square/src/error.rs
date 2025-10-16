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

    /// Bolt11 parsing error
    #[error("Bolt11 parsing error: {0}")]
    Bolt11Parse(String),
}
