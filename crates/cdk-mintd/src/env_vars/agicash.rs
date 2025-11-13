//! Agicash environment variables

use std::env;

use crate::config::{Agicash, ClosedLoop, ClosedLoopType};

// Agicash closed loop environment variables
pub const ENV_AGICASH_CLOSED_LOOP_TYPE: &str = "CDK_MINTD_AGICASH_CLOSED_LOOP_TYPE";
pub const ENV_AGICASH_CLOSED_LOOP_VALID_DESTINATION: &str =
    "CDK_MINTD_AGICASH_CLOSED_LOOP_VALID_DESTINATION";

// Square configuration environment variables
pub const ENV_AGICASH_CLOSED_LOOP_SQUARE_API_TOKEN: &str =
    "CDK_MINTD_AGICASH_CLOSED_LOOP_SQUARE_API_TOKEN";
pub const ENV_AGICASH_CLOSED_LOOP_SQUARE_ENVIRONMENT: &str =
    "CDK_MINTD_AGICASH_CLOSED_LOOP_SQUARE_ENVIRONMENT";
pub const ENV_AGICASH_CLOSED_LOOP_SQUARE_DATABASE_URL: &str =
    "CDK_MINTD_AGICASH_CLOSED_LOOP_SQUARE_DATABASE_URL";
pub const ENV_AGICASH_CLOSED_LOOP_SQUARE_WEBHOOK_ENABLED: &str =
    "CDK_MINTD_AGICASH_CLOSED_LOOP_SQUARE_WEBHOOK_ENABLED";
pub const ENV_AGICASH_CLOSED_LOOP_SQUARE_PAYMENT_EXPIRY: &str =
    "CDK_MINTD_AGICASH_CLOSED_LOOP_SQUARE_PAYMENT_EXPIRY";

impl Agicash {
    pub fn from_env(mut self) -> Self {
        // Check if any closed loop env vars are set
        let has_closed_loop_env = env::var(ENV_AGICASH_CLOSED_LOOP_TYPE).is_ok()
            || env::var(ENV_AGICASH_CLOSED_LOOP_VALID_DESTINATION).is_ok()
            || env::var(ENV_AGICASH_CLOSED_LOOP_SQUARE_API_TOKEN).is_ok()
            || env::var(ENV_AGICASH_CLOSED_LOOP_SQUARE_ENVIRONMENT).is_ok()
            || env::var(ENV_AGICASH_CLOSED_LOOP_SQUARE_DATABASE_URL).is_ok();

        if has_closed_loop_env {
            let mut closed_loop = self.closed_loop.unwrap_or_else(|| ClosedLoop {
                closed_loop_type: ClosedLoopType::Internal,
                valid_destination_name: String::new(),
                square: None,
            });

            if let Ok(loop_type_str) = env::var(ENV_AGICASH_CLOSED_LOOP_TYPE) {
                if let Ok(loop_type) = loop_type_str.parse() {
                    closed_loop.closed_loop_type = loop_type;
                }
            }

            if let Ok(valid_dest) = env::var(ENV_AGICASH_CLOSED_LOOP_VALID_DESTINATION) {
                closed_loop.valid_destination_name = valid_dest;
            }

            // Parse Square configuration from environment variables
            let has_square_env = env::var(ENV_AGICASH_CLOSED_LOOP_SQUARE_API_TOKEN).is_ok()
                || env::var(ENV_AGICASH_CLOSED_LOOP_SQUARE_ENVIRONMENT).is_ok()
                || env::var(ENV_AGICASH_CLOSED_LOOP_SQUARE_DATABASE_URL).is_ok();

            if has_square_env {
                let mut square_config =
                    closed_loop
                        .square
                        .unwrap_or_else(|| cdk_agicash::square::SquareConfig {
                            api_token: String::new(),
                            environment: "SANDBOX".to_string(),
                            database_url: String::new(),
                            webhook_enabled: true,
                            payment_expiry: 300,
                        });

                if let Ok(api_token) = env::var(ENV_AGICASH_CLOSED_LOOP_SQUARE_API_TOKEN) {
                    square_config.api_token = api_token;
                }

                if let Ok(environment) = env::var(ENV_AGICASH_CLOSED_LOOP_SQUARE_ENVIRONMENT) {
                    square_config.environment = environment;
                }

                if let Ok(database_url) = env::var(ENV_AGICASH_CLOSED_LOOP_SQUARE_DATABASE_URL) {
                    square_config.database_url = database_url;
                }

                if let Ok(webhook_enabled_str) =
                    env::var(ENV_AGICASH_CLOSED_LOOP_SQUARE_WEBHOOK_ENABLED)
                {
                    square_config.webhook_enabled =
                        webhook_enabled_str.parse::<bool>().unwrap_or(true);
                }

                if let Ok(payment_expiry_str) =
                    env::var(ENV_AGICASH_CLOSED_LOOP_SQUARE_PAYMENT_EXPIRY)
                {
                    if let Ok(payment_expiry) = payment_expiry_str.parse::<u64>() {
                        square_config.payment_expiry = payment_expiry;
                    }
                }

                closed_loop.square = Some(square_config);
            }

            self.closed_loop = Some(closed_loop);
        }

        self
    }
}
