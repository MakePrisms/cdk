//! Strike API client
//!
//! This module implements the Strike API v1 for Lightning Network payments.
//! See <https://docs.strike.me/api/> for the complete API reference.
//!
//! # Overview
//!
//! Strike enables receiving and sending payments via the Bitcoin Lightning Network.
//! This client supports:
//! - Creating invoices to receive payments
//! - Generating payment quotes and executing payments
//! - Currency exchange between supported currencies
//! - Webhook subscriptions for real-time notifications
//!
//! # Endpoints
//!
//! ## Invoices (Receiving Payments)
//!
//! | Method | Endpoint | Description |
//! |--------|----------|-------------|
//! | POST | `/v1/invoices` | Create an invoice for a specific amount |
//! | GET | `/v1/invoices/{id}` | Get invoice by ID |
//! | GET | `/v1/invoices` | List invoices with OData filtering |
//! | POST | `/v1/invoices/{id}/quote` | Generate BOLT11 Lightning invoice |
//!
//! **Invoice Flow:**
//! 1. Create invoice (state: `UNPAID`)
//! 2. Generate quote to get BOLT11 (expires in 30s cross-currency, 1hr same-currency)
//! 3. Payer pays the BOLT11
//! 4. Invoice transitions to `PAID`
//!
//! ## Payment Quotes (Sending Payments)
//!
//! | Method | Endpoint | Description |
//! |--------|----------|-------------|
//! | POST | `/v1/payment-quotes/lightning` | Quote for paying a BOLT11 invoice |
//! | PATCH | `/v1/payment-quotes/{id}/execute` | Execute the payment |
//! | GET | `/v1/payments/{id}` | Get payment status |
//!
//! **Payment States:** `PENDING` (awaiting confirmation), `COMPLETED`, `FAILED`
//!
//! ## Currency Exchange
//!
//! | Method | Endpoint | Description |
//! |--------|----------|-------------|
//! | POST | `/v1/currency-exchange-quotes` | Create exchange quote |
//! | GET | `/v1/currency-exchange-quotes/{id}` | Get exchange quote |
//! | PATCH | `/v1/currency-exchange-quotes/{id}/execute` | Execute exchange |
//!
//! ## Webhooks
//!
//! | Method | Endpoint | Description |
//! |--------|----------|-------------|
//! | POST | `/v1/subscriptions` | Create webhook subscription |
//! | GET | `/v1/subscriptions` | List subscriptions |
//! | DELETE | `/v1/subscriptions/{id}` | Delete subscription |
//!
//! **Event Types:** `invoice.updated`, `payment.updated`, `currency-exchange-quote.updated`
//!
//! # Authentication
//!
//! All requests require Bearer token authentication via the `Authorization` header.
//! API keys are generated in the Strike Dashboard at <https://dashboard.strike.me>.
//!
//! # Supported Currencies
//!
//! `BTC`, `USD`, `EUR`, `USDT`, `GBP`, `AUD`
//!
//! USD invoices are ideal for e-commerce since the received amount is guaranteed
//! regardless of Bitcoin price fluctuations.

pub mod error;
pub mod types;
pub mod webhook;

use error::{Error, StrikeApiError};
use rand::Rng;
use reqwest::Client;
use serde::Serialize;
use std::time::Duration;
use tracing::{debug, warn};
use types::*;
use url::Url;

/// Strike API client
#[derive(Debug, Clone)]
pub struct StrikeApi {
    api_key: String,
    base_url: Url,
    client: Client,
    webhook_secret: String,
}

impl StrikeApi {
    /// Create a new Strike API client
    pub fn new(api_key: &str, api_url: Option<&str>, timeout_ms: u64) -> anyhow::Result<Self> {
        let base_url = api_url.unwrap_or("https://api.strike.me");
        let base_url = Url::parse(base_url)?;

        let client = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()?;

        // Generate a random webhook secret
        let mut rng = rand::rng();
        let webhook_secret: String = (0..15)
            .map(|_| {
                let idx = rng.random_range(0..62);
                let chars = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
                chars[idx] as char
            })
            .collect();

        Ok(Self {
            api_key: api_key.to_string(),
            base_url,
            client,
            webhook_secret,
        })
    }

    /// Get the webhook secret for signature verification
    pub fn webhook_secret(&self) -> &str {
        &self.webhook_secret
    }

    /// Make a GET request
    async fn get(&self, path: &str) -> Result<serde_json::Value, Error> {
        let url = self.base_url.join(path)?;
        debug!("GET {}", url);

        let response = self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Accept", "application/json")
            .send()
            .await?;

        self.handle_response(response).await
    }

    /// Make a POST request
    async fn post<T: Serialize>(&self, path: &str, body: &T) -> Result<serde_json::Value, Error> {
        let url = self.base_url.join(path)?;
        debug!("POST {}", url);

        let response = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(body)
            .send()
            .await?;

        self.handle_response(response).await
    }

