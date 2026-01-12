//! Fee management and payout functionality
//!
//! This module provides functionality for calculating, tracking, and paying out fees
//! to lightning addresses when deposits are received.
//!
//! # Integration with Payment Backends
//!
//! Payment backends can integrate fee collection by:
//! 1. Creating a [`FeeConfig`] with a [`FeeCalculator`](super::calculator::FeeCalculator) implementation
//! 2. Passing a payment callback to [`FeeManager::new`]
//! 3. Registering pending fees when creating invoices
//! 4. Notifying the fee manager when invoices are paid
//!
//! The fee manager handles the rest, including LNURL resolution and automatic payouts.
//!
//! # Example Integration
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use cdk_agicash::{FeeConfig, FeeManager, FeePayoutBackend};
//!
//! // Your payment backend must implement FeePayoutBackend trait
//! let backend: Arc<dyn FeePayoutBackend> = Arc::new(my_payment_backend);
//!
//! let fee_config = FeeConfig::basis_points(
//!     300,                            // 3% fee
//!     10,                             // 10 sat minimum
//!     "operator@getalby.com".to_string(),
//!     "My Mint".to_string(),
//! );
//!
//! let fee_manager = FeeManager::new(kv_store, fee_config, backend)?;
//!
//! // When creating a mint quote:
//! let fee = fee_manager.calculate_fee(base_amount);
//! let total_amount = base_amount + fee;
//! // Create invoice for total_amount...
//! fee_manager.register_pending_fee(invoice_id, base_amount, fee, unit, expires_at).await?;
//!
//! // When the invoice is paid:
//! let base_amount = fee_manager.notify_invoice_paid(&invoice_id).await?;
//! // Report base_amount to the mint (payout happens automatically in background)
//! ```

use std::sync::Arc;

use cdk_common::database::mint::DynMintKVStore;
use cdk_common::database::Error as DbError;
use cdk_common::nuts::CurrencyUnit;

use super::backend::FeePayoutBackend;
use super::calculator::{BasisPointsFeeCalculator, FeeCalculator};
use super::error::FeeError;
use super::payout::{
    FeePayoutState, PendingFeePayout, CLEANUP_PERIOD, FEE_PRIMARY_NAMESPACE,
    FEE_SECONDARY_NAMESPACE,
};
use crate::lnurl::resolve_lightning_address;
use crate::supervisor::PeriodicSupervisor;

/// Fee configuration
///
/// Contains the fee calculator and payout configuration (lightning address).
/// This is the main configuration type that should be used when setting up fees.
#[derive(Clone)]
pub struct FeeConfig {
    /// The fee calculator implementation
    calculator: Arc<dyn FeeCalculator>,
    /// Lightning address where fees should be paid out
    lightning_address: String,
    /// Name of the mint for payment comments
    mint_name: String,
}

impl FeeConfig {
    /// Create a new fee configuration
    pub fn new(
        calculator: Arc<dyn FeeCalculator>,
        lightning_address: String,
        mint_name: String,
    ) -> Self {
        Self {
            calculator,
            lightning_address,
            mint_name,
        }
    }

    /// Create a fee configuration with basis points calculator
    pub fn basis_points(
        basis_points: u64,
        minimum_fee: u64,
        lightning_address: String,
        mint_name: String,
    ) -> Self {
        Self::new(
            Arc::new(BasisPointsFeeCalculator::new(basis_points, minimum_fee)),
            lightning_address,
            mint_name,
        )
    }

    /// Get the fee calculator
    pub fn calculator(&self) -> &Arc<dyn FeeCalculator> {
        &self.calculator
    }

    /// Get the lightning address
    pub fn lightning_address(&self) -> &str {
        &self.lightning_address
    }

    /// Get the mint name
    pub fn mint_name(&self) -> &str {
        &self.mint_name
    }
}

/// Fee manager for handling fee calculation, tracking, and payouts
#[derive(Clone)]
pub struct FeeManager {
    inner: Arc<FeeManagerInner>,
}

struct FeeManagerInner {
    kv_store: DynMintKVStore,
    calculator: Arc<dyn FeeCalculator>,
    lightning_address: String,
    mint_name: String,
    payment_backend: Arc<dyn FeePayoutBackend>,
    cleanup: PeriodicSupervisor,
}

