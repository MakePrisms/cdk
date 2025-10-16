//! NWC environment variables

use std::env;

use crate::config::Nwc;
#[cfg(feature = "square")]
use crate::config::SquareConfig;

// NWC environment variables
pub const ENV_NWC_URI: &str = "CDK_MINTD_NWC_URI";
pub const ENV_NWC_FEE_PERCENT: &str = "CDK_MINTD_NWC_FEE_PERCENT";
pub const ENV_NWC_RESERVE_FEE_MIN: &str = "CDK_MINTD_NWC_RESERVE_FEE_MIN";

// Square environment variables (optional integration with NWC)
#[cfg(feature = "square")]
pub const ENV_SQUARE_API_TOKEN: &str = "CDK_MINTD_SQUARE_API_TOKEN";
#[cfg(feature = "square")]
pub const ENV_SQUARE_ENVIRONMENT: &str = "CDK_MINTD_SQUARE_ENVIRONMENT";
#[cfg(feature = "square")]
pub const ENV_SQUARE_WEBHOOK_ENABLED: &str = "CDK_MINTD_SQUARE_WEBHOOK_ENABLED";

impl Nwc {
    pub fn from_env(mut self) -> Self {
        if let Ok(nwc_uri) = env::var(ENV_NWC_URI) {
            self.nwc_uri = nwc_uri;
        }

        if let Ok(fee_str) = env::var(ENV_NWC_FEE_PERCENT) {
            if let Ok(fee) = fee_str.parse() {
                self.fee_percent = fee;
            }
        }

        if let Ok(reserve_fee_str) = env::var(ENV_NWC_RESERVE_FEE_MIN) {
            if let Ok(reserve_fee) = reserve_fee_str.parse::<u64>() {
                self.reserve_fee_min = reserve_fee.into();
            }
        }

        // Square integration (optional)
        #[cfg(feature = "square")]
        if let Ok(api_token) = env::var(ENV_SQUARE_API_TOKEN) {
            let environment =
                env::var(ENV_SQUARE_ENVIRONMENT).unwrap_or_else(|_| "SANDBOX".to_string());

            let webhook_enabled = env::var(ENV_SQUARE_WEBHOOK_ENABLED)
                .ok()
                .and_then(|v| v.parse::<bool>().ok())
                .unwrap_or(true);

            self.square = Some(SquareConfig {
                api_token,
                environment,
                webhook_enabled,
            });
        }

        self
    }
}
