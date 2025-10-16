//! Square API client wrapper and core functionality

use std::sync::Arc;

use cdk_common::database::mint::DynMintKVStore;
use cdk_common::lightning_invoice::Bolt11Invoice;
use cdk_common::util::hex;
use squareup::config::{Configuration as SquareConfiguration, Environment as SquareEnvironment};
use squareup::SquareClient;
use tokio::sync::RwLock;

use crate::config::SquareConfig;
use crate::error::Error;
use crate::types::{ListMerchantsResponse, ListPaymentsParams, ListPaymentsResponse};
use crate::util::{
    INVOICE_HASH_PREFIX, SQUARE_KV_PRIMARY_NAMESPACE, SQUARE_KV_SECONDARY_NAMESPACE,
};

const SYNC_POLLING_INTERVAL: u64 = 5;

/// Square payment backend for tracking Lightning payments
#[derive(Clone)]
pub struct Square {
    /// Square API client
    pub(crate) client: Arc<SquareClient>,
    /// Square API token for direct API calls
    pub(crate) api_token: String,
    /// Square environment (sandbox or production)
    pub(crate) environment: SquareEnvironment,
    /// Square webhook notification URL (optional - if None, will use polling)
    pub(crate) webhook_url: Option<String>,
    /// Enable webhook mode (if false, uses polling mode even if webhook_url is provided)
    pub(crate) webhook_enabled: bool,
    /// Payment expiry time in seconds (how far back to sync payments)
    pub(crate) payment_expiry: u64,
    /// KV store for persistent data (invoice hashes, signature key, etc.)
    pub(crate) kv_store: DynMintKVStore,
    /// Cached merchant business names for invoice description matching
    pub(crate) merchant_names: Arc<RwLock<Vec<String>>>,
}

impl Square {
    /// Initialize Square backend from configuration
    ///
    /// Returns `Err` if configuration is invalid.
    pub fn from_config(
        square_config: SquareConfig,
        webhook_url: Option<String>,
        kv_store: DynMintKVStore,
    ) -> Result<Self, Error> {
        let environment = match square_config.environment.to_uppercase().as_str() {
            "PRODUCTION" => SquareEnvironment::Production,
            _ => SquareEnvironment::Sandbox,
        };

        let webhook_enabled = square_config.webhook_enabled;

        let config = SquareConfiguration {
            environment: environment.clone(),
            http_client_config: squareup::http::client::HttpClientConfiguration::default(),
            base_uri: squareup::config::BaseUri::default(),
        };

        let square_client = SquareClient::try_new(config)
            .map_err(|e| Error::SquareConfig(format!("Failed to create Square client: {}", e)))?;

        let square = Self {
            client: Arc::new(square_client),
            api_token: square_config.api_token,
            environment,
            webhook_url,
            webhook_enabled,
            payment_expiry: square_config.payment_expiry,
            kv_store,
            merchant_names: Arc::new(RwLock::new(Vec::new())),
        };

        Ok(square)
    }

