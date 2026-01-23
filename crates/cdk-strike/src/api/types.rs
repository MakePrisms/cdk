//! Strike API type definitions
//!
//! This module contains request and response types for the Strike API v1.
//! See <https://docs.strike.me/api/> for the complete API reference.
//!
//! # Type Definitions
//!
//! All types use `camelCase` serialization to match the API's JSON format.
//!
//! ## Amount Handling
//!
//! Strike API returns amounts as strings (e.g., `"0.00001234"` for BTC).
//! The [`Amount`] type handles this with custom deserialization via
//! [`parse_f64_from_string`].
//!
//! ## Invoice States
//!
//! [`InvoiceState`] covers both invoice and payment states:
//! - Invoice states: `UNPAID`, `PENDING`, `PAID`, `CANCELLED`
//! - Payment states: `PENDING`, `COMPLETED`, `FAILED`
//!
//! ## Currency Exchange States
//!
//! [`ExchangeQuoteState`]: `NEW`, `PENDING`, `COMPLETED`, `FAILED`
//!
//! ## Supported Currencies
//!
//! `BTC`, `USD`, `EUR`, `USDT`, `GBP`, `AUD`
//!
//! ## OData Query Support
//!
//! [`InvoiceQueryParams`] provides builder-style construction of OData queries
//! for filtering and paginating invoice lists. Supports `$filter`, `$orderby`,
//! `$skip`, and `$top` parameters.

use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

/// Parse f64 from a string (Strike API returns amounts as strings)
pub fn parse_f64_from_string<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrFloat {
        String(String),
        Float(f64),
    }

    match StringOrFloat::deserialize(deserializer)? {
        StringOrFloat::String(s) => s.parse().map_err(serde::de::Error::custom),
        StringOrFloat::Float(f) => Ok(f),
    }
}

/// Supported currencies in the Strike API
///
/// See <https://docs.strike.me/api/>
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    /// US Dollar - ideal for e-commerce transactions
    USD,
    /// Euro
    EUR,
    /// Bitcoin - used for Lightning Network operations
    BTC,
    /// Tether USD (stablecoin)
    USDT,
    /// British Pound
    GBP,
    /// Australian Dollar
    AUD,
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Currency::USD => write!(f, "USD"),
            Currency::EUR => write!(f, "EUR"),
            Currency::BTC => write!(f, "BTC"),
            Currency::USDT => write!(f, "USDT"),
            Currency::GBP => write!(f, "GBP"),
            Currency::AUD => write!(f, "AUD"),
        }
    }
}

impl std::str::FromStr for Currency {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "USD" => Ok(Currency::USD),
            "EUR" => Ok(Currency::EUR),
            "BTC" => Ok(Currency::BTC),
            "USDT" => Ok(Currency::USDT),
            "GBP" => Ok(Currency::GBP),
            "AUD" => Ok(Currency::AUD),
            // Also support lowercase unit names
            "SAT" | "SATS" => Ok(Currency::BTC),
            "MSAT" | "MSATS" => Ok(Currency::BTC),
            _ => Err(format!("Unknown currency: {}", s)),
        }
    }
}

/// Amount with currency
///
/// Strike API returns amounts as decimal strings (e.g., "0.00001234" for BTC).
/// This type handles both string and float deserialization.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Amount {
    /// The currency type (BTC, USD, EUR, USDT, GBP, AUD)
    pub currency: Currency,
    /// The amount value as a decimal
    #[serde(deserialize_with = "parse_f64_from_string")]
    pub amount: f64,
}

impl Amount {
    /// Create an amount from satoshis
    pub fn from_sats(sats: u64) -> Self {
        Self {
            currency: Currency::BTC,
            amount: sats as f64 / 100_000_000.0,
        }
    }

    /// Create an amount from millisatoshis
    pub fn from_msats(msats: u64) -> Self {
        Self {
            currency: Currency::BTC,
            amount: msats as f64 / 100_000_000_000.0,
        }
    }

