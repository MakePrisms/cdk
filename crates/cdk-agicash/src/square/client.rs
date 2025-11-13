//! Square payment backend core functionality
//!
//! This module contains the main application logic for tracking Lightning payments
//! through Square, including invoice checking, merchant caching, and sync coordination.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use cdk_common::database::mint::DynMintKVStore;
use cdk_common::lightning_invoice::Bolt11Invoice;
use cdk_common::util::hex;
use squareup::config::{Configuration as SquareConfiguration, Environment as SquareEnvironment};
use squareup::SquareClient;
use tokio::sync::RwLock;

use super::api::SquareApiClient;
use super::config::SquareConfig;
use super::db::SquareDatabase;
use super::error::Error;
use super::types::{ListMerchantsResponse, ListPaymentsParams, ListPaymentsResponse, PaymentData};
use super::util::{
    INVOICE_HASH_PREFIX, SQUARE_KV_PRIMARY_NAMESPACE, SQUARE_KV_SECONDARY_NAMESPACE,
};

/// Square payment backend for tracking Lightning payments
#[derive(Clone)]
pub struct Square {
    /// Square API client (unused but kept for SDK compatibility)
    pub(crate) client: Arc<SquareClient>,
    /// Low-level HTTP API client
    pub(crate) api_client: Arc<SquareApiClient>,
    /// Square API token (deprecated, kept for backward compatibility)
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
    /// PostgreSQL database URL for OAuth credentials
    pub(crate) database_url: String,
    /// PostgreSQL database connection for OAuth credentials (initialized in start())
    pub(crate) db: Arc<RwLock<Option<SquareDatabase>>>,
    /// Cached OAuth access token from database
    pub(crate) oauth_token: Arc<RwLock<String>>,
}

impl Square {
    /// Initialize Square backend from configuration
    ///
    /// Returns `Err` if configuration is invalid.
    /// Database initialization happens in `start()`.
    pub fn from_config(
        square_config: SquareConfig,
        webhook_url: Option<String>,
        kv_store: DynMintKVStore,
    ) -> Result<Self, Error> {
        // Square requires TLS for HTTPS requests
        // Install rustls crypto provider if not already set
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }

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

        let api_client = SquareApiClient::new(environment.clone())?;

        let square = Self {
            client: Arc::new(square_client),
            api_client: Arc::new(api_client),
            api_token: square_config.api_token,
            environment,
            webhook_url,
            webhook_enabled,
            payment_expiry: square_config.payment_expiry,
            kv_store,
            merchant_names: Arc::new(RwLock::new(Vec::new())),
            database_url: square_config.database_url,
            db: Arc::new(RwLock::new(None)),
            oauth_token: Arc::new(RwLock::new(String::new())),
        };

