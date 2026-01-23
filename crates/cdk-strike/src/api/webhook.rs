//! Strike webhook handling
//!
//! Webhooks are HTTP-based notifications that Strike sends when specific events
//! occur, such as invoice state changes or payment completions.
//!
//! See <https://docs.strike.me/webhooks/overview/> for the official documentation.
//!
//! # Signature Verification
//!
//! All webhooks are signed using HMAC-SHA256 for authentication:
//!
//! 1. Extract signature from `X-Webhook-Signature` header (hex-encoded)
//! 2. Compute HMAC-SHA256 of the raw request body using your webhook secret
//! 3. Compare signatures using timing-safe comparison to prevent timing attacks
//!
//! See <https://docs.strike.me/webhooks/signature-verification/> for details.
//!
//! # Event Types
//!
//! This module handles subscriptions for:
//! - `invoice.updated` - Invoice state changes (UNPAID → PAID)
//! - `currency-exchange-quote.updated` - Exchange quote state changes
//!
//! Other available event types (not currently used):
//! - `payment.created`, `payment.updated`
//! - `receive-request.receive-pending`, `receive-request.receive-completed`
//!
//! # Webhook Payload
//!
//! ```json
//! {
//!   "id": "event-uuid",
//!   "eventType": "invoice.updated",
//!   "webhookVersion": "v1",
//!   "data": {
//!     "entityId": "invoice-or-quote-uuid",
//!     "changes": ["state"]
//!   },
//!   "created": "2024-01-15T12:00:00Z",
//!   "deliverySuccess": true
//! }
//! ```
//!
//! **Important:** The webhook payload does NOT contain the new state value.
//! You must fetch the entity via the API to get the current state.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use ring::hmac;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Webhook subscription request sent to Strike API
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookRequest {
    /// URL to receive webhook notifications
    pub webhook_url: String,
    /// Webhook API version (currently "v1")
    pub webhook_version: String,
    /// Secret for HMAC signature verification
    pub secret: String,
    /// Whether the webhook is enabled
    pub enabled: bool,
    /// Event types to subscribe to
    pub event_types: Vec<String>,
}

/// Webhook subscription info response from Strike API
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookInfoResponse {
    /// Unique subscription identifier
    pub id: String,
    /// URL receiving webhook notifications
    pub webhook_url: String,
    /// Webhook API version
    pub webhook_version: String,
    /// Whether the webhook is enabled
    pub enabled: bool,
    /// Subscribed event types
    pub event_types: Vec<String>,
}

/// Webhook event payload received from Strike
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookEvent {
    /// Unique event identifier
    pub id: String,
    /// Event type (e.g., "invoice.updated")
    pub event_type: String,
    /// Webhook API version
    pub webhook_version: String,
    /// Event data payload
    pub data: WebhookData,
    /// Event creation timestamp (ISO 8601)
    pub created: String,
    /// Whether delivery was successful (for retries)
    #[serde(default)]
    pub delivery_success: Option<bool>,
}

/// Webhook event data payload
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookData {
    /// ID of the affected entity (invoice or quote)
    pub entity_id: String,
    /// List of changed fields
    #[serde(default)]
    pub changes: Vec<String>,
}

/// State for webhook handlers
#[derive(Clone)]
pub struct WebhookState {
    /// Channel sender for forwarding entity IDs
    pub sender: mpsc::Sender<String>,
    /// Secret for signature verification
    pub secret: String,
}

/// Verify webhook request signature using HMAC-SHA256
fn verify_signature(signature: &str, body: &[u8], secret: &[u8]) -> anyhow::Result<()> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);

    let signature_bytes =
        hex::decode(signature).map_err(|_| anyhow::anyhow!("Invalid signature hex"))?;

    hmac::verify(&key, body, &signature_bytes).map_err(|_| anyhow::anyhow!("Invalid signature"))?;

    Ok(())
}

/// Middleware to verify webhook signatures
async fn verify_request_body(
    State(state): State<WebhookState>,
    request: Request<Body>,
    next: Next,
) -> Result<impl IntoResponse, Response> {
    // Extract signature header
    let signature = request
        .headers()
        .get("X-Webhook-Signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            warn!("Missing X-Webhook-Signature header");
            (StatusCode::UNAUTHORIZED, "Missing signature").into_response()
        })?
        .to_string();

    // Collect body bytes
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, 1024 * 1024).await.map_err(|e| {
        warn!("Failed to read request body: {}", e);
        (StatusCode::BAD_REQUEST, "Invalid body").into_response()
    })?;

    // Verify signature
    if let Err(e) = verify_signature(&signature, &bytes, state.secret.as_bytes()) {
        warn!("Webhook signature verification failed: {}", e);
        return Err((StatusCode::UNAUTHORIZED, "Invalid signature").into_response());
    }

    debug!("Webhook signature verified");

    // Reconstruct request with body
    let request = Request::from_parts(parts, Body::from(bytes));
    Ok(next.run(request).await)
}

/// Handle invoice webhook events
async fn handle_invoice_webhook(
    State(state): State<WebhookState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let event: WebhookEvent = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to parse webhook event: {}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    debug!(
        "Received invoice webhook: {} - {}",
        event.event_type, event.data.entity_id
    );

    // Send entity ID to channel
    if let Err(e) = state.sender.send(event.data.entity_id).await {
        warn!("Failed to send webhook event to channel: {}", e);
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    StatusCode::OK
}

/// Handle currency exchange webhook events
async fn handle_exchange_webhook(
    State(state): State<WebhookState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let event: WebhookEvent = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to parse webhook event: {}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    debug!(
        "Received exchange webhook: {} - {}",
        event.event_type, event.data.entity_id
    );

    // Send entity ID to channel
    if let Err(e) = state.sender.send(event.data.entity_id).await {
        warn!("Failed to send webhook event to channel: {}", e);
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    StatusCode::OK
}

/// Create an Axum router for invoice webhooks
///
/// The router handles POST requests to the specified endpoint, verifies
/// the webhook signature, and forwards the entity ID to the provided channel.
pub fn create_invoice_webhook_router(
    endpoint: &str,
    sender: mpsc::Sender<String>,
    secret: String,
) -> Router {
    let state = WebhookState { sender, secret };

    Router::new()
        .route(endpoint, post(handle_invoice_webhook))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            verify_request_body,
        ))
        .with_state(state)
}

/// Create an Axum router for currency exchange webhooks
///
/// The router handles POST requests to the specified endpoint, verifies
/// the webhook signature, and forwards the entity ID to the provided channel.
pub fn create_exchange_webhook_router(
    endpoint: &str,
    sender: mpsc::Sender<String>,
    secret: String,
) -> Router {
    let state = WebhookState { sender, secret };

    Router::new()
        .route(endpoint, post(handle_exchange_webhook))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            verify_request_body,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_signature() {
        // Generate a test signature
        let secret = b"test_secret";
        let body = b"test body";
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
        let tag = hmac::sign(&key, body);
        let signature = hex::encode(tag.as_ref());

        // Verify should succeed
        assert!(verify_signature(&signature, body, secret).is_ok());

        // Wrong body should fail
        assert!(verify_signature(&signature, b"wrong body", secret).is_err());
    }
}