    /// Convert to satoshis (only valid for BTC)
    pub fn to_sats(&self) -> anyhow::Result<u64> {
        if self.currency != Currency::BTC {
            anyhow::bail!("Cannot convert {} to sats", self.currency);
        }
        Ok((self.amount * 100_000_000.0).round() as u64)
    }

    /// Convert to millisatoshis (only valid for BTC)
    pub fn to_msats(&self) -> anyhow::Result<u64> {
        if self.currency != Currency::BTC {
            anyhow::bail!("Cannot convert {} to msats", self.currency);
        }
        Ok((self.amount * 100_000_000_000.0).round() as u64)
    }

    /// Create an amount in USD
    pub fn usd(amount: f64) -> Self {
        Self {
            currency: Currency::USD,
            amount,
        }
    }

    /// Create an amount in EUR
    pub fn eur(amount: f64) -> Self {
        Self {
            currency: Currency::EUR,
            amount,
        }
    }
}

/// Fee policy for payments
///
/// Determines how fees are handled relative to the payment amount.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Deserialize, Serialize)]
pub enum FeePolicy {
    /// Fee is included in the amount - reduces the amount sent to recipient
    Inclusive,
    /// Fee is added on top of the amount - recipient receives full amount (default)
    Exclusive,
}

/// Payment amount with optional fee policy
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentAmount {
    /// The amount value
    #[serde(deserialize_with = "parse_f64_from_string")]
    pub amount: f64,
    /// The currency type
    pub currency: Currency,
    /// Optional fee policy
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_policy: Option<FeePolicy>,
}

/// Invoice and payment state
///
/// This enum covers states for both invoices and payments:
///
/// **Invoice states:**
/// - `UNPAID` - Invoice created, awaiting payment
/// - `PENDING` - Payment in progress
/// - `PAID` - Payment received successfully
/// - `CANCELLED` - Invoice was cancelled
///
/// **Payment states:**
/// - `PENDING` - Payment in progress, awaiting blockchain confirmation
/// - `COMPLETED` - Payment completed successfully
/// - `FAILED` - Payment failed
///
/// See <https://docs.strike.me/walkthrough/receiving-payments/>
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InvoiceState {
    /// Payment completed successfully (payment state)
    Completed,
    /// Invoice/payment has been paid (invoice state)
    Paid,
    /// Invoice is unpaid, awaiting payment (invoice state)
    Unpaid,
    /// Payment in progress, awaiting confirmation
    Pending,
    /// Payment failed (payment state)
    Failed,
    /// Invoice was cancelled (invoice state)
    Cancelled,
}

impl fmt::Display for InvoiceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvoiceState::Completed => write!(f, "COMPLETED"),
            InvoiceState::Paid => write!(f, "PAID"),
            InvoiceState::Unpaid => write!(f, "UNPAID"),
            InvoiceState::Pending => write!(f, "PENDING"),
            InvoiceState::Failed => write!(f, "FAILED"),
            InvoiceState::Cancelled => write!(f, "CANCELLED"),
        }
    }
}

/// Request to create an invoice
///
/// Invoices are used to receive payments for a specific amount. They begin in the
/// UNPAID state and transition to PAID upon successful payment delivery.
///
/// See <https://docs.strike.me/api/issue-invoice/>
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceRequest {
    /// Universally unique identifier for invoice tracking.
    /// Use this to correlate invoices with your internal systems.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Optional invoice description that will be included in the Lightning invoice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The invoice amount with currency. USD invoices are ideal for e-commerce
    /// since the received amount is guaranteed regardless of BTC price fluctuations.
    pub amount: Amount,
}

