//! Closed loop payment functionality
//!
//! This module provides functionality for registering and validating internal payments
//! to implement closed loop payment systems.

use std::convert::TryFrom;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cdk_common::database::mint::DynMintKVStore;
use cdk_common::database::Error as DbError;
use parking_lot::Mutex;
use tokio::select;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::square::Square;

/// Primary namespace for closed loop payment data in KV store
const CLOSED_LOOP_PRIMARY_NAMESPACE: &str = "cdk_closed_loop";
/// Secondary namespace for internal payment identifiers in KV store
const CLOSED_LOOP_SECONDARY_NAMESPACE: &str = "internal_payments";

/// Cleanup period for expired payments (1 hour)
const CLEANUP_PERIOD: Duration = Duration::from_secs(3600);

/// Type of closed loop validation
#[derive(Clone)]
pub enum ClosedLoopType {
    /// Internal payments only - validates against registered payment IDs in KV store
    Internal,
    /// Node pubkey validation - validates the invoice is from a specific node
    NodePubkey(String),
    /// Square payment validation - validates the invoice is a Square Lightning payment
    Square(Square),
}

impl std::fmt::Debug for ClosedLoopType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Internal => write!(f, "Internal"),
            Self::NodePubkey(pubkey) => write!(f, "NodePubkey({})", pubkey),
            Self::Square(_) => write!(f, "Square"),
        }
    }
}

/// Closed loop configuration
#[derive(Debug, Clone)]
pub struct ClosedLoopConfig {
    /// Type of closed loop validation
    pub loop_type: ClosedLoopType,
    /// Valid destination name to display in error messages
    pub valid_destination_name: String,
}

impl ClosedLoopConfig {
    /// Create a new closed loop config for internal payments
    pub fn internal(valid_destination_name: impl Into<String>) -> Self {
        Self {
            loop_type: ClosedLoopType::Internal,
            valid_destination_name: valid_destination_name.into(),
        }
    }

    /// Create a new closed loop config for node pubkey validation
    pub fn node_pubkey(
        node_pubkey: impl Into<String>,
        valid_destination_name: impl Into<String>,
    ) -> Self {
        Self {
            loop_type: ClosedLoopType::NodePubkey(node_pubkey.into()),
            valid_destination_name: valid_destination_name.into(),
        }
    }

    /// Create a new closed loop config for Square payment validation
    pub fn square(square: Square, valid_destination_name: impl Into<String>) -> Self {
        Self {
            loop_type: ClosedLoopType::Square(square),
            valid_destination_name: valid_destination_name.into(),
        }
    }
}

/// Payment data stored in KV store
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentData {
    /// Timestamp when the payment expires (Unix seconds)
    pub expires_at: u64,
}

impl PaymentData {
    /// Create a new payment data entry
    pub fn new(expires_at: u64) -> Self {
        Self { expires_at }
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
    /// Format: 8 bytes expires_at timestamp
    fn from(payment_data: PaymentData) -> Self {
        payment_data.expires_at.to_le_bytes().to_vec()
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
        Ok(Self::new(expires_at))
    }
}

/// Closed loop payment manager
#[derive(Clone)]
pub struct ClosedLoopManager {
    inner: Arc<ClosedLoopManagerInner>,
}

struct ClosedLoopManagerInner {
    kv_store: DynMintKVStore,
    config: ClosedLoopConfig,
    cleanup: CleanupSupervisor,
}

struct CleanupSupervisor {
    shutdown: CancellationToken,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl CleanupSupervisor {
    fn new() -> Self {
        Self {
            shutdown: CancellationToken::new(),
            handle: Mutex::new(None),
        }
    }

    fn start(&self, manager: ClosedLoopManager) {
        let mut guard = self.handle.lock();
        if guard.is_some() {
            return;
        }

        let shutdown = self.shutdown.clone();
        let task = tokio::spawn(async move {
            run_expired_payment_cleanup(&manager).await;

            let mut ticker = tokio::time::interval(CLEANUP_PERIOD);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                select! {
                    _ = shutdown.cancelled() => {
                        break;
                    }
                    _ = ticker.tick() => {
                        run_expired_payment_cleanup(&manager).await;
                    }
                }
            }
        });

        *guard = Some(task);
    }
}

impl Drop for CleanupSupervisor {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(handle) = self.handle.get_mut().take() {
            handle.abort();
        }
    }
}