impl FeeManager {
    /// Create a new fee manager with a payment backend
    ///
    /// The payment backend will be called to pay invoices when fees need to be paid out.
    pub fn new(
        kv_store: DynMintKVStore,
        config: FeeConfig,
        payment_backend: Arc<dyn FeePayoutBackend>,
    ) -> Result<Self, DbError> {
        let manager = Self {
            inner: Arc::new(FeeManagerInner {
                kv_store,
                calculator: config.calculator().clone(),
                lightning_address: config.lightning_address().to_string(),
                mint_name: config.mint_name().to_string(),
                payment_backend,
                cleanup: PeriodicSupervisor::new(),
            }),
        };

        // Start cleanup task with weak reference to avoid circular ownership
        let weak_inner = Arc::downgrade(&manager.inner);
        manager.inner.cleanup.start(CLEANUP_PERIOD, move || {
            let weak_inner = weak_inner.clone();
            async move {
                if let Some(inner) = weak_inner.upgrade() {
                    let manager = FeeManager { inner };
                    run_fee_payout_cleanup(&manager).await;
                }
            }
        });

        Ok(manager)
    }

    /// Calculate fee for an amount (returns 0 if no fee configured)
    pub fn calculate_fee(&self, amount: u64) -> u64 {
        self.inner.calculator.calculate_fee(amount)
    }

    /// Register a pending fee for later payout
    ///
    /// Stores the fee information in the KV store to be paid out when the invoice is settled.
    pub async fn register_pending_fee(
        &self,
        invoice_id: impl Into<String>,
        base_amount: u64,
        fee_amount: u64,
        unit: CurrencyUnit,
        expires_at: u64,
    ) -> Result<(), DbError> {
        let invoice_id = invoice_id.into();
        let created_at = cdk_common::util::unix_time();

        let pending_fee = PendingFeePayout::new(
            invoice_id.clone(),
            base_amount,
            fee_amount,
            unit.clone(),
            created_at,
            expires_at,
        );

        let data: Vec<u8> = pending_fee.into();
        let mut txn = self.inner.kv_store.begin_transaction().await?;
        txn.kv_write(
            FEE_PRIMARY_NAMESPACE,
            FEE_SECONDARY_NAMESPACE,
            &invoice_id,
            &data,
        )
        .await?;
        Box::new(txn).commit().await?;

        Ok(())
    }

    /// Notify that an invoice was paid and trigger fee payout
    ///
    /// Returns the base amount if a pending fee exists, otherwise returns None.
    /// Triggers the payout in the background using the configured callback.
    pub async fn notify_invoice_paid(
        &self,
        invoice_id: impl AsRef<str>,
    ) -> Result<Option<u64>, FeeError> {
        let invoice_id = invoice_id.as_ref();

        let pending_fee = match self.get_pending_fee(invoice_id).await? {
            Some(fee) => fee,
            None => return Ok(None),
        };

        // Check if already paid or in progress
        match pending_fee.state {
            FeePayoutState::Paid | FeePayoutState::Pending => {
                return Ok(Some(pending_fee.base_amount));
            }
            FeePayoutState::Failed(ref e) => {
                tracing::warn!(
                    "Fee for invoice {} previously failed with error: {}. Retrying...",
                    invoice_id,
                    e
                );
            }
            FeePayoutState::Unpaid => {}
        }

        let base_amount = pending_fee.base_amount;
        let payout = pending_fee.clone();

        // Update state to Pending before processing
        self.update_payout_state(invoice_id, FeePayoutState::Pending)
            .await?;

        // Spawn background task to process payout
        let manager = self.clone();

        tokio::spawn(async move {
            manager.execute_payout_flow(payout).await;
        });

        Ok(Some(base_amount))
    }

    /// Execute the payout flow (resolve address, pay, update state)
    async fn execute_payout_flow(&self, payout: PendingFeePayout) {
        let lightning_address = &self.inner.lightning_address;
        let invoice_id = payout.invoice_id.clone();
        let fee_amount = payout.fee_amount;

        let payout_result = async {
            let amount_msats = match payout.unit {
                CurrencyUnit::Sat => fee_amount * 1000,
                CurrencyUnit::Msat => fee_amount,
                _ => return Err(FeeError::UnsupportedUnit(payout.unit.clone())),
            };

            let comment = self.format_comment(&payout);
            let bolt11 =
                resolve_lightning_address(lightning_address, amount_msats, Some(&comment)).await?;

            let payment_id = self.inner.payment_backend.pay_invoice(bolt11).await?;

            Ok::<String, FeeError>(payment_id)
        }
        .await;

        match payout_result {
            Ok(_) => {
                if let Err(e) = self
                    .update_payout_state(&invoice_id, FeePayoutState::Paid)
                    .await
                {
                    tracing::error!(
                        "CRITICAL: Failed to update payout state to Paid for invoice {}: {}. Payment WAS made but state is inconsistent. Manual resolution required.",
                        invoice_id,
                        e
                    );
                }
            }
            Err(e) => {
                let error_msg = format!("{}", e);
                tracing::error!(
                    "Fee payout failed for invoice {}: {}. Lightning address: {}, Amount: {}",
                    invoice_id,
                    error_msg,
                    lightning_address,
                    fee_amount
                );

                if let Err(e) = self
                    .update_payout_state(&invoice_id, FeePayoutState::Failed(error_msg))
                    .await
                {
                    tracing::error!(
                        "CRITICAL: Failed to update payout state to Failed for invoice {}: {}. Manual resolution required.",
                        invoice_id,
                        e
                    );
                }
            }
        }
    }

