//! Strike API error types
//!
//! See <https://docs.strike.me/api/> for the complete error reference.
//!
//! # Error Handling
//!
//! The Strike API uses standard HTTP response codes:
//! - 2xx: Success
//! - 4xx: Client errors (invalid request, insufficient permissions, etc.)
//! - 5xx: Server errors
//!
//! Error responses follow this JSON structure:
//!
//! ```json
//! {
//!   "traceId": "optional-trace-id",
//!   "data": {
//!     "status": 400,
//!     "code": "INVALID_DATA",
//!     "message": "Human-readable error message",
//!     "validationErrors": {}
//!   }
//! }
//! ```
//!
//! Note: Error messages are for developer reference only and may change without
//! notice. Do not display them to end users.
//!
//! # Error Codes
//!
//! [`StrikeErrorCode`] covers all known Strike API error codes. Key codes include:
//!
//! - `RATE_LIMIT_EXCEEDED` / `TOO_MANY_ATTEMPTS` - Retry with exponential backoff
//! - `BALANCE_TOO_LOW` - Insufficient account balance
//! - `PAYMENT_QUOTE_EXPIRED` - Quote expired, create a new one
//! - `LN_ROUTE_NOT_FOUND` - Lightning payment routing failed
//! - `INVALID_LN_INVOICE` - Malformed BOLT11 invoice
//! - `DUPLICATE_PAYMENT_QUOTE` - Use idempotency-key header to prevent duplicates
//!
//! Use [`StrikeErrorCode::is_retryable()`] to check if an error is transient.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Strike API error
#[derive(Debug, Error)]
pub enum Error {
    /// Resource not found (404)
    #[error("Not found")]
    NotFound,

    /// Invalid URL format
    #[error("Invalid URL: {0}")]
    InvalidUrl(#[from] url::ParseError),

    /// HTTP request error
    #[error("HTTP error: {0}")]
    Reqwest(#[from] reqwest::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Strike API returned an error response
    #[error("Strike API error: {0}")]
    Api(#[from] StrikeApiError),
}

/// Detailed Strike API error response
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Error)]
#[serde(rename_all = "camelCase")]
#[error("{}", self.data.message)]
pub struct StrikeApiError {
    /// Optional trace ID for debugging
    #[serde(default)]
    pub trace_id: Option<String>,
    /// Error details
    pub data: StrikeApiErrorData,
}

impl StrikeApiError {
    /// Get the HTTP status code
    pub fn status(&self) -> u16 {
        self.data.status
    }

    /// Get the error code
    pub fn code(&self) -> &StrikeErrorCode {
        &self.data.code
    }

    /// Get the error message
    pub fn message(&self) -> &str {
        &self.data.message
    }

    /// Check if this error matches a specific code
    pub fn is_error_code(&self, code: &StrikeErrorCode) -> bool {
        &self.data.code == code
    }

    /// Check if this is a rate limit error
    pub fn is_rate_limit_error(&self) -> bool {
        matches!(
            self.data.code,
            StrikeErrorCode::RateLimitExceeded | StrikeErrorCode::TooManyAttempts
        )
    }

    /// Check if this is a server error (5xx)
    pub fn is_server_error(&self) -> bool {
        self.data.status >= 500
    }

    /// Check if this is a client error (4xx)
    pub fn is_client_error(&self) -> bool {
        self.data.status >= 400 && self.data.status < 500
    }

