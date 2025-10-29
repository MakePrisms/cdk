//! Webhook handling for Square payment notifications

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use bitcoin::base64::Engine as _;
use bitcoin::secp256k1::hashes::{hmac, sha256, Hash, HashEngine, HmacEngine};
use serde_json::Value;
use uuid::Uuid;

use crate::error::Error;
use crate::util::{
    SIGNATURE_KEY_STORAGE_KEY, SQUARE_KV_CONFIG_NAMESPACE, SQUARE_KV_PRIMARY_NAMESPACE,
};

/// Webhook functionality for Square
impl crate::client::Square {
    /// Set up Square webhook subscription (idempotent)
    pub(crate) async fn setup_webhook_subscription(&self) -> Result<(), Error> {
        // If no webhook URL is configured, skip webhook setup
        let webhook_url = match &self.webhook_url {
            Some(url) => url,
            None => {
                return Err(Error::SquareConfig(
                    "Webhook URL not configured".to_string(),
                ))
            }
        };

        let base_url = self.get_base_url();

        // List existing subscriptions to check if we already have one
        let client = reqwest::Client::new();
        let list_response = client
            .get(format!("{}/v2/webhooks/subscriptions", base_url))
            .header("Authorization", format!("Bearer {}", self.api_token))
            .header("Square-Version", "2025-09-24")
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| {
                Error::SquareHttp(format!("Failed to list webhook subscriptions: {}", e))
            })?;

        if list_response.status().is_success() {
            let list_body: Value = list_response.json().await.map_err(|e| {
                Error::SquareHttp(format!("Failed to parse list webhook response: {}", e))
            })?;

            // Check if we already have a subscription with our webhook URL and payment.created event
            if let Some(subscriptions) = list_body.get("subscriptions").and_then(|s| s.as_array()) {
                for subscription in subscriptions {
                    let notification_url = subscription
                        .get("notification_url")
                        .and_then(|u| u.as_str());
                    let event_types = subscription
                        .get("event_types")
                        .and_then(|e| e.as_array())
                        .map(|events| {
                            events.iter().any(|e| {
                                e.as_str().map(|s| s == "payment.created").unwrap_or(false)
                            })
                        })
                        .unwrap_or(false);

                    if notification_url == Some(webhook_url.as_str()) && event_types {
                        let subscription_id = subscription
                            .get("id")
                            .and_then(|id| id.as_str())
                            .unwrap_or("unknown");
                        tracing::info!(
                            "Using existing Square webhook subscription: {}",
                            subscription_id
                        );
                        return Ok(());
                    }
                }
            }
        } else {
            tracing::warn!(
                "Failed to list webhook subscriptions: {}",
                list_response.status()
            );
            // Continue to try creating a new one
        }

        // No existing subscription found, create a new one
        let idempotency_key = Uuid::new_v4().to_string();

        // Build the webhook subscription request using raw JSON to specify event types
        // since the SDK may not expose all event type enums
        let subscription_json = serde_json::json!({
            "idempotency_key": idempotency_key,
            "subscription": {
                "name": "CDK Payment Notifications",
                "enabled": true,
                "event_types": ["payment.created"],
                "notification_url": webhook_url,
                "api_version": "2025-09-24"
            }
        });

        let base_url = self.get_base_url();

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/v2/webhooks/subscriptions", base_url))
            .header("Authorization", format!("Bearer {}", self.api_token))
            .header("Content-Type", "application/json")
            .json(&subscription_json)
            .send()
            .await
            .map_err(|e| {
                Error::SquareHttp(format!("Failed to create webhook subscription: {}", e))
            })?;

