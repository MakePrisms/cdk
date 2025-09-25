//! CDK NWC Backend Error Types

use thiserror::Error;

/// NWC Error
#[derive(Debug, Error)]
pub enum Error {
    /// NWC client error
    #[error("NWC client error: {0}")]
    Nwc(#[from] nwc::error::Error),

    /// Unknown invoice amount
    #[error("Unknown invoice amount")]
    UnknownInvoiceAmount,

    /// Invalid URI
    #[error("Invalid NWC URI: {0}")]
    InvalidUri(String),

    /// Unsupported methods
    #[error("Wallet does not support required methods: {0}")]
    UnsupportedMethods(String),

    /// Unsupported notifications
    #[error("Wallet does not support required notifications: {0}")]
    UnsupportedNotifications(String),

    /// Serde JSON error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Connection error
    #[error("Connection error: {0}")]
    Connection(String),
}

impl From<Error> for cdk_common::payment::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::Nwc(e) => cdk_common::payment::Error::Lightning(Box::new(e)),
            Error::UnknownInvoiceAmount => {
                cdk_common::payment::Error::Custom("Unknown invoice amount".to_string())
            }
            Error::InvalidUri(msg) => {
                cdk_common::payment::Error::Custom(format!("Invalid NWC URI: {}", msg))
            }
            Error::UnsupportedMethods(methods) => cdk_common::payment::Error::Custom(format!(
                "Wallet does not support required methods: {}",
                methods
            )),
            Error::UnsupportedNotifications(notifications) => {
                cdk_common::payment::Error::Custom(format!(
                    "Wallet does not support required notifications: {}",
                    notifications
                ))
            }
            Error::Json(e) => cdk_common::payment::Error::Serde(e),
            Error::Connection(msg) => {
                cdk_common::payment::Error::Custom(format!("Connection error: {}", msg))
            }
        }
    }
}
