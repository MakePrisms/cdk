//! Square API types for extended square client functionality

use serde::{Deserialize, Serialize};

/// Payment brand filter
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentBrand {
    /// No brand filtering - return all payments
    All,
    /// Filter for LIGHTNING payments only
    Lightning,
}

impl PaymentBrand {
    /// Check if a payment matches this brand filter
    pub fn matches(&self, brand: Option<&str>) -> bool {
        match self {
            PaymentBrand::All => true,
            PaymentBrand::Lightning => brand == Some("LIGHTNING"),
        }
    }
}

/// Response from Square List Payments API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListPaymentsResponse {
    /// Array of payment objects
    #[serde(default)]
    pub payments: Vec<Payment>,
    /// Cursor for pagination (if more results available)
    pub cursor: Option<String>,
}

/// Square payment object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payment {
    /// Unique identifier for the payment
    pub id: String,
    /// RFC 3339 timestamp of when the payment was created
    pub created_at: String,
    /// RFC 3339 timestamp of when the payment was last updated
    pub updated_at: String,
    /// Money amount for the payment
    pub amount_money: Option<Money>,
    /// Current status of the payment (e.g., "COMPLETED", "FAILED", "PENDING", "CANCELED", "APPROVED")
    pub status: String,
    /// Source type of the payment (e.g., "WALLET")
    pub source_type: Option<String>,
    /// Wallet-specific payment details (contains Lightning info)
    pub wallet_details: Option<WalletDetails>,
}

/// Money amount representation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Money {
    /// Amount in the smallest currency unit (e.g., cents for USD)
    pub amount: i64,
    /// 3-letter ISO 4217 currency code (e.g., "USD", "BTC")
    pub currency: String,
}

/// Wallet details for a payment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletDetails {
    /// Payment brand (e.g., "LIGHTNING")
    pub brand: Option<String>,
    /// Lightning-specific payment details
    pub lightning_details: Option<LightningDetails>,
}

/// Lightning payment details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightningDetails {
    /// Lightning payment URL (lightning:bolt11...)
    pub payment_url: String,
    /// RFC 3339 timestamp of when the invoice expires
    pub expires_at: Option<String>,
    /// Payment amount in millisatoshis (as string to avoid precision loss)
    pub amount_milli_sats: Option<String>,
}

/// Parameters for listing payments
#[derive(Debug, Clone)]
pub struct ListPaymentsParams {
    /// RFC 3339 timestamp to filter payments from
    pub begin_time: Option<String>,
    /// Cursor for pagination
    pub cursor: Option<String>,
    /// Maximum number of results per page (default: 100)
    pub limit: u32,
    /// Payment brand filter (client-side filtering)
    pub brand: PaymentBrand,
}

impl Default for ListPaymentsParams {
    fn default() -> Self {
        Self::new()
    }
}

impl ListPaymentsParams {
    /// Create new params with default limit of 100 and no brand filtering
    pub fn new() -> Self {
        Self {
            begin_time: None,
            cursor: None,
            limit: 100,
            brand: PaymentBrand::All,
        }
    }

    /// Set begin_time filter
    pub fn with_begin_time(mut self, begin_time: String) -> Self {
        self.begin_time = Some(begin_time);
        self
    }

    /// Set cursor for pagination
    pub fn with_cursor(mut self, cursor: String) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// Set limit for results per page
    pub fn with_limit(mut self, limit: u32) -> Self {
        self.limit = limit;
        self
    }

    /// Set payment brand filter
    pub fn with_brand(mut self, brand: PaymentBrand) -> Self {
        self.brand = brand;
        self
    }
}

/// Response from Square List Merchants API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListMerchantsResponse {
    /// Array of merchant objects
    #[serde(default)]
    pub merchant: Vec<Merchant>,
    /// Cursor for pagination (if more results available)
    pub cursor: Option<i32>,
}

/// Square merchant object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Merchant {
    /// Unique identifier for the merchant
    pub id: String,
    /// Business name of the merchant
    pub business_name: Option<String>,
    /// Country code (e.g., "US")
    pub country: String,
    /// Language code (e.g., "en-US")
    pub language_code: Option<String>,
    /// Currency code (e.g., "USD")
    pub currency: Option<String>,
    /// Status of the merchant (e.g., "ACTIVE")
    pub status: Option<String>,
    /// Main location ID for the merchant
    pub main_location_id: Option<String>,
    /// RFC 3339 timestamp of when the merchant was created
    pub created_at: Option<String>,
}