        if response.status().is_success() {
            let response_body: Value = response.json().await.map_err(|e| {
                Error::SquareHttp(format!("Failed to parse webhook response: {}", e))
            })?;

            let subscription_id = response_body
                .get("subscription")
                .and_then(|s| s.get("id"))
                .and_then(|id| id.as_str())
                .unwrap_or("unknown");
            let signature_key = response_body
                .get("subscription")
                .and_then(|s| s.get("signature_key"))
                .and_then(|key| key.as_str());

            tracing::info!("Created Square webhook subscription: {}", subscription_id);

            // Store the signature key for webhook verification
            if let Some(key) = signature_key {
                self.store_signature_key(key).await?;
            } else {
                tracing::warn!("Square webhook response did not include signature_key");
            }

            Ok(())
        } else {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            tracing::error!(
                "Failed to create Square webhook subscription: {}",
                error_body
            );
            Err(Error::SquareHttp(format!(
                "Webhook creation failed: {}",
                error_body
            )))
        }
    }

    /// Store Square webhook signature key in KV store
    pub(crate) async fn store_signature_key(&self, signature_key: &str) -> Result<(), Error> {
        let mut tx = self.kv_store.begin_transaction().await?;
        tx.kv_write(
            SQUARE_KV_PRIMARY_NAMESPACE,
            SQUARE_KV_CONFIG_NAMESPACE,
            SIGNATURE_KEY_STORAGE_KEY,
            signature_key.as_bytes(),
        )
        .await?;
        tx.commit().await?;

        Ok(())
    }

    /// Retrieve Square webhook signature key from KV store
    async fn get_signature_key(&self) -> Result<Option<String>, Error> {
        let result = self
            .kv_store
            .kv_read(
                SQUARE_KV_PRIMARY_NAMESPACE,
                SQUARE_KV_CONFIG_NAMESPACE,
                SIGNATURE_KEY_STORAGE_KEY,
            )
            .await?;

        Ok(result.map(|bytes| String::from_utf8_lossy(&bytes).to_string()))
    }

    /// Verify Square webhook signature
    ///
    /// This validates that the webhook request came from Square by verifying
    /// the HMAC-SHA256 signature in the request header.
    ///
    /// # Arguments
    /// * `webhook_url` - The full URL of the webhook endpoint
    /// * `raw_body` - The raw request body bytes
    /// * `signature_header` - The signature from the x-square-hmacsha256-signature header
    ///
    /// # Returns
    /// * `Ok(true)` if signature is valid
    /// * `Ok(false)` if signature is invalid or signature key not found
    /// * `Err` if there's a database error
    ///
    /// See: <https://developer.squareup.com/docs/webhooks/step3validate>
    async fn verify_webhook_signature(
        &self,
        webhook_url: &str,
        raw_body: &[u8],
        signature_header: &str,
    ) -> Result<bool, Error> {
        let signature_key = match self.get_signature_key().await? {
            Some(key) => key,
            None => {
                tracing::warn!("Square webhook signature key not found in KV store");
                return Ok(false);
            }
        };

        // Compute the expected signature
        // According to Square docs: HMAC-SHA256(notification_url + request_body, signature_key)
        let mut engine = HmacEngine::<sha256::Hash>::new(signature_key.as_bytes());

        // Concatenate URL + body
        engine.input(webhook_url.as_bytes());
        engine.input(raw_body);

        // Finalize and encode to base64
        let hmac_result = hmac::Hmac::<sha256::Hash>::from_engine(engine);
        let expected_signature =
            bitcoin::base64::engine::general_purpose::STANDARD.encode(hmac_result.to_byte_array());

        // Constant-time comparison to prevent timing attacks
        let signatures_match = expected_signature.len() == signature_header.len()
            && expected_signature
                .as_bytes()
                .iter()
                .zip(signature_header.as_bytes().iter())
                .all(|(a, b)| a == b);

        if !signatures_match {
            tracing::warn!(
                "Square webhook signature mismatch. Expected: {}, Got: {}",
                expected_signature,
                signature_header
            );
        }

        Ok(signatures_match)
    }

    /// Create Square webhook router for payment notifications
    /// This router can be merged into the mint's main router
    ///
    /// Returns `None` if webhook mode is disabled or no webhook URL is configured (polling mode).
    pub fn create_webhook_router(&self) -> Option<Router> {
        // If webhook mode is disabled, return None
        if !self.webhook_enabled {
            tracing::info!(
                "Webhook mode disabled in configuration, skipping webhook router creation"
            );
            return None;
        }

        // If no webhook URL is configured, return None and error if webhook mode is enabled
        let webhook_url = match &self.webhook_url {
            Some(url) => url.clone(),
            None => {
                return None;
            }
        };

        let square = self.clone();
        let webhook_url_owned = webhook_url.clone();

        // Create the webhook handler
        let handler = move |headers: HeaderMap, body: Bytes| {
            let square = square.clone();
            let webhook_url = webhook_url_owned.clone();

            async move {
                let signature = match headers.get("x-square-hmacsha256-signature") {
                    Some(sig) => match sig.to_str() {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!("Invalid signature header format: {}", e);
                            return StatusCode::BAD_REQUEST.into_response();
                        }
                    },
                    None => {
                        tracing::warn!("Missing x-square-hmacsha256-signature header");
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                };

                // Verify the signature
                match square
                    .verify_webhook_signature(&webhook_url, &body, signature)
                    .await
                {
                    Ok(true) => {
                        tracing::debug!("Square webhook signature verified");
                    }
                    Ok(false) => {
                        tracing::warn!("Square webhook signature verification failed");
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    Err(e) => {
                        tracing::error!("Error verifying Square webhook signature: {}", e);
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                }

                // Parse the JSON payload
                let payload: Value = match serde_json::from_slice(&body) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("Failed to parse webhook JSON: {}", e);
                        return StatusCode::BAD_REQUEST.into_response();
                    }
                };

                // Process the webhook
                match square.process_payment_webhook(&payload).await {
                    Ok(_) => {
                        tracing::debug!("Successfully processed Square webhook");
                        StatusCode::OK.into_response()
                    }
                    Err(e) => {
                        tracing::error!("Failed to process Square webhook: {}", e);
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            }
        };

        // Extract just the path from the URL for the route
        let webhook_path = webhook_url
            .split("://")
            .nth(1)
            .and_then(|s| s.split_once('/'))
            .map(|(_, path)| format!("/{}", path))
            .unwrap_or_else(|| "/webhook/square/payment".to_string());

        // Create the router with the webhook endpoint
        let router = Router::new().route(&webhook_path, post(handler));

        tracing::info!(
            "Created Square webhook router at path: {} (full URL: {})",
            webhook_path,
            webhook_url
        );

        Some(router)
    }

    /// Process a Square payment webhook from raw JSON payload
    ///
    /// This method parses the raw webhook JSON to extract Lightning payment details
    /// that are not exposed in the squareup SDK type system.
    pub async fn process_payment_webhook(&self, webhook_payload: &Value) -> Result<(), Error> {
        use std::str::FromStr;

        use cdk_common::util::hex;
        use cdk_common::Bolt11Invoice;

        // Extract payment data from webhook JSON
        // Square webhook structure: data.object.payment
        let payment = match webhook_payload
            .get("data")
            .and_then(|d| d.get("object"))
            .and_then(|obj| obj.get("payment"))
        {
            Some(payment_obj) => payment_obj,
            None => {
                tracing::warn!("Webhook payload missing data.object.payment");
                return Ok(());
            }
        };

        let payment_id = match payment.get("id").and_then(|id| id.as_str()) {
            Some(id) => id,
            None => {
                tracing::warn!("Payment missing id field");
                return Ok(());
            }
        };

        // Check if it's a LIGHTNING payment
        let wallet_details = match payment.get("wallet_details") {
            Some(details) => details,
            None => {
                tracing::debug!("Payment {} has no wallet_details", payment_id);
                return Ok(());
            }
        };

        let _brand = match wallet_details.get("brand").and_then(|b| b.as_str()) {
            Some("LIGHTNING") => "LIGHTNING",
            _ => {
                tracing::debug!("Payment {} is not a LIGHTNING payment", payment_id);
                return Ok(());
            }
        };

        // Extract lightning details
        let lightning_details = match wallet_details.get("lightning_details") {
            Some(details) => details,
            None => {
                tracing::warn!("LIGHTNING payment {} missing lightning_details", payment_id);
                return Ok(());
            }
        };

        let payment_url = match lightning_details
            .get("payment_url")
            .and_then(|url| url.as_str())
        {
            Some(url) => url,
            None => {
                tracing::warn!("LIGHTNING payment {} missing payment_url", payment_id);
                return Ok(());
            }
        };

        // Parse bolt11 from payment_url
        let bolt11_str = payment_url
            .strip_prefix("lightning:")
            .unwrap_or(payment_url)
            .to_uppercase();

        // Parse the invoice and extract payment hash
        match Bolt11Invoice::from_str(&bolt11_str) {
            Ok(invoice) => {
                let payment_hash = invoice.payment_hash().as_ref();

                // Store in KV store
                self.store_invoice_hash(payment_hash, payment_id).await?;

                tracing::info!(
                    "Synced Square LIGHTNING payment from webhook: {} (hash: {})",
                    payment_id,
                    hex::encode(payment_hash)
                );
                Ok(())
            }
            Err(e) => {
                let err_msg = format!(
                    "Failed to parse bolt11 invoice for webhook payment {}: {}",
                    payment_id, e
                );
                tracing::warn!("{}", err_msg);
                Err(Error::Bolt11Parse(err_msg))
            }
        }
    }
}
