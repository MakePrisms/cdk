//! Square payment backend integration for CDK
//!
//! This module provides Square Lightning payment tracking and webhook handling
//! for internal payment detection in payment processor backends.
//!
//! # Features
//!
//! - **Webhook Mode**: Real-time payment notifications with signature verification
//! - **Polling Mode**: Automatic fallback to polling every 5 seconds
//! - **KV Store Integration**: Persistent storage of payment data
//! - **Payment Hash Tracking**: Maps Lightning payment hashes to Square payment IDs
//!
//! # Usage
//!
//! ```rust,no_run
//! use cdk_agicash::square::{Square, SquareConfig};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! # let kv_store = todo!();
//! let config = SquareConfig {
//!     api_token: "your-api-token".to_string(),
//!     environment: "SANDBOX".to_string(),
//!     database_url: "postgresql://user:pass@host:5432/db?sslmode=require".to_string(),
//!     webhook_enabled: true,
//!     payment_expiry: 300, // 5 minutes
//! };
//!
//! let square = Square::from_config(
//!     config,
//!     Some("https://your-mint.com/webhook".to_string()),
//!     kv_store,
//! )?;
//!
//! square.start().await?;
//! # Ok(())
//! # }
//! ```

mod api;
pub mod client;
pub mod config;
pub mod db;
pub mod error;
pub mod sync;
pub mod types;
pub mod util;
pub mod webhook;

// Re-export main types
pub use client::Square;
pub use config::SquareConfig;
pub use db::OAuthCredentials;
pub use error::Error;
pub use types::{
    LightningDetails, ListMerchantsResponse, ListPaymentsParams, ListPaymentsResponse, Merchant,
    Money, Payment, PaymentBrand, WalletDetails,
};

/// Default payment expiry time in seconds
pub const DEFAULT_SQUARE_PAYMENT_EXPIRY: u64 = 500;