impl ClosedLoopManager {
    /// Create a new closed loop manager
    ///
    /// Automatically starts a background cleanup task that runs every hour to remove expired payments.
    /// If the loop type is Square, starts the Square sync system.
    pub async fn new(
        kv_store: DynMintKVStore,
        config: ClosedLoopConfig,
    ) -> Result<Self, cdk_common::error::Error> {
        // Start Square if configured
        if let ClosedLoopType::Square(ref square) = config.loop_type {
            square.start().await.map_err(|e| {
                cdk_common::error::Error::SendError(format!("Failed to start Square: {}", e))
            })?;
        }

        let manager = Self {
            inner: Arc::new(ClosedLoopManagerInner {
                kv_store,
                config,
                cleanup: CleanupSupervisor::new(),
            }),
        };

        manager.inner.cleanup.start(manager.clone());

        Ok(manager)
    }

    /// Get the valid destination name from the config
    pub fn valid_destination_name(&self) -> &str {
        &self.inner.config.valid_destination_name
    }

    /// Check if the closed loop type is Internal
    pub fn is_internal(&self) -> bool {
        matches!(self.inner.config.loop_type, ClosedLoopType::Internal)
    }

    /// Register a payment with expiry
    ///
    /// # Arguments
    /// * `payment_id` - The payment identifier to register (e.g., payment hash)
    /// * `expiry_secs` - Time in seconds from now that the payment will expire
    pub async fn register_payment(
        &self,
        payment_id: impl Into<String>,
        expiry_secs: u64,
    ) -> Result<(), cdk_common::error::Error> {
        let payment_id = payment_id.into();
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let payment_data = PaymentData::new(current_time + expiry_secs);
        let data: Vec<u8> = payment_data.into();

        let mut txn = self.inner.kv_store.begin_transaction().await?;
        txn.kv_write(
            CLOSED_LOOP_PRIMARY_NAMESPACE,
            CLOSED_LOOP_SECONDARY_NAMESPACE,
            &payment_id,
            &data,
        )
        .await?;
        Box::new(txn).commit().await?;

        Ok(())
    }

    /// Validate a payment against closed loop rules
    ///
    /// For `Internal` type: checks if the payment_id exists in the KV store.
    /// For `NodePubkey` type: validates the invoice is from the configured node pubkey.
    /// For `Square` type: validates the invoice exists in Square's system.
    ///
    /// Returns `Ok(())` if valid, throws `InvalidDestination` error with destination name if not found.
    pub async fn validate_payment(
        &self,
        payment_id: impl AsRef<str>,
        bolt11: Option<&cdk_common::Bolt11Invoice>,
    ) -> Result<(), cdk_common::error::Error> {
        use cdk_common::error::Error;

        let valid_dest = &self.inner.config.valid_destination_name;

        match &self.inner.config.loop_type {
            ClosedLoopType::Internal => {
                match self
                    .inner
                    .kv_store
                    .kv_read(
                        CLOSED_LOOP_PRIMARY_NAMESPACE,
                        CLOSED_LOOP_SECONDARY_NAMESPACE,
                        payment_id.as_ref(),
                    )
                    .await
                {
                    Ok(Some(_)) => Ok(()),
                    Ok(None) => Err(Error::InvalidDestination(valid_dest.clone())),
                    Err(e) => Err(Error::Database(e)),
                }
            }
            ClosedLoopType::NodePubkey(expected_pubkey) => {
                let invoice =
                    bolt11.ok_or_else(|| Error::InvalidDestination(valid_dest.clone()))?;

                let payee_pubkey = invoice
                    .payee_pub_key()
                    .map(|pk| pk.to_string())
                    .unwrap_or_else(|| invoice.recover_payee_pub_key().to_string());

                if payee_pubkey != *expected_pubkey {
                    return Err(Error::InvalidDestination(valid_dest.clone()));
                }

                Ok(())
            }
            ClosedLoopType::Square(square) => {
                let invoice =
                    bolt11.ok_or_else(|| Error::InvalidDestination(valid_dest.clone()))?;

                let exists = square
                    .check_invoice_exists(invoice)
                    .await
                    .map_err(|e| Error::RecvError(format!("Square validation error: {}", e)))?;

                if !exists {
                    return Err(Error::InvalidDestination(valid_dest.clone()));
                }

                Ok(())
            }
        }
    }