/// Response from creating an invoice
///
/// See <https://docs.strike.me/api/issue-invoice/>
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceResponse {
    /// Unique invoice identifier (UUID)
    pub invoice_id: String,
    /// The invoice amount with currency
    pub amount: Amount,
    /// Current invoice state: UNPAID, PENDING, PAID, or CANCELLED
    pub state: InvoiceState,
    /// Creation timestamp (ISO 8601)
    pub created: String,
    /// Universally unique identifier for invoice tracking, provided in the create request
    #[serde(default)]
    pub correlation_id: Option<String>,
    /// Optional invoice description
    #[serde(default)]
    pub description: Option<String>,
    /// ID of the invoice issuer (UUID)
    pub issuer_id: String,
    /// ID of the payment receiver (UUID)
    pub receiver_id: String,
}

/// Request for an invoice quote
///
/// See <https://docs.strike.me/api/issue-quote-for-invoice/>
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceQuoteRequest {
    /// Optional description hash for BOLT11 spec compliance.
    /// When provided, the resulting Lightning invoice will include this hash
    /// instead of the plain text description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_hash: Option<String>,
}

/// Response from getting an invoice quote (includes BOLT11)
///
/// After creating an invoice, generate a quote to get the BOLT11 Lightning invoice.
/// Quotes have an expiration time: 30 seconds for cross-currency, 3600 seconds (1 hour)
/// for same-currency invoices.
///
/// See <https://docs.strike.me/api/issue-quote-for-invoice/>
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceQuoteResponse {
    /// Unique quote identifier (UUID)
    pub quote_id: String,
    /// Optional description forwarded from the invoice
    #[serde(default)]
    pub description: Option<String>,
    /// BOLT11 Lightning invoice string. This alphanumeric code contains the
    /// payment amount and destination, and can be presented as a QR code.
    pub ln_invoice: String,
    /// Optional on-chain Bitcoin address for fallback payment
    #[serde(default)]
    pub onchain_address: Option<String>,
    /// Quote expiration timestamp (ISO 8601)
    pub expiration: String,
    /// Seconds until quote expiration. 30 sec for cross-currency, 3600 sec for same-currency.
    pub expiration_in_sec: u64,
    /// Source amount - what the payer sends in BTC
    pub source_amount: Amount,
    /// Target amount - what the receiver gets in their chosen currency
    pub target_amount: Amount,
    /// Currency conversion rate. Value is 1 for BTC-to-BTC invoices.
    pub conversion_rate: ConversionRate,
}

/// Invoice list item
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceListItem {
    /// Unique invoice identifier
    pub invoice_id: String,
    /// The invoice amount
    pub amount: Amount,
    /// Current invoice state
    pub state: InvoiceState,
    /// Creation timestamp (ISO 8601)
    pub created: String,
    /// Optional correlation ID for tracking
    #[serde(default)]
    pub correlation_id: Option<String>,
    /// Optional invoice description
    #[serde(default)]
    pub description: Option<String>,
    /// ID of the invoice issuer
    pub issuer_id: String,
    /// ID of the payment receiver
    pub receiver_id: String,
    /// ID of the payer (if paid)
    #[serde(default)]
    pub payer_id: Option<String>,
}

/// Response from listing invoices
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct InvoiceListResponse {
    /// List of invoices
    pub items: Vec<InvoiceListItem>,
    /// Total count of matching invoices
    pub count: i64,
}

/// Conversion rate between currencies
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionRate {
    /// The conversion rate value
    #[serde(deserialize_with = "parse_f64_from_string")]
    pub amount: f64,
    /// Source currency
    pub source_currency: Currency,
    /// Target currency
    pub target_currency: Currency,
}

/// Request to get a Lightning payment quote
///
/// See <https://docs.strike.me/api/create-lightning-payment-quote/>
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PayInvoiceQuoteRequest {
    /// BOLT11 Lightning invoice to pay (min length: 1)
    pub ln_invoice: String,
    /// Currency to send from. Defaults to the user's default currency if not specified.
    pub source_currency: Currency,
    /// Amount to pay. Required only for zero-amount invoices; omit otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<PaymentAmount>,
}