    /// Start Square backend (setup webhook and sync payments)
    ///
    /// If webhook_enabled is true and webhook_url is configured, sets up webhook subscription.
    /// Otherwise, starts a background task that polls every 5 seconds.
    ///
    /// This method blocks until the initial payment sync completes successfully.
    /// If the initial sync fails, an error is returned and the mint will not start.
    pub async fn start(&self) -> Result<(), Error> {
        self.refresh_merchant_names().await?;

        if self.webhook_enabled && self.webhook_url.is_some() {
            self.setup_webhook_subscription().await?;

            self.sync_payments().await?;
        } else {
            self.sync_payments().await?;

            let square = self.clone();
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(tokio::time::Duration::from_secs(SYNC_POLLING_INTERVAL));
                loop {
                    interval.tick().await;
                    if let Err(e) = square.sync_payments().await {
                        tracing::warn!("Square payment sync failed: {}", e);
                    }
                }
            });
        }
        tracing::debug!("Square payment sync completed successfully");

        Ok(())
    }

    /// Check if a Square invoice exists by Bolt11 invoice
    ///
    /// Invoices created by Square merchants are assumed to have the merchant name in the description.
    ///
    /// This method first checks if the invoice description contains any cached merchant names.
    /// If no merchant name is found in the description, returns None without checking the KV store.
    /// This optimization avoids unnecessary database lookups for invoices that are not from Square merchants.
    ///
    /// If a merchant name is found but the payment hash is not in the KV store, it re-syncs the payments and checks again.
    pub async fn check_invoice_exists(&self, invoice: &Bolt11Invoice) -> Result<bool, Error> {
        let description = invoice.description().to_string();
        let description_lower = description.to_lowercase();

        let merchant_names = self.merchant_names.read().await;

        // Check if any merchant name appears in the description (case-insensitive)
        let merchant_found = merchant_names.iter().any(|merchant_name| {
            let merchant_name_lower = merchant_name.to_lowercase();
            description_lower.contains(&merchant_name_lower)
        });

        drop(merchant_names);

        if !merchant_found {
            tracing::debug!(
                "No Square merchant name found in invoice description: '{}'. Skipping KV store check.",
                description
            );
            return Ok(false);
        }

        let payment_hash: &[u8] = invoice.payment_hash().as_ref();
        let key = format!("{}{}", INVOICE_HASH_PREFIX, hex::encode(payment_hash));

        let result = self
            .kv_store
            .kv_read(
                SQUARE_KV_PRIMARY_NAMESPACE,
                SQUARE_KV_SECONDARY_NAMESPACE,
                &key,
            )
            .await?;

        // If found, return immediately
        if result.is_some() {
            return Ok(true);
        }

        // Not found in KV store - trigger re-sync to get latest payments
        tracing::debug!(
            "Payment hash not found in KV store, triggering re-sync for invoice with description: {}",
            description
        );

        self.sync_payments().await?;

        // Check KV store again after sync
        let result_after_sync = self
            .kv_store
            .kv_read(
                SQUARE_KV_PRIMARY_NAMESPACE,
                SQUARE_KV_SECONDARY_NAMESPACE,
                &key,
            )
            .await?;

        if let Some(bytes) = result_after_sync {
            tracing::debug!(
                "Found Square payment in KV store after re-sync: {} (hash: {})",
                String::from_utf8_lossy(&bytes).to_string(),
                hex::encode(payment_hash)
            );
            Ok(true)
        } else {
            tracing::debug!(
                "Payment not found in Square even after re-sync (hash: {}, description: {})",
                hex::encode(payment_hash),
                description
            );
            Ok(false)
        }
    }

    /// Store Square invoice payment hash to payment ID mapping in KV store
    pub(crate) async fn store_invoice_hash(
        &self,
        payment_hash: &[u8; 32],
        payment_id: &str,
    ) -> Result<(), Error> {
        let key = format!("{}{}", INVOICE_HASH_PREFIX, hex::encode(payment_hash));
        let value = payment_id.as_bytes();

        let mut tx = self.kv_store.begin_transaction().await?;
        tx.kv_write(
            SQUARE_KV_PRIMARY_NAMESPACE,
            SQUARE_KV_SECONDARY_NAMESPACE,
            &key,
            value,
        )
        .await?;
        tx.commit().await?;

        Ok(())
    }

    pub(crate) async fn remove_expired_payments(&self) -> Result<(), Error> {
        let mut tx = self.kv_store.begin_transaction().await?;
        tx.kv_remove_older_than(
            SQUARE_KV_PRIMARY_NAMESPACE,
            SQUARE_KV_SECONDARY_NAMESPACE,
            self.payment_expiry,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// These methods directly call the Square API via HTTP,
/// as the squareup SDK v2.13.2 does not expose the functionality we need.
impl Square {
    /// List payments from Square API
    ///
    /// Applies client-side filtering based on `params.brand` filter.
    pub async fn list_payments(
        &self,
        params: ListPaymentsParams,
    ) -> Result<ListPaymentsResponse, Error> {
        use crate::types::PaymentBrand;

        let base_url = self.get_base_url();

        let url = format!("{}/v2/payments", base_url);
        let client = reqwest::Client::new();

        let mut request = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .header("Square-Version", "2025-09-24")
            .query(&[("limit", params.limit.to_string())]);

        if let Some(ref begin_time) = params.begin_time {
            request = request.query(&[("begin_time", begin_time.as_str())]);
        }

        if let Some(ref cursor) = params.cursor {
            request = request.query(&[("cursor", cursor.as_str())]);
        }

        let response = request
            .send()
            .await
            .map_err(|e| Error::SquareHttp(format!("Failed to list payments: {}", e)))?;

        if !response.status().is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            tracing::error!("Failed to list Square payments: {}", error_body);
            return Err(Error::SquareHttp(format!(
                "List payments failed: {}",
                error_body
            )));
        }

        let mut response_body: ListPaymentsResponse = response
            .json()
            .await
            .map_err(|e| Error::SquareHttp(format!("Failed to parse payments response: {}", e)))?;

        if params.brand != PaymentBrand::All {
            response_body.payments.retain(|payment| {
                let brand = payment
                    .wallet_details
                    .as_ref()
                    .and_then(|details| details.brand.as_deref());
                params.brand.matches(brand)
            });
        }

        Ok(response_body)
    }

    /// List merchants associated with the access token
    ///
    /// According to Square's API, the access token is associated with a single merchant,
    /// so this typically returns a list with one merchant object.
    pub async fn list_merchants(&self) -> Result<ListMerchantsResponse, Error> {
        let base_url = self.get_base_url();

        let url = format!("{}/v2/merchants", base_url);
        let client = reqwest::Client::new();

        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .header("Square-Version", "2025-09-24")
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| Error::SquareHttp(format!("Failed to list merchants: {}", e)))?;

        if !response.status().is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            tracing::error!("Failed to list Square merchants: {}", error_body);
            return Err(Error::SquareHttp(format!(
                "List merchants failed: {}",
                error_body
            )));
        }

        let response_body: ListMerchantsResponse = response
            .json()
            .await
            .map_err(|e| Error::SquareHttp(format!("Failed to parse merchants response: {}", e)))?;

        Ok(response_body)
    }

    /// Refresh merchant names cache by listing merchants
    ///
    /// This method fetches all merchants and stores their business names in memory.
    pub async fn refresh_merchant_names(&self) -> Result<(), Error> {
        let merchants_response = self.list_merchants().await?;

        let mut merchant_names = self.merchant_names.write().await;
        merchant_names.clear();

        for merchant in merchants_response.merchant {
            let business_name = merchant
                .business_name
                .unwrap_or_else(|| merchant.id.clone());
            merchant_names.push(business_name);
        }

        tracing::debug!("Cached merchant names: {}", merchant_names.join(", "));

        Ok(())
    }

    pub(crate) fn get_base_url(&self) -> String {
        match self.environment {
            SquareEnvironment::Production => "https://connect.squareup.com".to_string(),
            SquareEnvironment::Sandbox => "https://connect.squareupsandbox.com".to_string(),
        }
    }
}
