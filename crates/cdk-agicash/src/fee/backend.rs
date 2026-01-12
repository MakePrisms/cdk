//! Fee payout backend abstraction
//!
//! This module defines the trait that payment backends must implement to support
//! automatic fee payouts through the fee manager.

use async_trait::async_trait;

use super::error::FeeError;

/// Fee payout backend trait
///
/// Implement this trait to provide fee payout functionality through any payment backend.
/// The fee manager will call this when it needs to pay out accumulated fees to the
/// configured lightning address.
///
/// # Example Implementation
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use cdk_agicash::{FeePayoutBackend, FeeError};
///
/// struct MyPaymentBackend {
///     // ... fields
/// }
///
/// #[async_trait]
/// impl FeePayoutBackend for MyPaymentBackend {
///     async fn pay_invoice(&self, bolt11: String) -> Result<String, FeeError> {
///         // Parse and validate the invoice
///         let invoice = bolt11.parse()?;
///         
///         // Execute the payment using your backend
///         let result = self.execute_payment(&invoice).await?;
///         
///         // Return the payment ID
///         Ok(result.payment_id)
///     }
/// }
/// ```
#[async_trait]
pub trait FeePayoutBackend: Send + Sync {
    /// Pay a bolt11 lightning invoice
    ///
    /// # Arguments
    ///
    /// * `bolt11` - The bolt11 invoice string to pay
    ///
    /// # Returns
    ///
    /// Returns the payment ID on success, or a [`FeeError`] on failure.
    ///
    /// # Errors
    ///
    /// This method should return an error if:
    /// - The invoice cannot be parsed
    /// - The payment fails
    /// - Network or backend errors occur
    async fn pay_invoice(&self, bolt11: String) -> Result<String, FeeError>;
}