    /// Remove all expired internal payments from the store
    /// Returns the number of expired payments removed
    pub async fn remove_expired_payments(&self) -> Result<usize, DbError> {
        // List all keys in the namespace
        let keys = self
            .inner
            .kv_store
            .kv_list(
                CLOSED_LOOP_PRIMARY_NAMESPACE,
                CLOSED_LOOP_SECONDARY_NAMESPACE,
            )
            .await?;

        let mut expired_keys = Vec::new();

        // Check each payment for expiry
        for key in keys {
            let value = self
                .inner
                .kv_store
                .kv_read(
                    CLOSED_LOOP_PRIMARY_NAMESPACE,
                    CLOSED_LOOP_SECONDARY_NAMESPACE,
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
            let mut txn = self.inner.kv_store.begin_transaction().await?;
            for key in expired_keys {
                txn.kv_remove(
                    CLOSED_LOOP_PRIMARY_NAMESPACE,
                    CLOSED_LOOP_SECONDARY_NAMESPACE,
                    &key,
                )
                .await?;
            }
            Box::new(txn).commit().await?;
        }

        Ok(count)
    }
}

async fn run_expired_payment_cleanup(manager: &ClosedLoopManager) {
    match manager.remove_expired_payments().await {
        Ok(count) => {
            if count > 0 {
                tracing::debug!(count, "Removed expired closed loop payments");
            }
        }
        Err(error) => {
            tracing::warn!(
                error = ?error,
                "Failed to remove expired closed loop payments"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use cdk_common::database::mint::{
        DbTransactionFinalizer, KVStore, KVStoreDatabase, KVStoreTransaction,
    };
    use tokio::sync::Mutex;

    use super::*;

    #[test]
    fn test_payment_data_creation() {
        let data = PaymentData::new(4600);
        assert_eq!(data.expires_at, 4600);
    }

    #[test]
    fn test_payment_data_expiry_check() {
        // Create data that expires in the future
        let future_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        let data = PaymentData::new(future_time);
        assert!(!data.is_expired());

        // Create data that's already expired
        let past_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 100;
        let expired_data = PaymentData::new(past_time);
        assert!(expired_data.is_expired());
    }

    #[test]
    fn test_payment_data_serialization() {
        let data = PaymentData::new(1234567890);
        let bytes: Vec<u8> = data.clone().into();

        assert_eq!(bytes.len(), 8);

        let decoded = PaymentData::try_from(bytes.as_slice()).unwrap();
        assert_eq!(decoded, data);
        assert_eq!(decoded.expires_at, 1234567890);
    }

    #[test]
    fn test_payment_data_invalid_bytes() {
        // Too short
        assert!(PaymentData::try_from(&[1, 2, 3][..]).is_err());

        // Empty
        assert!(PaymentData::try_from(&[][..]).is_err());

        // Valid length
        let data = PaymentData::try_from(&[0u8; 8][..]);
        assert!(data.is_ok());
    }

    // Mock KV store for testing
    #[derive(Debug, Default)]
    struct MockKVStore {
        data: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    }

    #[async_trait]
    impl KVStoreDatabase for MockKVStore {
        type Err = DbError;

        async fn kv_read(
            &self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
        ) -> Result<Option<Vec<u8>>, Self::Err> {
            let full_key = format!("{}:{}:{}", primary_namespace, secondary_namespace, key);
            let data = self.data.lock().await;
            Ok(data.get(&full_key).cloned())
        }

        async fn kv_list(
            &self,
            primary_namespace: &str,
            secondary_namespace: &str,
        ) -> Result<Vec<String>, Self::Err> {
            let prefix = format!("{}:{}:", primary_namespace, secondary_namespace);
            let data = self.data.lock().await;
            let keys: Vec<String> = data
                .keys()
                .filter_map(|k| {
                    if k.starts_with(&prefix) {
                        Some(k[prefix.len()..].to_string())
                    } else {
                        None
                    }
                })
                .collect();
            Ok(keys)
        }
    }

    struct MockKVTransaction {
        store: Arc<MockKVStore>,
        changes: HashMap<String, Option<Vec<u8>>>,
    }

    #[async_trait]
    impl<'a> KVStoreTransaction<'a, DbError> for MockKVTransaction {
        async fn kv_read(
            &mut self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
        ) -> Result<Option<Vec<u8>>, DbError> {
            self.store
                .kv_read(primary_namespace, secondary_namespace, key)
                .await
        }

        async fn kv_write(
            &mut self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
            value: &[u8],
        ) -> Result<(), DbError> {
            let full_key = format!("{}:{}:{}", primary_namespace, secondary_namespace, key);
            self.changes.insert(full_key, Some(value.to_vec()));
            Ok(())
        }

        async fn kv_remove(
            &mut self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
        ) -> Result<(), DbError> {
            let full_key = format!("{}:{}:{}", primary_namespace, secondary_namespace, key);
            self.changes.insert(full_key, None);
            Ok(())
        }

        async fn kv_list(
            &mut self,
            primary_namespace: &str,
            secondary_namespace: &str,
        ) -> Result<Vec<String>, DbError> {
            self.store
                .kv_list(primary_namespace, secondary_namespace)
                .await
        }
    }

    #[async_trait]
    impl DbTransactionFinalizer for MockKVTransaction {
        type Err = DbError;

        async fn commit(self: Box<Self>) -> Result<(), Self::Err> {
            let mut data = self.store.data.lock().await;
            for (key, value) in self.changes {
                match value {
                    Some(v) => {
                        data.insert(key, v);
                    }
                    None => {
                        data.remove(&key);
                    }
                }
            }
            Ok(())
        }

        async fn rollback(self: Box<Self>) -> Result<(), Self::Err> {
            Ok(())
        }
    }

    #[async_trait]
    impl KVStore for MockKVStore {
        async fn begin_transaction<'a>(
            &'a self,
        ) -> Result<Box<dyn KVStoreTransaction<'a, Self::Err> + Send + Sync + 'a>, DbError>
        {
            Ok(Box::new(MockKVTransaction {
                store: Arc::new(MockKVStore {
                    data: self.data.clone(),
                }),
                changes: HashMap::new(),
            }))
        }
    }

    fn create_mock_kv_store() -> DynMintKVStore {
        Arc::new(MockKVStore::default())
    }

    #[tokio::test]
    async fn test_register_payment() {
        let kv_store = create_mock_kv_store();
        let config = ClosedLoopConfig::internal("test-mint");
        let manager = ClosedLoopManager::new(kv_store.clone(), config)
            .await
            .unwrap();

        // Should write to the store
        let result = manager.register_payment("payment_123", 3600).await;
        assert!(result.is_ok());

        // Verify it was written to the store
        let value = kv_store
            .kv_read(
                CLOSED_LOOP_PRIMARY_NAMESPACE,
                CLOSED_LOOP_SECONDARY_NAMESPACE,
                "payment_123",
            )
            .await
            .unwrap();
        assert!(value.is_some());
    }

    #[tokio::test]
    async fn test_register_payment_with_custom_expiry() {
        let kv_store = create_mock_kv_store();
        let config = ClosedLoopConfig::internal("test-mint");
        let manager = ClosedLoopManager::new(kv_store.clone(), config)
            .await
            .unwrap();

        // Register payment with 60 second expiry
        let result = manager.register_payment("payment_456", 60).await;
        assert!(result.is_ok());

        // Verify it was written with expiry data
        let value = kv_store
            .kv_read(
                CLOSED_LOOP_PRIMARY_NAMESPACE,
                CLOSED_LOOP_SECONDARY_NAMESPACE,
                "payment_456",
            )
            .await
            .unwrap();
        assert!(value.is_some());
        let data = value.unwrap();
        assert_eq!(data.len(), 8); // 8 bytes expires_at timestamp

        // Payment should be valid now
        assert!(manager.validate_payment("payment_456", None).await.is_ok());
    }

    #[tokio::test]
    async fn test_register_payment_with_long_expiry() {
        let kv_store = create_mock_kv_store();
        let config = ClosedLoopConfig::internal("test-mint");
        let manager = ClosedLoopManager::new(kv_store.clone(), config)
            .await
            .unwrap();

        // Register payment with very long expiry (1 year)
        let one_year_secs = 365 * 24 * 60 * 60;
        let result = manager.register_payment("payment_789", one_year_secs).await;
        assert!(result.is_ok());

        // Verify it was written
        let value = kv_store
            .kv_read(
                CLOSED_LOOP_PRIMARY_NAMESPACE,
                CLOSED_LOOP_SECONDARY_NAMESPACE,
                "payment_789",
            )
            .await
            .unwrap();
        assert!(value.is_some());

        // Payment should be valid
        assert!(manager.validate_payment("payment_789", None).await.is_ok());
    }

    #[tokio::test]
    async fn test_remove_expired_payments() {
        let kv_store = create_mock_kv_store();
        let config = ClosedLoopConfig::internal("test-mint");
        let manager = ClosedLoopManager::new(kv_store.clone(), config)
            .await
            .unwrap();

        // Register payment with 0 second expiry (already expired)
        manager
            .register_payment("expired_payment", 0)
            .await
            .unwrap();

        // Register payment with long expiry
        manager
            .register_payment("valid_payment", 3600)
            .await
            .unwrap();

        // Wait a moment to ensure expiry (using real sleep since SystemTime is used)
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Remove expired payments
        let count = manager.remove_expired_payments().await.unwrap();
        assert_eq!(count, 1);

        // Expired payment should be gone
        assert!(manager
            .validate_payment("expired_payment", None)
            .await
            .is_err());

        // Valid payment should still exist
        assert!(manager
            .validate_payment("valid_payment", None)
            .await
            .is_ok());
    }
}
