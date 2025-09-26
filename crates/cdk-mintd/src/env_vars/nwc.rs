//! NWC environment variables

use std::env;

use crate::config::Nwc;

// NWC environment variables
pub const ENV_NWC_URI: &str = "CDK_MINTD_NWC_URI";
pub const ENV_NWC_FEE_PERCENT: &str = "CDK_MINTD_NWC_FEE_PERCENT";
pub const ENV_NWC_RESERVE_FEE_MIN: &str = "CDK_MINTD_NWC_RESERVE_FEE_MIN";

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

        self
    }
}
