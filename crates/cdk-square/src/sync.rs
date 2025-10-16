//! Payment synchronization logic for Square

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Error;
use crate::types::{ListPaymentsParams, PaymentBrand};
use crate::util::{
    unix_to_rfc3339, LAST_SYNC_TIME_KEY, SQUARE_KV_CONFIG_NAMESPACE, SQUARE_KV_PRIMARY_NAMESPACE,
};

/// Payment synchronization functionality
impl crate::client::Square {
    /// Sync all Square LIGHTNING payments to KV store
    ///
    /// Only syncs payments created after the last sync time to avoid re-processing all payments.
    pub async fn sync_payments(&self) -> Result<(), Error> {
        use cdk_common::Bolt11Invoice;

        let last_sync_time = self.get_last_sync_time().await?;

        let current_time_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::SquareHttp(format!("System time error: {}", e)))?
            .as_secs();

        let current_time_rfc3339 = unix_to_rfc3339(current_time_secs);

        let mut cursor: Option<String> = None;
        let mut synced_count = 0;
        let mut total_processed = 0;

        // Paginate through all payments
        loop {
            let mut params = ListPaymentsParams::new().with_brand(PaymentBrand::Lightning);

            if let Some(ref last_sync) = last_sync_time {
                params = params.with_begin_time(last_sync.clone());
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
                        let payment_hash = invoice.payment_hash().as_ref();
                        self.store_invoice_hash(payment_hash, &payment.id).await?;
                        synced_count += 1;
                    }
                    Err(e) => {
                        return Err(Error::SquareHttp(format!(
                            "Failed to parse bolt11 invoice for payment {}: {}",
                            payment.id, e
                        )));
                    }
                }
            }

            cursor = response.cursor;
            if cursor.is_none() {
                break; // No more pages
            }
        }

        if synced_count > 0 {
            tracing::info!(
                "Square payment sync complete: {} LIGHTNING payments synced out of {} total payments processed",
                synced_count,
                total_processed
            );
        }

        self.store_last_sync_time(&current_time_rfc3339).await?;

        tracing::debug!(
            "Updated last Square payment sync time to: {}",
            current_time_rfc3339
        );

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
