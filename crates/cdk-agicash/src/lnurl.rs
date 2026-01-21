//! Lightning Address and LNURL utilities
//!
//! This module provides utilities for working with Lightning Addresses and the LNURL protocol.
//! Lightning addresses (e.g., "user@domain.com") can be resolved to bolt11 invoices using the
//! LNURL-pay protocol.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Error type for LNURL operations
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid lightning address format
    #[error("Invalid lightning address format: {0}")]
    InvalidFormat(String),

    /// HTTP/Network error
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// LNURL endpoint returned an error status
    #[error("LNURL endpoint returned error: {0}")]
    EndpointError(String),

    /// Amount is outside the allowed bounds
    #[error("Amount {amount} msats out of bounds ({min} - {max})")]
    AmountOutOfBounds {
        /// The requested amount
        amount: u64,
        /// Minimum allowed amount
        min: u64,
        /// Maximum allowed amount
        max: u64,
    },

    /// LNURL callback returned an error status
    #[error("LNURL callback returned error: {0}")]
    CallbackError(String),
}

/// LNURL pay request response
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LnUrlPayResponse {
    /// Callback URL to request invoice
    pub callback: String,
    /// Maximum sendable amount in millisatoshis
    pub max_sendable: u64,
    /// Minimum sendable amount in millisatoshis
    pub min_sendable: u64,
    /// Metadata string
    pub metadata: String,
    /// Maximum length of comment allowed
    #[serde(default)]
    pub comment_allowed: Option<u64>,
    /// Tag (should be "payRequest")
    pub tag: String,
}

/// LNURL callback response with invoice
#[derive(Debug, Deserialize, Serialize)]
pub struct LnUrlCallbackResponse {
    /// The bolt11 payment request
    pub pr: String,
    /// Optional success action
    #[serde(default)]
    pub success_action: Option<Value>,
    /// Optional routes
    #[serde(default)]
    pub routes: Vec<Value>,
}

/// Resolve a lightning address to a bolt11 invoice
///
/// This implements the LNURL-pay protocol to convert a lightning address
/// (e.g., "user@domain.com") into a bolt11 invoice for the specified amount.
///
/// # Arguments
/// * `lightning_address` - The lightning address to resolve (e.g., "user@wallet.com")
/// * `amount_msats` - The amount in millisatoshis
/// * `comment` - Optional comment for the payment
///
/// # Returns
/// The bolt11 invoice string
///
/// # Errors
/// Returns an error if:
/// - The lightning address format is invalid
/// - The LNURL endpoint cannot be reached
/// - The amount is outside the allowed bounds
/// - The callback fails
///
/// # Example
/// ```no_run
/// # use cdk_agicash::lnurl::resolve_lightning_address;
/// # use cdk_agicash::lnurl::Error;
/// # async fn example() -> Result<(), Error> {
/// let invoice =
///     resolve_lightning_address("user@wallet.com", 1000, Some("Payment comment")).await?;
/// println!("Invoice: {}", invoice);
/// # Ok(())
/// # }
/// ```
pub async fn resolve_lightning_address(
    lightning_address: &str,
    amount_msats: u64,
    comment: Option<&str>,
) -> Result<String, Error> {
    // Parse the lightning address
    let parts: Vec<&str> = lightning_address.split('@').collect();
    if parts.len() != 2 {
        return Err(Error::InvalidFormat(lightning_address.to_string()));
    }
    let (username, domain) = (parts[0], parts[1]);

    // Construct the LNURL endpoint
    let lnurl_endpoint = format!("https://{}/.well-known/lnurlp/{}", domain, username);

    // Fetch LNURL pay response
    let client = reqwest::Client::new();
    let lnurl_response = client.get(&lnurl_endpoint).send().await?;

    if !lnurl_response.status().is_success() {
        return Err(Error::EndpointError(lnurl_response.status().to_string()));
    }

    let lnurl_pay: LnUrlPayResponse = lnurl_response.json().await?;

    // Validate amount is within bounds
    if amount_msats < lnurl_pay.min_sendable || amount_msats > lnurl_pay.max_sendable {
        return Err(Error::AmountOutOfBounds {
            amount: amount_msats,
            min: lnurl_pay.min_sendable,
            max: lnurl_pay.max_sendable,
        });
    }

    // Request invoice from callback
    let mut query_params = vec![("amount", amount_msats.to_string())];

    if let Some(comment) = comment {
        if let Some(max_len) = lnurl_pay.comment_allowed {
            if max_len > 0 {
                let comment_len = comment.len() as u64;
                let truncated_comment = if comment_len > max_len {
                    &comment[..max_len as usize]
                } else {
                    comment
                };
                query_params.push(("comment", truncated_comment.to_string()));
            }
        }
    }

    let callback_response = client
        .get(&lnurl_pay.callback)
        .query(&query_params)
        .send()
        .await?;

    if !callback_response.status().is_success() {
        return Err(Error::CallbackError(callback_response.status().to_string()));
    }

    let callback_result: LnUrlCallbackResponse = callback_response.json().await?;

    Ok(callback_result.pr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lnurl_pay_response_deserialization() {
        let json = r#"{
            "callback": "https://example.com/callback",
            "maxSendable": 100000000,
            "minSendable": 1000,
            "metadata": "[[\"text/plain\",\"Payment to user@example.com\"]]",
            "tag": "payRequest"
        }"#;

        let response: LnUrlPayResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.callback, "https://example.com/callback");
        assert_eq!(response.max_sendable, 100000000);
        assert_eq!(response.min_sendable, 1000);
        assert_eq!(response.tag, "payRequest");
    }

    #[test]
    fn test_lnurl_callback_response_deserialization() {
        let json = r#"{
            "pr": "lnbc1..."
        }"#;

        let response: LnUrlCallbackResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.pr, "lnbc1...");
        assert!(response.success_action.is_none());
        assert!(response.routes.is_empty());
    }
}