    /// Make a PATCH request (no body)
    async fn patch(&self, path: &str) -> Result<serde_json::Value, Error> {
        let url = self.base_url.join(path)?;
        debug!("PATCH {}", url);

        let response = self
            .client
            .patch(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Accept", "application/json")
            .send()
            .await?;

        self.handle_response(response).await
    }

    /// Make a DELETE request
    async fn delete(&self, path: &str) -> Result<(), Error> {
        let url = self.base_url.join(path)?;
        debug!("DELETE {}", url);

        let response = self
            .client
            .delete(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            let text = response.text().await?;
            let error: StrikeApiError = serde_json::from_str(&text)?;
            Err(Error::Api(error))
        }
    }

    /// Handle API response
    async fn handle_response(
        &self,
        response: reqwest::Response,
    ) -> Result<serde_json::Value, Error> {
        let status = response.status();
        let text = response.text().await?;

        if status.is_success() {
            let json: serde_json::Value = serde_json::from_str(&text)?;
            Ok(json)
        } else if status == reqwest::StatusCode::NOT_FOUND {
            Err(Error::NotFound)
        } else {
            warn!("Strike API error: {} - {}", status, text);
            let error: StrikeApiError = serde_json::from_str(&text)?;
            Err(Error::Api(error))
        }
    }

    // ==================== Invoice Endpoints ====================

    /// Create an invoice
    pub async fn create_invoice(&self, request: InvoiceRequest) -> Result<InvoiceResponse, Error> {
        let json = self.post("/v1/invoices", &request).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Get an invoice by ID
    pub async fn get_incoming_invoice(&self, invoice_id: &str) -> Result<InvoiceResponse, Error> {
        let json = self.get(&format!("/v1/invoices/{}", invoice_id)).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// List invoices with optional query parameters
    pub async fn get_invoices(
        &self,
        params: Option<InvoiceQueryParams>,
    ) -> Result<InvoiceListResponse, Error> {
        let query_string = params.map(|p| p.to_query_string()).unwrap_or_default();
        let json = self.get(&format!("/v1/invoices{}", query_string)).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Get a quote for an invoice (returns BOLT11)
    ///
    /// See <https://docs.strike.me/api/issue-quote-for-invoice/>
    pub async fn invoice_quote(&self, invoice_id: &str) -> Result<InvoiceQuoteResponse, Error> {
        self.invoice_quote_with_options(invoice_id, InvoiceQuoteRequest::default())
            .await
    }

    /// Get a quote for an invoice with options (returns BOLT11)
    ///
    /// Use this method when you need to specify a description hash for BOLT11 compliance.
    /// See <https://docs.strike.me/api/issue-quote-for-invoice/>
    pub async fn invoice_quote_with_options(
        &self,
        invoice_id: &str,
        request: InvoiceQuoteRequest,
    ) -> Result<InvoiceQuoteResponse, Error> {
        let json = self
            .post(&format!("/v1/invoices/{}/quote", invoice_id), &request)
            .await?;
        Ok(serde_json::from_value(json)?)
    }

    // ==================== Payment Endpoints ====================

    /// Get a quote for paying a Lightning invoice
    pub async fn payment_quote(
        &self,
        request: PayInvoiceQuoteRequest,
    ) -> Result<PayInvoiceQuoteResponse, Error> {
        let json = self.post("/v1/payment-quotes/lightning", &request).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Execute a payment quote
    pub async fn pay_quote(&self, quote_id: &str) -> Result<InvoicePaymentResponse, Error> {
        let json = self
            .patch(&format!("/v1/payment-quotes/{}/execute", quote_id))
            .await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Get an outgoing payment by ID
    pub async fn get_outgoing_payment(
        &self,
        payment_id: &str,
    ) -> Result<InvoicePaymentResponse, Error> {
        let json = self.get(&format!("/v1/payments/{}", payment_id)).await?;
        Ok(serde_json::from_value(json)?)
    }

    // ==================== Currency Exchange Endpoints ====================

    /// Create a currency exchange quote
    pub async fn create_currency_exchange_quote(
        &self,
        request: CurrencyExchangeQuoteRequest,
    ) -> Result<CurrencyExchangeQuoteResponse, Error> {
        let json = self.post("/v1/currency-exchange-quotes", &request).await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Get a currency exchange quote by ID
    pub async fn get_currency_exchange_quote(
        &self,
        quote_id: &str,
    ) -> Result<CurrencyExchangeQuoteResponse, Error> {
        let json = self
            .get(&format!("/v1/currency-exchange-quotes/{}", quote_id))
            .await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Execute a currency exchange quote
    pub async fn execute_currency_exchange_quote(&self, quote_id: &str) -> Result<(), Error> {
        self.patch(&format!(
            "/v1/currency-exchange-quotes/{}/execute",
            quote_id
        ))
        .await?;
        Ok(())
    }

    // ==================== Webhook Endpoints ====================

    /// Subscribe to invoice webhooks
    pub async fn subscribe_to_invoice_webhook(&self, webhook_url: String) -> Result<(), Error> {
        let request = webhook::WebhookRequest {
            webhook_url,
            webhook_version: "v1".to_string(),
            secret: self.webhook_secret.clone(),
            enabled: true,
            event_types: vec!["invoice.updated".to_string()],
        };

        self.post("/v1/subscriptions", &request).await?;
        Ok(())
    }

    /// Subscribe to currency exchange webhooks
    pub async fn subscribe_to_currency_exchange_webhook(
        &self,
        webhook_url: String,
    ) -> Result<(), Error> {
        let request = webhook::WebhookRequest {
            webhook_url,
            webhook_version: "v1".to_string(),
            secret: self.webhook_secret.clone(),
            enabled: true,
            event_types: vec!["currency-exchange-quote.updated".to_string()],
        };

        self.post("/v1/subscriptions", &request).await?;
        Ok(())
    }

    /// Get current webhook subscriptions
    pub async fn get_current_subscriptions(
        &self,
    ) -> Result<Vec<webhook::WebhookInfoResponse>, Error> {
        let json = self.get("/v1/subscriptions").await?;
        Ok(serde_json::from_value(json)?)
    }

    /// Delete a webhook subscription
    pub async fn delete_subscription(&self, webhook_id: &str) -> Result<(), Error> {
        self.delete(&format!("/v1/subscriptions/{}", webhook_id))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_client() {
        let client = StrikeApi::new("test_key", None, 30000).unwrap();
        assert_eq!(client.base_url.as_str(), "https://api.strike.me/");
        assert_eq!(client.webhook_secret.len(), 15);
    }
}
