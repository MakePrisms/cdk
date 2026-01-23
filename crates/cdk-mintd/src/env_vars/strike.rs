//! Strike environment variables

use std::env;

use cdk::nuts::CurrencyUnit;

use crate::config::Strike;

pub const ENV_STRIKE_API_KEY: &str = "CDK_MINTD_STRIKE_API_KEY";
pub const ENV_STRIKE_SUPPORTED_UNITS: &str = "CDK_MINTD_STRIKE_SUPPORTED_UNITS";
pub const ENV_STRIKE_WEBHOOK_URL: &str = "CDK_MINTD_STRIKE_WEBHOOK_URL";

impl Strike {
    pub fn from_env(mut self) -> Self {
        if let Ok(api_key) = env::var(ENV_STRIKE_API_KEY) {
            self.api_key = api_key;
        }

        if let Ok(units_str) = env::var(ENV_STRIKE_SUPPORTED_UNITS) {
            let units: Vec<CurrencyUnit> = units_str
                .split(',')
                .filter_map(|unit| unit.trim().parse().ok())
                .collect();
            if !units.is_empty() {
                self.supported_units = units;
            }
        }

        if let Ok(webhook_url) = env::var(ENV_STRIKE_WEBHOOK_URL) {
            self.webhook_url = Some(webhook_url);
        }

        self
    }
}
