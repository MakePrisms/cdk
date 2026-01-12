//! Fee calculation logic

use super::config::DepositFeeConfig;

/// Trait for calculating fees
///
/// This trait provides an abstraction for different fee calculation strategies,
/// allowing for extensibility beyond the default basis points calculator. Payment
/// backends can implement custom fee calculators (e.g., tiered pricing, fixed fees,
/// dynamic fee, etc.) without modifying the core fee management logic.
///
/// The calculator only handles the mathematical calculation logic and does not
/// include payout configuration like lightning addresses, which are handled by [`FeeConfig`](super::manager::FeeConfig).
///
/// # Example
///
/// ```rust,ignore
/// use cdk_agicash::FeeCalculator;
///
/// struct CustomFeeCalculator {
///     base_fee: u64,
/// }
///
/// impl FeeCalculator for CustomFeeCalculator {
///     fn calculate_fee(&self, amount: u64) -> u64 {
///         // Custom logic: $1 flat fee + 1% of amount
///         self.base_fee + (amount / 100)
///     }
/// }
/// ```
pub trait FeeCalculator: Send + Sync {
    /// Calculate the fee amount for a given amount
    fn calculate_fee(&self, amount: u64) -> u64;
}

/// Basis points fee calculator
///
/// Calculates fees based on a percentage (in basis points) with an optional minimum fee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasisPointsFeeCalculator {
    /// Fee in basis points (1 basis point = 0.01%, so 300 = 3%)
    pub basis_points: u64,
    /// Minimum fee in base units (e.g., sats)
    pub minimum_fee: u64,
}

impl BasisPointsFeeCalculator {
    /// Create a new basis points fee calculator
    pub fn new(basis_points: u64, minimum_fee: u64) -> Self {
        Self {
            basis_points,
            minimum_fee,
        }
    }
}

impl From<&DepositFeeConfig> for BasisPointsFeeCalculator {
    fn from(config: &DepositFeeConfig) -> Self {
        Self::new(config.basis_points, config.minimum_fee)
    }
}

impl From<DepositFeeConfig> for BasisPointsFeeCalculator {
    fn from(config: DepositFeeConfig) -> Self {
        Self::new(config.basis_points, config.minimum_fee)
    }
}

impl FeeCalculator for BasisPointsFeeCalculator {
    fn calculate_fee(&self, amount: u64) -> u64 {
        let percentage_fee = (amount * self.basis_points) / 10000;
        std::cmp::max(percentage_fee, self.minimum_fee)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basis_points_fee_calculator_calculate_fee() {
        let calculator = BasisPointsFeeCalculator::new(300, 10);

        // Test percentage fee
        assert_eq!(calculator.calculate_fee(10000), 300); // 3% of 10000 = 300

        // Test minimum fee applies
        assert_eq!(calculator.calculate_fee(100), 10); // 3% of 100 = 3, but minimum is 10

        // Test larger amount
        assert_eq!(calculator.calculate_fee(1000), 30); // 3% of 1000 = 30
    }

    #[test]
    fn test_from_deposit_fee_config() {
        let config = DepositFeeConfig {
            basis_points: 300,
            minimum_fee: 10,
            lightning_address: "test@example.com".to_string(),
        };
        let calculator: BasisPointsFeeCalculator = config.into();

        assert_eq!(calculator.basis_points, 300);
        assert_eq!(calculator.minimum_fee, 10);
        assert_eq!(calculator.calculate_fee(10000), 300);
        assert_eq!(calculator.calculate_fee(100), 10);
    }

    #[test]
    fn test_from_deposit_fee_config_ref() {
        let config = DepositFeeConfig {
            basis_points: 300,
            minimum_fee: 10,
            lightning_address: "test@example.com".to_string(),
        };
        let calculator: BasisPointsFeeCalculator = (&config).into();

        assert_eq!(calculator.basis_points, 300);
        assert_eq!(calculator.minimum_fee, 10);
    }
}
