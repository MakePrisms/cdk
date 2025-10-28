//! Configuration types for Square integration

use serde::{Deserialize, Serialize};

/// Square configuration for payment backends
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SquareConfig {
    /// Square API token (for webhook management)
    pub api_token: String,
    /// Square environment (SANDBOX or PRODUCTION)
    pub environment: String,
    /// PostgreSQL database URL for OAuth credentials
    /// Format: postgresql://user:password@host:port/database?sslmode=require
    pub database_url: String,
    /// Enable webhook mode (if false, uses polling mode even if webhook_url is provided)
    /// Default is true
    #[serde(default = "default_webhook_enabled")]
    pub webhook_enabled: bool,
    /// Payment expiry time in seconds (how far back to sync payments)
    /// Default is 300 seconds (5 minutes)
    #[serde(default = "default_payment_expiry")]
    pub payment_expiry: u64,
}

fn default_webhook_enabled() -> bool {
    true
}

fn default_payment_expiry() -> u64 {
    300 // 5 minutes in seconds
}
