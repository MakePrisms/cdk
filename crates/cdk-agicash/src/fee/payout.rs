//! Payout state management and data structures

use std::convert::TryFrom;
use std::time::Duration;

use cdk_common::nuts::CurrencyUnit;

/// Primary namespace for fee data in KV store
pub const FEE_PRIMARY_NAMESPACE: &str = "cdk_fee_manager";
/// Secondary namespace for pending fee payouts in KV store
pub const FEE_SECONDARY_NAMESPACE: &str = "pending_payouts";
/// How often to run cleanup of paid/expired fee payouts
pub const CLEANUP_PERIOD: Duration = Duration::from_secs(86400); // 24 hours

/// State of a fee payout
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeePayoutState {
    /// Payout has not been initiated yet
    Unpaid,
    /// Payout is in progress
    Pending,
    /// Payout was successful
    Paid,
    /// Payout failed with error message
    Failed(String),
}

/// Pending fee payout data stored in KV store
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingFeePayout {
    /// Invoice ID this fee is associated with
    pub invoice_id: String,
    /// Base amount (what we report to the mint as the quote amount)
    pub base_amount: u64,
    /// Fee amount to be paid out
    pub fee_amount: u64,
    /// Unit of the amounts
    pub unit: CurrencyUnit,
    /// Timestamp when this was created
    pub created_at: u64,
    /// Timestamp when the record was last updated
    pub updated_at: u64,
    /// Timestamp when the payment expires
    pub expires_at: u64,
    /// Current state of the payout
    pub state: FeePayoutState,
}

impl PendingFeePayout {
    /// Create a new pending fee payout
    pub fn new(
        invoice_id: String,
        base_amount: u64,
        fee_amount: u64,
        unit: CurrencyUnit,
        created_at: u64,
        expires_at: u64,
    ) -> Self {
        Self {
            invoice_id,
            base_amount,
            fee_amount,
            unit,
            created_at,
            updated_at: created_at,
            expires_at,
            state: FeePayoutState::Unpaid,
        }
    }
}

impl From<PendingFeePayout> for Vec<u8> {
    fn from(payout: PendingFeePayout) -> Self {
        serde_json::to_vec(&payout).unwrap_or_default()
    }
}

impl TryFrom<&[u8]> for PendingFeePayout {
    type Error = serde_json::Error;

    fn try_from(data: &[u8]) -> Result<Self, Self::Error> {
        serde_json::from_slice(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pending_fee_payout_serialization() {
        let payout = PendingFeePayout::new(
            "invoice_123".to_string(),
            10000,
            300,
            CurrencyUnit::Sat,
            1234567890,
            1234571490,
        );

        let bytes: Vec<u8> = payout.clone().into();
        assert!(!bytes.is_empty());

        let decoded = PendingFeePayout::try_from(bytes.as_slice()).unwrap();
        assert_eq!(decoded.invoice_id, "invoice_123");
        assert_eq!(decoded.base_amount, 10000);
        assert_eq!(decoded.fee_amount, 300);
        assert_eq!(decoded.state, FeePayoutState::Unpaid);
        assert_eq!(decoded.updated_at, 1234567890);
    }
}
