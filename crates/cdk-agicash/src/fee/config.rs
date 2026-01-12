//! Fee configuration types
//!
//! This module provides configuration types for deposit fees.
//! Basis points are a common way to express percentages in financial calculations,
//! where 1 basis point = 0.01% (or 0.0001 as a decimal).
//!
//! Example:
//! - 100 basis points = 1%
//!
//! For fee calculation logic, see the `manager` module which uses
//! `BasisPointsFeeCalculator` as the single source of truth.

use cashu::nuts::nut06::DepositFee;
use serde::{Deserialize, Serialize};

/// Deposit fee configuration (basis points with minimum fee)
///
/// This is a configuration-only type used in TOML files and environment variables.
/// All fee calculation logic is handled by the `FeeManager` and `BasisPointsFeeCalculator`
/// in the `manager` module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositFeeConfig {
    /// Fee in basis points (1 basis point = 0.01%, so 300 = 3%)
    pub basis_points: u64,
    /// Minimum fee in base units (e.g., sats). If the percentage fee is less than this, use the minimum instead.
    #[serde(default)]
    pub minimum_fee: u64,
    /// Lightning address to send collected fees to
    pub lightning_address: String,
}

impl DepositFeeConfig {
    /// Create a new deposit fee config
    pub fn new(basis_points: u64, minimum_fee: u64, lightning_address: String) -> Self {
        Self {
            basis_points,
            minimum_fee,
            lightning_address,
        }
    }

    /// Convert to NUT06 DepositFee for advertising in mint info
    pub fn to_nut06(&self) -> DepositFee {
        DepositFee {
            fee_type: "basis_points".to_string(),
            value: self.basis_points,
            minimum: if self.minimum_fee > 0 {
                Some(self.minimum_fee)
            } else {
                None
            },
        }
    }

    /// Convert to FeeConfig for fee manager initialization
    ///
    /// # Note
    ///
    /// The returned [`FeeConfig`] must be passed to [`crate::FeeManager::new`] along with
    /// a payment callback that implements the actual invoice payment logic. The FeeManager
    /// handles automatic LNURL resolution and payout orchestration.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use cdk_agicash::DepositFeeConfig;
    ///
    /// let fee_config = DepositFeeConfig {
    ///     basis_points: 300,
    ///     minimum_fee: 0,
    ///     lightning_address: "operator@getalby.com".to_string(),
    /// };
    ///
    /// let config = fee_config.to_fee_config("My Mint".to_string());
    /// // Pass config to FeeManager::new with a pay_invoice callback
    /// let fee_manager = FeeManager::new(kv_store, config, pay_callback)?;
    /// ```
    pub fn to_fee_config(&self, mint_name: String) -> crate::FeeConfig {
        crate::FeeConfig::basis_points(
            self.basis_points,
            self.minimum_fee,
            self.lightning_address.clone(),
            mint_name,
        )
    }
}

impl From<u64> for DepositFeeConfig {
    fn from(basis_points: u64) -> Self {
        // This conversion is primarily for testing or simplified scenarios
        // where a default lightning_address is acceptable.
        Self::new(basis_points, 0, "default@example.com".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_u64() {
        let fee_config: DepositFeeConfig = 300u64.into();
        assert_eq!(fee_config.basis_points, 300);
        assert_eq!(fee_config.lightning_address, "default@example.com");
    }
}
