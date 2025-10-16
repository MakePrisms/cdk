//! Payment synchronization logic for Square

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use cdk_common::util::hex;

use crate::error::Error;
use crate::types::{ListPaymentsParams, PaymentBrand};
use crate::util::{
    rfc3339_to_unix, unix_to_rfc3339, LAST_SYNC_TIME_KEY, SQUARE_KV_CONFIG_NAMESPACE,
    SQUARE_KV_PRIMARY_NAMESPACE,
};

/// Payment synchronization functionality
impl crate::client::Square {
    /// Sync all Square LIGHTNING payments to KV store
    ///
    /// Only syncs payments created after the last sync time to avoid re-processing all payments.
    ///
    /// This function ensures canonical syncing by:
    /// 1. Capturing the sync start time BEFORE querying the API
    /// 2. Using a 2-second overlap buffer to handle boundary cases
    /// 3. Batching all payment hash writes in a single transaction per page
    /// 4. Only updating the last sync time after ALL payments are successfully processed
    pub async fn sync_payments(&self) -> Result<(), Error> {
        use cdk_common::Bolt11Invoice;

        // Capture sync start time BEFORE any API calls to prevent race conditions
        let sync_start_time_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::SquareHttp(format!("System time error: {}", e)))?
            .as_secs();

        let sync_start_time_rfc3339 = unix_to_rfc3339(sync_start_time_secs);

        let last_sync_time = self.get_last_sync_time().await?;

        // Apply a 2-second overlap buffer to prevent missing payments at boundaries
        let query_begin_time = if let Some(ref last_sync) = last_sync_time {
            // Parse the last sync time and subtract 2 seconds for overlap
            // This ensures we don't miss payments due to timing precision or clock skew
            if let Some(last_sync_unix) = rfc3339_to_unix(last_sync) {
                let buffered_time = last_sync_unix.saturating_sub(2);
                Some(unix_to_rfc3339(buffered_time))
            } else {
                tracing::warn!("Failed to parse last sync time, syncing all payments");
                None
            }
        } else {
            None
        };

        let mut cursor: Option<String> = None;
        let mut synced_count = 0;
        let mut total_processed = 0;
        let mut skipped_duplicates = 0;

        // Paginate through all payments
        loop {
            let mut params = ListPaymentsParams::new().with_brand(PaymentBrand::Lightning);

            if let Some(ref begin_time) = query_begin_time {
                params = params.with_begin_time(begin_time.clone());
            }

            if let Some(ref cursor_value) = cursor {
                params = params.with_cursor(cursor_value.clone());
            }

            let response = self.list_payments(params).await?;

            let payments = &response.payments;

            if payments.is_empty() {
                break;
            }

            total_processed += payments.len();

            // Collect all payment hash writes for this page in a single transaction
            // This ensures atomicity - either all payments in the page are stored or none
            let mut payment_hashes_to_store: Vec<([u8; 32], String)> = Vec::new();

            // Process each LIGHTNING payment
            for payment in payments {
                // Skip CANCELED and FAILED payments - only track viable payments
                if payment.status == "CANCELED" || payment.status == "FAILED" {
                    continue;
                }

                // Extract wallet details (should exist since we filtered for LIGHTNING)
                let wallet_details = match &payment.wallet_details {
                    Some(details) => details,
                    None => {
                        return Err(Error::SquareHttp(format!(
                            "LIGHTNING payment {} missing wallet_details",
                            payment.id
                        )));
                    }
                };

                let lightning_details = match &wallet_details.lightning_details {
                    Some(details) => details,
                    None => {
                        return Err(Error::SquareHttp(format!(
                            "LIGHTNING payment {} missing lightning_details",
                            payment.id
                        )));
                    }
                };

                let payment_url = &lightning_details.payment_url;

                let bolt11_str = payment_url
                    .strip_prefix("lightning:")
                    .unwrap_or(payment_url)
                    .to_uppercase();

                match Bolt11Invoice::from_str(&bolt11_str) {
                    Ok(invoice) => {
                        let payment_hash = *invoice.payment_hash().as_ref();

                        // Check if we've already stored this payment (for overlap handling)
                        let key = format!(
                            "{}{}",
                            crate::util::INVOICE_HASH_PREFIX,
                            hex::encode(payment_hash)
                        );
                        let existing = self
                            .kv_store
                            .kv_read(
                                crate::util::SQUARE_KV_PRIMARY_NAMESPACE,
                                crate::util::SQUARE_KV_SECONDARY_NAMESPACE,
                                &key,
                            )
                            .await?;

                        if existing.is_some() {
                            skipped_duplicates += 1;
                            continue;
                        }

                        payment_hashes_to_store.push((payment_hash, payment.id.clone()));
                    }
                    Err(e) => {
                        return Err(Error::SquareHttp(format!(
                            "Failed to parse bolt11 invoice for payment {}: {}",
                            payment.id, e
                        )));
                    }
                }
            }

            // Store all payment hashes from this page in a single transaction
            if !payment_hashes_to_store.is_empty() {
                let mut tx = self.kv_store.begin_transaction().await?;

                for (payment_hash, payment_id) in &payment_hashes_to_store {
                    let key = format!(
                        "{}{}",
                        crate::util::INVOICE_HASH_PREFIX,
                        hex::encode(payment_hash)
                    );
                    tx.kv_write(
                        crate::util::SQUARE_KV_PRIMARY_NAMESPACE,
                        crate::util::SQUARE_KV_SECONDARY_NAMESPACE,
                        &key,
                        payment_id.as_bytes(),
                    )
                    .await?;
                }

                tx.commit().await?;
                synced_count += payment_hashes_to_store.len();
            }

            cursor = response.cursor;
            if cursor.is_none() {
                break; // No more pages
            }
        }

        if synced_count > 0 || skipped_duplicates > 0 {
            tracing::info!(
                "Square payment sync complete: {} new payments synced, {} duplicates skipped, {} total processed",
                synced_count,
                skipped_duplicates,
                total_processed
            );
        }

        self.store_last_sync_time(&sync_start_time_rfc3339).await?;

        self.remove_expired_payments().await?;

        Ok(())
    }

    /// Store last payment sync timestamp in KV store
    pub(crate) async fn store_last_sync_time(&self, timestamp: &str) -> Result<(), Error> {
        let mut tx = self.kv_store.begin_transaction().await?;
        tx.kv_write(
            SQUARE_KV_PRIMARY_NAMESPACE,
            SQUARE_KV_CONFIG_NAMESPACE,
            LAST_SYNC_TIME_KEY,
            timestamp.as_bytes(),
        )
        .await?;
        tx.commit().await?;

        Ok(())
    }

    /// Retrieve last payment sync timestamp from KV store
    pub(crate) async fn get_last_sync_time(&self) -> Result<Option<String>, Error> {
        let result = self
            .kv_store
            .kv_read(
                SQUARE_KV_PRIMARY_NAMESPACE,
                SQUARE_KV_CONFIG_NAMESPACE,
                LAST_SYNC_TIME_KEY,
            )
            .await?;

        Ok(result.map(|bytes| String::from_utf8_lossy(&bytes).to_string()))
    }
}
