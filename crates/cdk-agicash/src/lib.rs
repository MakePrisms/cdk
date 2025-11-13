//! Extended CDK functionality for Agicash
//!
//! Agicash is a fork of CDK that adds extended functionality beyond the core Cashu protocol.
//! This crate provides the additional features required by Agicash implementations, including
//! closed loop payment management and other Agicash-specific extensions.

#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]

pub mod closed_loop_manager;

// Re-export main types
pub use closed_loop_manager::{ClosedLoopConfig, ClosedLoopManager, ClosedLoopType, PaymentData};
