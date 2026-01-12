//! Fee management and payout functionality
//!
//! This module provides functionality for calculating, tracking, and paying out fees
//! to lightning addresses when deposits are received.

pub mod backend;
pub mod calculator;
pub mod config;
pub mod error;
pub mod manager;
pub mod payout;

// Re-export main types for convenience
pub use backend::FeePayoutBackend;
pub use calculator::{BasisPointsFeeCalculator, FeeCalculator};
pub use config::DepositFeeConfig;
pub use error::FeeError;
pub use manager::{FeeConfig, FeeManager};
pub use payout::{FeePayoutState, PendingFeePayout};
