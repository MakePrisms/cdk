//! Square API types for extended square client functionality

use std::convert::TryFrom;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Payment data stored in KV store
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentData {
    /// Timestamp when the payment expires (Unix seconds)
    pub expires_at: u64,
    /// Square payment ID
    pub payment_id: String,
}

impl PaymentData {
    /// Create a new payment data entry
    pub fn new(expires_at: u64, payment_id: String) -> Self {
        Self {
            expires_at,
            payment_id,
        }
    }

    /// Check if the payment is currently expired
    pub fn is_expired(&self) -> bool {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        current_time >= self.expires_at
    }
}

impl From<PaymentData> for Vec<u8> {
    /// Encode payment data to bytes
    /// Format: 8 bytes expires_at timestamp + variable length payment_id (UTF-8)
    fn from(payment_data: PaymentData) -> Self {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&payment_data.expires_at.to_le_bytes());
        bytes.extend_from_slice(payment_data.payment_id.as_bytes());
        bytes
    }
}

impl TryFrom<&[u8]> for PaymentData {
    type Error = ();

    /// Decode payment data from bytes
    fn try_from(data: &[u8]) -> Result<Self, Self::Error> {
        if data.len() < 8 {
            return Err(());
        }
        let expires_at = u64::from_le_bytes(data[0..8].try_into().map_err(|_| ())?);
        let payment_id = String::from_utf8(data[8..].to_vec()).map_err(|_| ())?;
        Ok(Self::new(expires_at, payment_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_payment_data_creation() {
        let data = PaymentData::new(4600, "payment_123".to_string());
        assert_eq!(data.expires_at, 4600);
        assert_eq!(data.payment_id, "payment_123");
    }

    #[test]
    fn test_payment_data_expiry_check() {
        // Create data that expires in the future
        let future_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        let data = PaymentData::new(future_time, "payment_123".to_string());
        assert!(!data.is_expired());

        // Create data that's already expired
        let past_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 100;
        let expired_data = PaymentData::new(past_time, "payment_456".to_string());
        assert!(expired_data.is_expired());
    }

    #[test]
    fn test_payment_data_serialization() {
        let data = PaymentData::new(1234567890, "test_payment_id_789".to_string());
        let bytes: Vec<u8> = data.clone().into();

        // Should be 8 bytes (expires_at) + length of payment_id
        assert_eq!(bytes.len(), 8 + "test_payment_id_789".len());

        let decoded = PaymentData::try_from(bytes.as_slice()).unwrap();
        assert_eq!(decoded, data);
        assert_eq!(decoded.expires_at, 1234567890);
        assert_eq!(decoded.payment_id, "test_payment_id_789");
    }

    #[test]
    fn test_payment_data_invalid_bytes() {
        // Too short
        assert!(PaymentData::try_from(&[1, 2, 3][..]).is_err());

        // Empty
        assert!(PaymentData::try_from(&[][..]).is_err());

        // Valid length (8 bytes for expires_at + payment_id)
        let mut valid_bytes = vec![0u8; 8];
        valid_bytes.extend_from_slice(b"payment123");
        let data = PaymentData::try_from(valid_bytes.as_slice());
        assert!(data.is_ok());
    }

    #[test]
    fn test_payment_data_with_special_characters() {
        let payment_id = "payment_äöü_🎉_123".to_string();
        let data = PaymentData::new(9999999999, payment_id.clone());
        let bytes: Vec<u8> = data.clone().into();

        let decoded = PaymentData::try_from(bytes.as_slice()).unwrap();
        assert_eq!(decoded.expires_at, 9999999999);
        assert_eq!(decoded.payment_id, payment_id);
    }
}