/// Response from getting a Lightning payment quote
///
/// Contains the cost breakdown for paying a Lightning invoice, including
/// the amount, fees, and conversion rate for cross-currency payments.
///
/// See <https://docs.strike.me/api/create-lightning-payment-quote/>
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PayInvoiceQuoteResponse {
    /// Unique payment quote identifier (UUID)
    pub payment_quote_id: String,
    /// Optional description forwarded from the Lightning invoice
    #[serde(default)]
    pub description: Option<String>,
    /// Quote expiration timestamp (ISO 8601). Execute before this time.
    #[serde(default)]
    pub valid_until: Option<String>,
    /// Currency conversion rate for cross-currency payments.
    /// Shows how much of source currency equals 1 unit of target currency (BTC).
    #[serde(default)]
    pub conversion_rate: Option<ConversionRate>,
    /// The payment amount in the source currency
    pub amount: Amount,
    /// Lightning Network routing fee charged by the network
    #[serde(default)]
    pub lightning_network_fee: Option<Amount>,
    /// Total fee including all applicable fees
    #[serde(default)]
    pub total_fee: Option<Amount>,
    /// Total amount the sender will spend (amount + all fees)
    pub total_amount: Amount,
    /// Optional reward amount (e.g., cashback)
    #[serde(default)]
    pub reward: Option<Amount>,
}

/// Response from executing a payment
///
/// Returned after executing a payment quote. The payment state indicates
/// whether the payment is PENDING (awaiting blockchain confirmation) or
/// COMPLETED (successfully delivered).
///
/// See <https://docs.strike.me/api/execute-payment-quote/>
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoicePaymentResponse {
    /// Unique payment identifier (UUID)
    pub payment_id: String,
    /// Current payment state: PENDING or COMPLETED
    pub state: InvoiceState,
    /// Completion timestamp (ISO 8601), present when state is COMPLETED
    #[serde(default)]
    pub completed: Option<String>,
    /// Currency conversion rate used for cross-currency payments
    #[serde(default)]
    pub conversion_rate: Option<ConversionRate>,
    /// The payment amount in the source currency
    pub amount: Amount,
    /// Total fee charged for the payment
    #[serde(default)]
    pub total_fee: Option<Amount>,
    /// Lightning Network routing fee
    #[serde(default)]
    pub lightning_network_fee: Option<Amount>,
    /// Total amount spent (amount + all fees)
    pub total_amount: Amount,
    /// Optional reward earned (e.g., cashback)
    #[serde(default)]
    pub reward: Option<Amount>,
    /// Lightning-specific payment details
    #[serde(default)]
    pub lightning: Option<LightningPaymentDetails>,
    /// On-chain payment details (for on-chain payments)
    #[serde(default)]
    pub onchain: Option<OnchainPaymentDetails>,
}

/// Lightning-specific payment details
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LightningPaymentDetails {
    /// Optional network fee
    #[serde(default)]
    pub network_fee: Option<Amount>,
}

/// On-chain payment details
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnchainPaymentDetails {
    /// Optional transaction ID
    #[serde(default)]
    pub txn_id: Option<String>,
}

/// Currency exchange quote state
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Deserialize, Serialize)]
pub enum ExchangeQuoteState {
    /// Quote newly created
    New,
    /// Exchange is pending
    Pending,
    /// Exchange completed
    Completed,
    /// Exchange failed
    Failed,
}

/// Request for a currency exchange quote
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct CurrencyExchangeQuoteRequest {
    /// Currency to sell
    pub sell: Currency,
    /// Currency to buy
    pub buy: Currency,
    /// Amount to exchange
    pub amount: ExchangeAmount,
}

/// Amount for currency exchange
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExchangeAmount {
    /// Amount as string
    pub amount: String,
    /// Currency type
    pub currency: Currency,
    /// Optional fee policy
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_policy: Option<FeePolicy>,
}