        Ok(square)
    }

    /// Start Square backend (setup webhook and sync payments)
    ///
    /// If webhook_enabled is true and webhook_url is configured, sets up webhook subscription.
    /// Otherwise, starts a background task that polls every 5 seconds.
    ///
    /// This method initializes the database connection and blocks until the initial payment sync completes successfully.
    /// If the initial sync fails, an error is returned and the mint will not start.
    pub async fn start(&self) -> Result<(), Error> {
        // Initialize database connection for OAuth credentials
        let db = SquareDatabase::new(&self.database_url).await?;

        // Fetch initial OAuth credentials from database
        let credentials = db.read_credentials().await.map_err(|e| {
            Error::SquareConfig(format!(
                "Failed to read OAuth credentials from database: {}. Ensure credentials are set up in PostgreSQL.",
                e
            ))
        })?;

        // Store the database connection and OAuth token
        {
            let mut db_lock = self.db.write().await;
            *db_lock = Some(db);
        }
        {
            let mut token_lock = self.oauth_token.write().await;
            *token_lock = credentials.access_token;
        }

        self.refresh_merchant_names().await?;

        if self.webhook_enabled && self.webhook_url.is_some() {
            // Attempt to set up webhook subscription, but don't fail if it doesn't work
            // Common reasons: subscription limit reached, authentication issues
            if let Err(e) = self.setup_webhook_subscription().await {
                tracing::warn!(
                    "Failed to set up Square webhook subscription: {}. Continuing without webhooks.",
                    e
                );
            }

            self.sync_payments().await?;
        } else {
            self.sync_payments().await?;

            // TODO: I commented this out because we can only have 3 webhook subscriptions on Square which means we
            // will have to disable webhook mode for production. This is more efficient than polling.
            // Most invoices will be paid within 30 seconds of creation, so polling every 5 seconds seems unnecessary,
            // when we can just resync the payments per payment request

            // let square = self.clone();
            // tokio::spawn(async move {
            //     let mut interval =
            //         tokio::time::interval(tokio::time::Duration::from_secs(SYNC_POLLING_INTERVAL));
            //     loop {
            //         interval.tick().await;
            //         if let Err(e) = square.sync_payments().await {
            //             tracing::warn!("Square payment sync failed: {}", e);
            //         }
            //     }
            // });
        }

        // Start background task to periodically refresh OAuth token from database
        let square_for_token_refresh = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let db_opt = square_for_token_refresh.db.read().await;
                if let Some(ref db) = *db_opt {
                    match db.read_credentials().await {
                        Ok(credentials) => {
                            drop(db_opt); // Release read lock before acquiring write lock
                            let mut token = square_for_token_refresh.oauth_token.write().await;
                            *token = credentials.access_token;
                            tracing::debug!("Refreshed OAuth access token from database");
                        }
                        Err(e) => {
                            tracing::warn!("Failed to refresh OAuth token from database: {}", e);
                        }
                    }
                }
            }
        });

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

    /// Store Square invoice payment hash to payment ID mapping in KV store with expiry
    pub(crate) async fn store_invoice_hash(
        &self,
        payment_hash: &[u8; 32],
        payment_id: &str,
    ) -> Result<(), Error> {
        let key = format!("{}{}", INVOICE_HASH_PREFIX, hex::encode(payment_hash));

        // Calculate expiry time based on payment_expiry config
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let expires_at = current_time + self.payment_expiry;

        let payment_data = PaymentData::new(expires_at, payment_id.to_string());
        let value: Vec<u8> = payment_data.into();

        let mut tx = self.kv_store.begin_transaction().await?;
        tx.kv_write(
            SQUARE_KV_PRIMARY_NAMESPACE,
            SQUARE_KV_SECONDARY_NAMESPACE,
            &key,
            &value,
        )
        .await?;
        tx.commit().await?;

        Ok(())
    }

    /// Remove all expired Square payments from the KV store
    /// Returns the number of expired payments removed
    pub(crate) async fn remove_expired_payments(&self) -> Result<usize, Error> {
        // List all keys in the namespace
        let keys = self
            .kv_store
            .kv_list(SQUARE_KV_PRIMARY_NAMESPACE, SQUARE_KV_SECONDARY_NAMESPACE)
            .await?;

        let mut expired_keys = Vec::new();

        // Check each payment for expiry
        for key in keys {
            let value = self
                .kv_store
                .kv_read(
                    SQUARE_KV_PRIMARY_NAMESPACE,
                    SQUARE_KV_SECONDARY_NAMESPACE,
                    &key,
                )
                .await?;

            if let Some(data) = value {
                if let Ok(payment_data) = PaymentData::try_from(data.as_slice()) {
                    if payment_data.is_expired() {
                        expired_keys.push(key);
                    }
                }
            }
        }

        // Remove expired payments
        let count = expired_keys.len();
        if count > 0 {
            let mut txn = self.kv_store.begin_transaction().await?;
            for key in expired_keys {
                txn.kv_remove(
                    SQUARE_KV_PRIMARY_NAMESPACE,
                    SQUARE_KV_SECONDARY_NAMESPACE,
                    &key,
                )
                .await?;
            }
            txn.commit().await?;
        }

        Ok(count)
    }

    /// List payments from Square API using the low-level API client
    pub async fn list_payments(
        &self,
        params: ListPaymentsParams,
    ) -> Result<ListPaymentsResponse, Error> {
        let oauth_token = self.oauth_token.read().await.clone();
        self.api_client.list_payments(&oauth_token, &params).await
    }

    /// List merchants associated with the access token
    ///
    /// According to Square's API, the access token is associated with a single merchant,
    /// so this typically returns a list with one merchant object.
    pub async fn list_merchants(&self) -> Result<ListMerchantsResponse, Error> {
        let oauth_token = self.oauth_token.read().await.clone();
        self.api_client.list_merchants(&oauth_token).await
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
}
