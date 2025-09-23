//! Strike environment variables

use std::env;

use cdk::nuts::CurrencyUnit;

use crate::config::Strike;

// Strike environment variables
pub const ENV_STRIKE_API_KEY: &str = "CDK_MINTD_STRIKE_API_KEY";
pub const ENV_STRIKE_SUPPORTED_UNITS: &str = "CDK_MINTD_STRIKE_SUPPORTED_UNITS";

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

        self
    }
}
