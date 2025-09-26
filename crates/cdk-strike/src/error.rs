//! Error for Strike ln backend

use strike_rs::Error as StrikeRsError;
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
    /// Strike-rs error
    #[error(transparent)]
    StrikeRs(#[from] StrikeRsError),
    /// Anyhow error
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}

impl From<Error> for cdk_common::payment::Error {
    fn from(e: Error) -> Self {
        Self::Lightning(Box::new(e))
    }
}
