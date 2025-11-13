//! Square REST API client layer
//!
//! This module handles raw HTTP communication with Square's REST API.
//! It provides low-level API calls without business logic.

use serde::de::DeserializeOwned;
use serde_json::Value;
use squareup::config::Environment as SquareEnvironment;

use super::error::Error;
use super::types::{ListMerchantsResponse, ListPaymentsParams, ListPaymentsResponse, PaymentBrand};

/// Low-level Square API client for HTTP operations
pub struct SquareApiClient {
    environment: SquareEnvironment,
    http_client: reqwest::Client,
}

impl SquareApiClient {
    /// Create a new API client
    pub fn new(environment: SquareEnvironment) -> Result<Self, Error> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| {
                Error::SquareHttp(format!(
                    "Failed to build HTTP client: {}. This may indicate TLS backend issues.",
                    e
                ))
            })?;

        Ok(Self {
            environment,
            http_client,
        })
    }

    /// Get base URL for the configured environment
    pub fn base_url(&self) -> &str {
        match self.environment {
            SquareEnvironment::Production => "https://connect.squareup.com",
            SquareEnvironment::Sandbox => "https://connect.squareupsandbox.com",
        }
    }

    /// Make a GET request to Square API with detailed error diagnostics
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        token: &str,
        query_params: &[(&str, String)],
    ) -> Result<T, Error> {
        let url = format!("{}{}", self.base_url(), path);

        tracing::debug!("GET {} - Environment: {:?}", url, self.environment);

        let response = self
            .http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Square-Version", "2025-09-24")
            .header("Content-Type", "application/json")
            .query(query_params)
            .send()
            .await
            .map_err(|e| Error::SquareHttp(format!("Failed to send request to {}: {}", url, e)))?;

        tracing::debug!("Response status: {}", response.status());

        if !response.status().is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            tracing::error!("API error: {}", error_body);
            return Err(Error::SquareHttp(format!(
                "{} failed: {}",
                path, error_body
            )));
        }

        response
            .json()
            .await
            .map_err(|e| Error::SquareHttp(format!("Failed to parse response: {}", e)))
    }

    /// Make a POST request to Square API with detailed error diagnostics
    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        token: &str,
        body: &Value,
    ) -> Result<T, Error> {
        let url = format!("{}{}", self.base_url(), path);

        tracing::debug!("POST {} - Environment: {:?}", url, self.environment);

        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Square-Version", "2025-09-24")
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|e| Error::SquareHttp(format!("Failed to send request to {}: {}", url, e)))?;

        tracing::debug!("Response status: {}", response.status());

        if !response.status().is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            tracing::error!("API error: {}", error_body);
            return Err(Error::SquareHttp(format!(
                "{} failed: {}",
                path, error_body
            )));
        }

        response
            .json()
            .await
            .map_err(|e| Error::SquareHttp(format!("Failed to parse response: {}", e)))
    }

    /// List payments from Square API and apply client-side filtering based on the `params.brand` filter
    /// because the Square API does not support filtering by brand.
    pub async fn list_payments(
        &self,
        token: &str,
        params: &ListPaymentsParams,
    ) -> Result<ListPaymentsResponse, Error> {
        let mut query_params = vec![("limit", params.limit.to_string())];

        if let Some(ref begin_time) = params.begin_time {
            query_params.push(("begin_time", begin_time.clone()));
        }

        if let Some(ref cursor) = params.cursor {
            query_params.push(("cursor", cursor.clone()));
        }

        tracing::debug!(
            "Listing Square payments - begin_time: {:?}",
            params.begin_time
        );

        let mut response: ListPaymentsResponse =
            self.get("/v2/payments", token, &query_params).await?;

        // Apply client-side brand filtering
        if params.brand != PaymentBrand::All {
            response.payments.retain(|payment| {
                let brand = payment
                    .wallet_details
                    .as_ref()
                    .and_then(|details| details.brand.as_deref());
                params.brand.matches(brand)
            });
        }

        Ok(response)
    }

    /// List merchants from Square API
    pub async fn list_merchants(&self, token: &str) -> Result<ListMerchantsResponse, Error> {
        tracing::info!(
            "Listing Square merchants - Environment: {:?}",
            self.environment
        );

        self.get("/v2/merchants", token, &[]).await
    }

    /// List webhook subscriptions
    pub async fn list_webhook_subscriptions(&self, token: &str) -> Result<Value, Error> {
        self.get("/v2/webhooks/subscriptions", token, &[]).await
    }

    /// Create webhook subscription
    pub async fn create_webhook_subscription(
        &self,
        token: &str,
        body: &Value,
    ) -> Result<Value, Error> {
        self.post("/v2/webhooks/subscriptions", token, body).await
    }
}
