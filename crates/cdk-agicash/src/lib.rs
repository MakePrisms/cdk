//! Extended CDK functionality for Agicash
//!
//! Agicash is a fork of CDK that adds extended functionality beyond the core Cashu protocol.
//! This crate provides the additional features required by Agicash implementations, including
//! closed loop payment management and other Agicash-specific extensions.

#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]

pub mod closed_loop_manager;
pub mod fee;
pub mod lnurl;
pub mod square;
pub mod supervisor;
pub use closed_loop_manager::{
    ClosedLoopConfig, ClosedLoopKind, ClosedLoopManager, ClosedLoopType, PaymentData,
};
pub use fee::{
    BasisPointsFeeCalculator, DepositFeeConfig, FeeCalculator, FeeConfig, FeeError, FeeManager,
    FeePayoutBackend, FeePayoutState, PendingFeePayout,
};
pub use lnurl::{
    resolve_lightning_address, Error as LnUrlError, LnUrlCallbackResponse, LnUrlPayResponse,
};
pub use square::{
    Error as SquareError, LightningDetails, ListMerchantsResponse, ListPaymentsParams,
    ListPaymentsResponse, Merchant, Money, OAuthCredentials, Payment, PaymentBrand, Square,
    SquareConfig, WalletDetails, DEFAULT_SQUARE_PAYMENT_EXPIRY,
};
pub use supervisor::PeriodicSupervisor;