    /// Get a pending fee payout from the KV store
    async fn get_pending_fee(&self, invoice_id: &str) -> Result<Option<PendingFeePayout>, DbError> {
        let data = self
            .inner
            .kv_store
            .kv_read(FEE_PRIMARY_NAMESPACE, FEE_SECONDARY_NAMESPACE, invoice_id)
            .await?;

        match data {
            Some(bytes) => {
                let pending_fee = PendingFeePayout::try_from(bytes.as_slice())?;
                Ok(Some(pending_fee))
            }
            None => Ok(None),
        }
    }

    /// Update the state of a pending fee payout
    async fn update_payout_state(
        &self,
        invoice_id: &str,
        state: FeePayoutState,
    ) -> Result<(), DbError> {
        let mut pending_fee = match self.get_pending_fee(invoice_id).await? {
            Some(fee) => fee,
            None => {
                tracing::warn!(
                    "Cannot update payout state for invoice_id={}: no pending fee found",
                    invoice_id
                );
                return Ok(());
            }
        };

        pending_fee.state = state;
        pending_fee.updated_at = cdk_common::util::unix_time();
        let data: Vec<u8> = pending_fee.into();

        let mut txn = self.inner.kv_store.begin_transaction().await?;
        txn.kv_write(
            FEE_PRIMARY_NAMESPACE,
            FEE_SECONDARY_NAMESPACE,
            invoice_id,
            &data,
        )
        .await?;
        Box::new(txn).commit().await?;

        Ok(())
    }

    /// Get all pending fee payouts from the KV store
    async fn get_all_pending_fees(&self) -> Result<Vec<PendingFeePayout>, DbError> {
        let keys = self
            .inner
            .kv_store
            .kv_list(FEE_PRIMARY_NAMESPACE, FEE_SECONDARY_NAMESPACE)
            .await?;

        let mut payouts = Vec::new();
        for key in keys.iter() {
            if let Some(data) = self
                .inner
                .kv_store
                .kv_read(FEE_PRIMARY_NAMESPACE, FEE_SECONDARY_NAMESPACE, key)
                .await?
            {
                if let Ok(payout) = PendingFeePayout::try_from(data.as_slice()) {
                    payouts.push(payout);
                }
            }
        }

        Ok(payouts)
    }

    /// Remove a fee payout from the KV store
    async fn remove_payout(&self, invoice_id: &str) -> Result<(), DbError> {
        let mut txn = self.inner.kv_store.begin_transaction().await?;
        txn.kv_remove(FEE_PRIMARY_NAMESPACE, FEE_SECONDARY_NAMESPACE, invoice_id)
            .await?;
        Box::new(txn).commit().await?;

        Ok(())
    }
    /// Format a payment comment for a deposit
    /// The output is formatted for CSV and Excel parsing.
    fn format_comment(&self, payout: &PendingFeePayout) -> String {
        format!(
            "{},{},{},{},{}",
            self.inner.mint_name,
            payout.base_amount,
            payout.unit,
            payout.invoice_id,
            payout.created_at
        )
    }
}