    /// Check if this error is retryable
    pub fn is_retryable(&self) -> bool {
        self.data.code.is_retryable()
    }
}

/// Strike API error data payload
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrikeApiErrorData {
    /// HTTP status code
    pub status: u16,
    /// Error code enum
    pub code: StrikeErrorCode,
    /// Human-readable error message
    pub message: String,
    /// Additional context values
    #[serde(default)]
    pub values: HashMap<String, serde_json::Value>,
    /// Field-specific validation errors
    #[serde(default)]
    pub validation_errors: HashMap<String, Vec<ValidationError>>,
    /// Debug information (only in non-production)
    #[serde(default)]
    pub debug: Option<DebugInfo>,
}

/// Validation error for a specific field
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ValidationError {
    /// Validation error code
    pub code: ValidationErrorCode,
    /// Human-readable validation message
    pub message: String,
    /// Additional context values
    #[serde(default)]
    pub values: HashMap<String, serde_json::Value>,
}

/// Debug information included in error responses
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DebugInfo {
    /// Full debug output
    #[serde(default)]
    pub full: Option<String>,
    /// Request body that caused the error
    #[serde(default)]
    pub body: Option<String>,
}

/// Strike API error codes
///
/// These codes are returned in the `code` field of error responses.
/// Use [`is_retryable`](StrikeErrorCode::is_retryable) to check if an error
/// should be retried with backoff.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StrikeErrorCode {
    /// Resource not found
    NotFound,
    /// Internal server error
    InternalServerError,
    /// Bad gateway
    BadGateway,
    /// Service in maintenance mode
    MaintenanceMode,
    /// Service temporarily unavailable
    ServiceUnavailable,
    /// Gateway timeout
    GatewayTimeout,
    /// Rate limit exceeded
    RateLimitExceeded,
    /// Too many attempts
    TooManyAttempts,
    /// Concurrent processing conflict
    ProcessingConflict,
    /// Authentication required
    Unauthorized,
    /// Permission denied
    Forbidden,
    /// Invalid request data
    InvalidData,
    /// Invalid query parameters
    InvalidDataQuery,
    /// Request could not be processed
    UnprocessableEntity,
    /// Account setup incomplete
    AccountNotReady,
    /// Invoice already paid
    InvalidStateForInvoicePaid,
    /// Invoice already reversed
    InvalidStateForInvoiceReversed,
    /// Invoice already cancelled
    InvalidStateForInvoiceCancelled,
    /// Invalid payment recipient
    InvalidRecipient,
    /// Payment currently processing
    ProcessingPayment,
    /// Duplicate invoice
    DuplicateInvoice,
    /// Cannot pay to self
    SelfPaymentNotAllowed,
    /// Currency not available for user
    UserCurrencyUnavailable,
    /// Lightning Network unavailable
    LnUnavailable,
    /// Exchange rate not available
    ExchangeRateNotAvailable,
    /// Insufficient balance
    BalanceTooLow,
    /// Invalid amount
    InvalidAmount,
    /// Amount exceeds maximum
    AmountTooHigh,
    /// Amount below minimum
    AmountTooLow,
    /// Payment method not supported
    UnsupportedPaymentMethod,
    /// Invalid payment method
    InvalidPaymentMethod,
    /// Currency not supported
    CurrencyUnsupported,
    /// Plaid linking failed
    PlaidLinkingFailed,
    /// Payment method not ready
    PaymentMethodNotReady,
    /// Payout originator not approved
    PayoutOriginatorNotApproved,
    /// Payout already initiated
    PayoutAlreadyInitiated,
    /// Invoice has expired
    InvalidStateForInvoiceExpired,
    /// Payment already processed
    PaymentProcessed,
    /// Invalid Lightning invoice
    InvalidLnInvoice,
    /// Lightning invoice already processed
    LnInvoiceProcessed,
    /// Invalid Bitcoin address
    InvalidBitcoinAddress,
    /// Lightning route not found
    LnRouteNotFound,
    /// Payment quote has expired
    PaymentQuoteExpired,
    /// Duplicate payment quote
    DuplicatePaymentQuote,
    /// Too many transactions
    TooManyTransactions,
    /// Duplicate currency exchange quote
    DuplicateCurrencyExchangeQuote,
    /// Currency exchange quote already processed
    CurrencyExchangeQuoteProcessed,
    /// Currency exchange quote expired
    CurrencyExchangeQuoteExpired,
    /// Currency exchange pair not supported
    CurrencyExchangePairNotSupported,
    /// Currency exchange amount too low
    CurrencyExchangeAmountTooLow,
    /// Deposit limit exceeded
    DepositLimitExceeded,
    /// Duplicate deposit
    DuplicateDeposit,
    /// Unknown error code
    #[serde(other)]
    Unknown,
}

impl StrikeErrorCode {
    /// Get the typical HTTP status for this error code
    pub fn typical_status(&self) -> u16 {
        match self {
            StrikeErrorCode::NotFound => 404,
            StrikeErrorCode::InternalServerError => 500,
            StrikeErrorCode::BadGateway => 502,
            StrikeErrorCode::MaintenanceMode | StrikeErrorCode::ServiceUnavailable => 503,
            StrikeErrorCode::GatewayTimeout => 504,
            StrikeErrorCode::RateLimitExceeded | StrikeErrorCode::TooManyAttempts => 429,
            StrikeErrorCode::ProcessingConflict | StrikeErrorCode::DuplicateInvoice => 409,
            StrikeErrorCode::Unauthorized => 401,
            StrikeErrorCode::Forbidden => 403,
            StrikeErrorCode::InvalidData | StrikeErrorCode::InvalidDataQuery => 400,
            StrikeErrorCode::AccountNotReady => 425,
            _ => 422,
        }
    }

    /// Check if this error is retryable
    ///
    /// Returns `true` for transient errors that may succeed on retry:
    /// - Server errors (5xx)
    /// - Rate limiting
    /// - Processing conflicts
    /// - Lightning Network unavailable
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            StrikeErrorCode::InternalServerError
                | StrikeErrorCode::BadGateway
                | StrikeErrorCode::MaintenanceMode
                | StrikeErrorCode::ServiceUnavailable
                | StrikeErrorCode::GatewayTimeout
                | StrikeErrorCode::RateLimitExceeded
                | StrikeErrorCode::TooManyAttempts
                | StrikeErrorCode::ProcessingConflict
                | StrikeErrorCode::LnUnavailable
        )
    }
}

/// Validation error codes for field-level errors
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ValidationErrorCode {
    /// Generic invalid data
    InvalidData,
    /// Required field missing
    InvalidDataRequired,
    /// Invalid field length
    InvalidDataLength,
    /// Field too short
    InvalidDataMinlength,
    /// Field too long
    InvalidDataMaxlength,
    /// Invalid field value
    InvalidDataValue,
    /// Invalid currency
    InvalidDataCurrency,
    /// Unknown validation error
    #[serde(other)]
    Unknown,
}
