//! Agicash environment variables

use std::env;

use crate::config::{Agicash, ClosedLoop, ClosedLoopType};

// Agicash closed loop environment variables
pub const ENV_AGICASH_CLOSED_LOOP_TYPE: &str = "CDK_MINTD_AGICASH_CLOSED_LOOP_TYPE";
pub const ENV_AGICASH_CLOSED_LOOP_VALID_DESTINATION: &str =
    "CDK_MINTD_AGICASH_CLOSED_LOOP_VALID_DESTINATION";

impl Agicash {
    pub fn from_env(mut self) -> Self {
        // Check if any closed loop env vars are set
        let has_closed_loop_env = env::var(ENV_AGICASH_CLOSED_LOOP_TYPE).is_ok()
            || env::var(ENV_AGICASH_CLOSED_LOOP_VALID_DESTINATION).is_ok();

        if has_closed_loop_env {
            let mut closed_loop = self.closed_loop.unwrap_or_else(|| ClosedLoop {
                closed_loop_type: ClosedLoopType::Internal,
                valid_destination_name: String::new(),
            });

            if let Ok(loop_type_str) = env::var(ENV_AGICASH_CLOSED_LOOP_TYPE) {
                if let Ok(loop_type) = loop_type_str.parse() {
                    closed_loop.closed_loop_type = loop_type;
                }
            }

            if let Ok(valid_dest) = env::var(ENV_AGICASH_CLOSED_LOOP_VALID_DESTINATION) {
                closed_loop.valid_destination_name = valid_dest;
            }

            self.closed_loop = Some(closed_loop);
        }

        self
    }
}
