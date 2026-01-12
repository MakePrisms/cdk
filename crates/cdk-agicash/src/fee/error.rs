//! Error types for fee operations

use cdk_common::nuts::CurrencyUnit;

/// Error type for fee operations
#[derive(Debug, thiserror::Error)]
pub enum FeeError {
    /// Unsupported currency unit for fee payout
    #[error("Fee payout via lightning address not supported for {0:?} unit. Only sat/msat are supported.")]
    UnsupportedUnit(CurrencyUnit),

    /// Payment error
    #[error(transparent)]
    Payment(#[from] cdk_common::payment::Error),

    /// Database error
    #[error(transparent)]
    Database(#[from] cdk_common::database::Error),

    /// Lightning invoice parse error
    #[error(transparent)]
    InvoiceParse(#[from] cdk_common::lightning_invoice::ParseOrSemanticError),

    /// HTTP/Network error for LNURL
    #[error(transparent)]
    Http(#[from] reqwest::Error),

    /// LNURL error
    #[error(transparent)]
    Lnurl(#[from] crate::lnurl::Error),

    /// General error
    #[error("{0}")]
    Other(String),
}

impl From<String> for FeeError {
    fn from(s: String) -> Self {
        FeeError::Other(s)
    }
}

impl From<&str> for FeeError {
    fn from(s: &str) -> Self {
        FeeError::Other(s.to_string())
    }
}

impl From<serde_json::Error> for FeeError {
    fn from(e: serde_json::Error) -> Self {
        FeeError::Database(e.into())
    }
}
