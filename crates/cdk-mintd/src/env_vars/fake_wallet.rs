//! FakeWallet environment variables

use std::env;

use cdk::nuts::CurrencyUnit;

use crate::config::{FakeWallet, SquareConfig};

// Fake Wallet environment variables
pub const ENV_FAKE_WALLET_SUPPORTED_UNITS: &str = "CDK_MINTD_FAKE_WALLET_SUPPORTED_UNITS";
pub const ENV_FAKE_WALLET_FEE_PERCENT: &str = "CDK_MINTD_FAKE_WALLET_FEE_PERCENT";
pub const ENV_FAKE_WALLET_RESERVE_FEE_MIN: &str = "CDK_MINTD_FAKE_WALLET_RESERVE_FEE_MIN";
pub const ENV_FAKE_WALLET_MIN_DELAY: &str = "CDK_MINTD_FAKE_WALLET_MIN_DELAY";
pub const ENV_FAKE_WALLET_MAX_DELAY: &str = "CDK_MINTD_FAKE_WALLET_MAX_DELAY";

// Square environment variables (optional integration with Fake Wallet)
pub const ENV_FAKE_WALLET_SQUARE_API_TOKEN: &str = "CDK_MINTD_FAKE_WALLET_SQUARE_API_TOKEN";
pub const ENV_FAKE_WALLET_SQUARE_ENVIRONMENT: &str = "CDK_MINTD_FAKE_WALLET_SQUARE_ENVIRONMENT";
pub const ENV_FAKE_WALLET_SQUARE_WEBHOOK_ENABLED: &str =
    "CDK_MINTD_FAKE_WALLET_SQUARE_WEBHOOK_ENABLED";
pub const ENV_FAKE_WALLET_SQUARE_DATABASE_URL: &str = "CDK_MINTD_FAKE_WALLET_SQUARE_DATABASE_URL";

impl FakeWallet {
    pub fn from_env(mut self) -> Self {
        // Supported Units - expects comma-separated list
        if let Ok(units_str) = env::var(ENV_FAKE_WALLET_SUPPORTED_UNITS) {
            if let Ok(units) = units_str
                .split(',')
                .map(|s| s.trim().parse())
                .collect::<Result<Vec<CurrencyUnit>, _>>()
            {
                self.supported_units = units;
            }
        }

        if let Ok(fee_str) = env::var(ENV_FAKE_WALLET_FEE_PERCENT) {
            if let Ok(fee) = fee_str.parse() {
                self.fee_percent = fee;
            }
        }

        if let Ok(reserve_fee_str) = env::var(ENV_FAKE_WALLET_RESERVE_FEE_MIN) {
            if let Ok(reserve_fee) = reserve_fee_str.parse::<u64>() {
                self.reserve_fee_min = reserve_fee.into();
            }
        }

        if let Ok(min_delay_str) = env::var(ENV_FAKE_WALLET_MIN_DELAY) {
            if let Ok(min_delay) = min_delay_str.parse() {
                self.min_delay_time = min_delay;
            }
        }

        if let Ok(max_delay_str) = env::var(ENV_FAKE_WALLET_MAX_DELAY) {
            if let Ok(max_delay) = max_delay_str.parse() {
                self.max_delay_time = max_delay;
            }
        }

        // Square integration (optional)
        if let Ok(api_token) = env::var(ENV_FAKE_WALLET_SQUARE_API_TOKEN) {
            let environment = env::var(ENV_FAKE_WALLET_SQUARE_ENVIRONMENT)
                .unwrap_or_else(|_| "SANDBOX".to_string());

            let webhook_enabled = env::var(ENV_FAKE_WALLET_SQUARE_WEBHOOK_ENABLED)
                .ok()
                .and_then(|v| v.parse::<bool>().ok())
                .unwrap_or(true);

            let database_url = env::var(ENV_FAKE_WALLET_SQUARE_DATABASE_URL).unwrap_or_default();

            self.square = Some(SquareConfig {
                api_token,
                environment,
                webhook_enabled,
                database_url,
            });
        }

        self
    }
}
