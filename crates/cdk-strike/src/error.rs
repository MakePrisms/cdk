//! Error for Strike ln backend

use crate::api::error::Error as StrikeApiError;
use thiserror::Error;

/// Strike Error
#[derive(Debug, Error)]
pub enum Error {
    /// Invoice amount not defined
    #[error("Unknown invoice amount")]
    UnknownInvoiceAmount,
    /// Unknown invoice
    #[error("Unknown invoice")]
    UnknownInvoice,
    /// Unsupported unit
    #[error("Unsupported unit")]
    UnsupportedUnit,
    /// Strike API error
    #[error(transparent)]
    StrikeApi(#[from] StrikeApiError),
    /// Anyhow error
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}

impl From<Error> for cdk_common::payment::Error {
    fn from(e: Error) -> Self {
        Self::Lightning(Box::new(e))
    }
}