/// Response from creating a currency exchange quote
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrencyExchangeQuoteResponse {
    /// Unique quote identifier
    pub id: String,
    /// Creation timestamp (ISO 8601)
    pub created: String,
    /// Quote expiration timestamp (ISO 8601)
    pub valid_until: String,
    /// Source amount
    pub source: Amount,
    /// Target amount
    pub target: Amount,
    /// Optional fee amount
    #[serde(default)]
    pub fee: Option<Amount>,
    /// Conversion rate
    pub conversion_rate: ConversionRate,
    /// Current quote state
    pub state: ExchangeQuoteState,
    /// Completion timestamp (ISO 8601)
    #[serde(default)]
    pub completed: Option<String>,
}

/// Filter operation for OData queries
#[derive(Debug, Clone, Copy)]
pub enum FilterOp {
    /// Equal
    Eq,
    /// Not equal
    Ne,
    /// Greater than
    Gt,
    /// Less than
    Lt,
    /// Greater than or equal
    Ge,
    /// Less than or equal
    Le,
}

impl fmt::Display for FilterOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterOp::Eq => write!(f, "eq"),
            FilterOp::Ne => write!(f, "ne"),
            FilterOp::Gt => write!(f, "gt"),
            FilterOp::Lt => write!(f, "lt"),
            FilterOp::Ge => write!(f, "ge"),
            FilterOp::Le => write!(f, "le"),
        }
    }
}

/// Filter for OData queries
#[derive(Debug, Clone)]
pub struct Filter {
    /// Field name to filter on
    pub field: &'static str,
    /// Filter operation
    pub op: FilterOp,
    /// Filter value
    pub value: String,
}

impl Filter {
    /// Create an equality filter
    pub fn eq(field: &'static str, value: impl ToString) -> Self {
        Self {
            field,
            op: FilterOp::Eq,
            value: value.to_string(),
        }
    }

    /// Create a not-equal filter
    pub fn ne(field: &'static str, value: impl ToString) -> Self {
        Self {
            field,
            op: FilterOp::Ne,
            value: value.to_string(),
        }
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} '{}'", self.field, self.op, self.value)
    }
}

/// Query parameters for listing invoices
#[derive(Clone, Debug, Default)]
pub struct InvoiceQueryParams {
    /// List of filters to apply
    pub filters: Vec<Filter>,
    /// Optional sort order
    pub orderby: Option<String>,
    /// Number of results to skip
    pub skip: Option<i32>,
    /// Maximum number of results
    pub top: Option<i32>,
}

impl InvoiceQueryParams {
    /// Create new empty query params
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a filter
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Set the sort order
    pub fn orderby(mut self, orderby: impl Into<String>) -> Self {
        self.orderby = Some(orderby.into());
        self
    }

    /// Set the number of results to skip
    pub fn skip(mut self, skip: i32) -> Self {
        self.skip = Some(skip);
        self
    }

    /// Set the maximum number of results (max 100)
    pub fn top(mut self, top: i32) -> Self {
        self.top = Some(top.min(100)); // Max 100
        self
    }

    /// Convert to OData query string
    pub fn to_query_string(&self) -> String {
        let mut parts = Vec::new();

        if !self.filters.is_empty() {
            let filter_str = self
                .filters
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(" and ");
            parts.push(format!("$filter={}", urlencoding::encode(&filter_str)));
        }

        if let Some(ref orderby) = self.orderby {
            parts.push(format!("$orderby={}", urlencoding::encode(orderby)));
        }

        if let Some(skip) = self.skip {
            parts.push(format!("$skip={}", skip));
        }

        if let Some(top) = self.top {
            parts.push(format!("$top={}", top));
        }

        if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_amount_from_sats() {
        let amount = Amount::from_sats(100_000_000);
        assert_eq!(amount.to_sats().unwrap(), 100_000_000);
    }

    #[test]
    fn test_query_params() {
        let params = InvoiceQueryParams::new()
            .filter(Filter::eq("state", "PAID"))
            .top(10);

        let qs = params.to_query_string();
        assert!(qs.contains("$filter="));
        assert!(qs.contains("$top=10"));
    }
}