/// Run cleanup of paid and expired unpaid fee payouts
///
/// Only removes:
/// - Paid payouts
/// - Expired unpaid payouts
///
/// Pending and Failed payouts are left for manual resolution.
async fn run_fee_payout_cleanup(manager: &FeeManager) {
    let current_time = cdk_common::util::unix_time();

    match manager.get_all_pending_fees().await {
        Ok(payouts) => {
            for payout in payouts {
                let should_remove = match payout.state {
                    FeePayoutState::Paid => true,
                    FeePayoutState::Unpaid if payout.expires_at < current_time => true,
                    FeePayoutState::Unpaid => false,
                    FeePayoutState::Pending => false,
                    FeePayoutState::Failed(_) => false,
                };

                if should_remove {
                    if let Err(e) = manager.remove_payout(&payout.invoice_id).await {
                        tracing::warn!(
                            "Failed to remove payout: invoice_id={}, error={}",
                            payout.invoice_id,
                            e
                        );
                    }
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to get pending fees for cleanup: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use cdk_common::database::mint::{
        DbTransactionFinalizer, KVStore, KVStoreDatabase, KVStoreTransaction,
    };

    use super::*;

    // Mock FeePayoutBackend
    struct MockFeePayoutBackend;

    impl MockFeePayoutBackend {
        fn new() -> Self {
            Self
        }
    }

    #[async_trait]
    impl FeePayoutBackend for MockFeePayoutBackend {
        async fn pay_invoice(&self, _bolt11: String) -> Result<String, FeeError> {
            Ok("payment_id".to_string())
        }
    }

    // Mock KVStore
    #[derive(Clone)]
    struct MockMintKVStore {
        data: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    }

    impl MockMintKVStore {
        fn new() -> Self {
            Self {
                data: Arc::new(Mutex::new(HashMap::new())),
            }
        }
    }

    #[async_trait]
    impl KVStoreDatabase for MockMintKVStore {
        type Err = DbError;

        async fn kv_read(
            &self,
            _primary_namespace: &str,
            _secondary_namespace: &str,
            key: &str,
        ) -> Result<Option<Vec<u8>>, Self::Err> {
            let data = self.data.lock().unwrap();
            Ok(data.get(key).cloned())
        }

        async fn kv_list(
            &self,
            _primary_namespace: &str,
            _secondary_namespace: &str,
        ) -> Result<Vec<String>, Self::Err> {
            let data = self.data.lock().unwrap();
            Ok(data.keys().cloned().collect())
        }
    }

    #[async_trait]
    impl KVStore for MockMintKVStore {
        async fn begin_transaction<'a>(
            &'a self,
        ) -> Result<Box<dyn KVStoreTransaction<'a, Self::Err> + Send + Sync + 'a>, DbError>
        {
            Ok(Box::new(MockMintKVStoreTransaction {
                store: self,
                pending_writes: HashMap::new(),
            }))
        }
    }

    struct MockMintKVStoreTransaction<'a> {
        store: &'a MockMintKVStore,
        pending_writes: HashMap<String, Vec<u8>>,
    }

    #[async_trait]
    impl<'a> DbTransactionFinalizer for MockMintKVStoreTransaction<'a> {
        type Err = DbError;

        async fn commit(self: Box<Self>) -> Result<(), Self::Err> {
            let mut data = self.store.data.lock().unwrap();
            for (key, value) in self.pending_writes {
                data.insert(key, value);
            }
            Ok(())
        }

        async fn rollback(self: Box<Self>) -> Result<(), Self::Err> {
            Ok(())
        }
    }

    #[async_trait]
    impl<'a> KVStoreTransaction<'a, DbError> for MockMintKVStoreTransaction<'a> {
        async fn kv_read(
            &mut self,
            _primary_namespace: &str,
            _secondary_namespace: &str,
            key: &str,
        ) -> Result<Option<Vec<u8>>, DbError> {
            if let Some(value) = self.pending_writes.get(key) {
                return Ok(Some(value.clone()));
            }
            let data = self.store.data.lock().unwrap();
            Ok(data.get(key).cloned())
        }

        async fn kv_write(
            &mut self,
            _primary_namespace: &str,
            _secondary_namespace: &str,
            key: &str,
            value: &[u8],
        ) -> Result<(), DbError> {
            self.pending_writes.insert(key.to_string(), value.to_vec());
            Ok(())
        }

        async fn kv_remove(
            &mut self,
            _primary_namespace: &str,
            _secondary_namespace: &str,
            _key: &str,
        ) -> Result<(), DbError> {
            // For simplicity in this mock, we just write empty or handle it if needed.
            // But for format_comment test we don't need this.
            // To be correct we should track removals.
            // For now, let's just ignore or panic if used, but format_comment doesn't use it.
            Ok(())
        }

        async fn kv_list(
            &mut self,
            _primary_namespace: &str,
            _secondary_namespace: &str,
        ) -> Result<Vec<String>, DbError> {
            Ok(vec![])
        }
    }

    #[test]
    fn test_fee_config_creation() {
        let config = FeeConfig::basis_points(
            300,
            10,
            "test@example.com".to_string(),
            "Test Mint".to_string(),
        );

        assert_eq!(config.lightning_address(), "test@example.com");
        assert_eq!(config.mint_name(), "Test Mint");
        assert_eq!(config.calculator().calculate_fee(10000), 300);
    }

    #[tokio::test]
    async fn test_format_comment() {
        let config = FeeConfig::basis_points(
            300,
            10,
            "test@example.com".to_string(),
            "Test Mint".to_string(),
        );

        let kv_store = Arc::new(MockMintKVStore::new());
        let backend = Arc::new(MockFeePayoutBackend::new());

        let manager = FeeManager::new(kv_store, config, backend).unwrap();

        let payout = PendingFeePayout::new(
            "invoice_123".to_string(),
            1000,
            30,
            CurrencyUnit::Sat,
            1234567890,
            1234571490,
        );

        let comment = manager.format_comment(&payout);

        assert_eq!(comment, "Test Mint; 1000; sat; invoice_123; 1234567890");
    }
}
