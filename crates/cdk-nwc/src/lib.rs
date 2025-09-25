//! CDK NWC LN Backend
//!
//! Used for connecting to a Nostr Wallet Connect (NWC) enabled wallet
//! to send and receive payments.
//!
//! The wallet uses NWC notifications to stream payment updates to the mint.

#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]

use std::cmp::max;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bitcoin::hashes::sha256::Hash;
use cdk_common::amount::{to_unit, Amount};
use cdk_common::common::FeeReserve;
use cdk_common::nuts::{CurrencyUnit, MeltOptions, MeltQuoteState};
use cdk_common::payment::{
    self, Bolt11Settings, CreateIncomingPaymentResponse, Event, IncomingPaymentOptions,
    MakePaymentResponse, MintPayment, OutgoingPaymentOptions, PaymentIdentifier,
    PaymentQuoteResponse, WaitPaymentResponse,
};
use cdk_common::util::hex;
use cdk_common::Bolt11Invoice;
use error::Error;
use futures::stream::StreamExt;
use futures::Stream;
use nwc::prelude::*;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

pub mod error;

/// Default validation timeout in seconds
const VALIDATION_TIMEOUT_SECS: u64 = 15;

/// NWC Wallet Backend  
#[derive(Clone)]
pub struct NWCWallet {
    /// NWC client
    nwc_client: Arc<NWC>,
    /// Fee reserve configuration
    fee_reserve: FeeReserve,
    /// Channel sender for payment notifications
    sender: tokio::sync::broadcast::Sender<WaitPaymentResponse>,
    /// Channel receiver for payment notifications  
    receiver: Arc<tokio::sync::broadcast::Receiver<WaitPaymentResponse>>,
    /// Cancellation token for wait invoice
    wait_invoice_cancel_token: CancellationToken,
    /// Flag indicating if wait invoice is active
    wait_invoice_is_active: Arc<AtomicBool>,
    /// Currency unit
    unit: CurrencyUnit,
    /// Notification handler task handle
    notification_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl NWCWallet {
    /// Create new [`NWCWallet`] from NWC URI string
    pub async fn new(
        nwc_uri: &str,
        fee_reserve: FeeReserve,
        unit: CurrencyUnit,
        internal_settlement_only: bool,
    ) -> Result<Self, Error> {
        // NWC requires TLS for talking to the relay
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }

        let uri = NostrWalletConnectURI::from_str(nwc_uri)
            .map_err(|e| Error::InvalidUri(e.to_string()))?;

        let nwc_client = Arc::new(NWC::new(uri));

        let mut required_methods = vec![
            "make_invoice",
            "lookup_invoice",
            "list_transactions",
            "get_info",
        ];

        if !internal_settlement_only {
            required_methods.push("pay_invoice");
        }

        let required_notifications = &["payment_received"];

        NWCWallet::validate_supported_methods_and_notifications(
            &nwc_client,
            VALIDATION_TIMEOUT_SECS,
            &required_methods,
            required_notifications,
        )
        .await?;

        let (sender, receiver) = tokio::sync::broadcast::channel(100);

        let wallet = Self {
            nwc_client,
            fee_reserve,
            sender,
            receiver: Arc::new(receiver),
            wait_invoice_cancel_token: CancellationToken::new(),
            wait_invoice_is_active: Arc::new(AtomicBool::new(false)),
            unit,
            notification_handle: Arc::new(Mutex::new(None)),
        };

        // Start notification handler
        wallet.start_notification_handler().await?;

        Ok(wallet)
    }

    /// Start the notification handler for payment updates
    async fn start_notification_handler(&self) -> Result<(), Error> {
        let nwc_client = self.nwc_client.clone();
        let sender = self.sender.clone();

        let unit = self.unit.clone();
        let handle = tokio::spawn(async move {
            tracing::info!("NWC: Starting notification handler");

            if let Err(e) = nwc_client.subscribe_to_notifications().await {
                tracing::error!("NWC: Failed to subscribe to notifications: {}", e);
                return;
            }

            let result = nwc_client
                .handle_notifications(|notification| {
                    let sender = sender.clone();
                    let unit = unit.clone();

                    async move {
                        match notification.notification_type {
                            NotificationType::PaymentReceived => {
                                if let Ok(payment) = notification.to_pay_notification() {
                                    tracing::debug!(
                                        "NWC: Payment received: {:?}",
                                        payment.payment_hash
                                    );

                                    let payment_hash = match Hash::from_str(&payment.payment_hash) {
                                        Ok(hash) => hash,
                                        Err(e) => {
                                            tracing::error!(
                                                "NWC: Failed to parse payment hash: {}",
                                                e
                                            );
                                            return Ok(false);
                                        }
                                    };

                                    let payment_id =
                                        PaymentIdentifier::PaymentHash(*payment_hash.as_ref());

                                    // nwc amounts are in msat, convert to target unit
                                    let amount =
                                        to_unit(payment.amount, &CurrencyUnit::Msat, &unit)?;

                                    let wait_payment_response = WaitPaymentResponse {
                                        payment_identifier: payment_id,
                                        payment_amount: amount,
                                        unit: unit.clone(),
                                        payment_id: payment.payment_hash,
                                    };

                                    if let Err(e) = sender.send(wait_payment_response) {
                                        tracing::error!(
                                            "NWC: Failed to send payment notification: {}",
                                            e
                                        );
                                        return Ok(true); // Exit the notification handler
                                    }
                                }
                            }
                            NotificationType::PaymentSent => {
                                // We don't need to handle payment sent notifications
                                // Status can be checked via lookup_invoice when needed
                            }
                        }
                        Ok(false) // Continue processing
                    }
                })
                .await;

            match result {
                Ok(_) => {
                    tracing::info!("NWC: Notification handler completed normally");
                }
                Err(e) => {
                    tracing::error!("NWC: Notification handler failed: {}", e);
                }
            }
        });

        let mut notification_handle = self.notification_handle.lock().await;
        *notification_handle = Some(handle);

        Ok(())
    }

    /// Check if outgoing payment is already paid.
    async fn check_outgoing_unpaid(
        &self,
        payment_identifier: &PaymentIdentifier,
    ) -> Result<(), payment::Error> {
        let pay_state = self.check_outgoing_payment(payment_identifier).await?;

        match pay_state.status {
            MeltQuoteState::Unpaid | MeltQuoteState::Unknown | MeltQuoteState::Failed => Ok(()),
            MeltQuoteState::Paid => {
                tracing::debug!("NWC: Melt attempted on invoice already paid");
                Err(payment::Error::InvoiceAlreadyPaid)
            }
            MeltQuoteState::Pending => {
                tracing::debug!("NWC: Melt attempted on invoice already pending");
                Err(payment::Error::InvoicePaymentPending)
            }
        }
    }
}

#[async_trait]
impl MintPayment for NWCWallet {
    type Err = payment::Error;

    #[instrument(skip_all)]
    async fn get_settings(&self) -> Result<Value, Self::Err> {
        Ok(serde_json::to_value(Bolt11Settings {
            mpp: false,
            unit: self.unit.clone(),
            invoice_description: true,
            amountless: true,
            bolt12: false,
        })?)
    }

    #[instrument(skip_all)]
    fn is_wait_invoice_active(&self) -> bool {
        self.wait_invoice_is_active.load(Ordering::SeqCst)
    }

    #[instrument(skip_all)]
    fn cancel_wait_invoice(&self) {
        self.wait_invoice_cancel_token.cancel()
    }

    #[instrument(skip_all)]
    async fn wait_payment_event(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Event> + Send>>, Self::Err> {
        tracing::info!("NWC: Starting stream for payment notifications");

        self.wait_invoice_is_active.store(true, Ordering::SeqCst);

        let receiver = self.receiver.clone();
        let receiver_stream = BroadcastStream::new(receiver.resubscribe());

        // Map the stream to handle BroadcastStreamRecvError and wrap in Event
        let response_stream = receiver_stream.filter_map(|result| async move {
            match result {
                Ok(payment) => Some(Event::PaymentReceived(payment)),
                Err(err) => {
                    tracing::warn!("Error in broadcast stream: {}", err);
                    None
                }
            }
        });

        Ok(Box::pin(response_stream))
    }

    #[instrument(skip_all)]
    async fn get_payment_quote(
        &self,
        unit: &CurrencyUnit,
        options: OutgoingPaymentOptions,
    ) -> Result<PaymentQuoteResponse, Self::Err> {
        let (amount_msat, request_lookup_id) = match options {
            OutgoingPaymentOptions::Bolt11(bolt11_options) => {
                let amount_msat: Amount = if let Some(melt_options) = bolt11_options.melt_options {
                    match melt_options {
                        MeltOptions::Amountless { amountless } => {
                            let amount_msat = amountless.amount_msat;

                            if let Some(invoice_amount) =
                                bolt11_options.bolt11.amount_milli_satoshis()
                            {
                                if invoice_amount != u64::from(amount_msat) {
                                    return Err(payment::Error::AmountMismatch);
                                }
                            }
                            amount_msat
                        }
                        MeltOptions::Mpp { mpp } => mpp.amount,
                    }
                } else {
                    bolt11_options
                        .bolt11
                        .amount_milli_satoshis()
                        .ok_or_else(|| Error::UnknownInvoiceAmount)?
                        .into()
                };

                let payment_id =
                    PaymentIdentifier::PaymentHash(*bolt11_options.bolt11.payment_hash().as_ref());
                (amount_msat, Some(payment_id))
            }
            OutgoingPaymentOptions::Bolt12(_) => {
                return Err(payment::Error::UnsupportedUnit);
            }
        };

        let amount = to_unit(amount_msat, &CurrencyUnit::Msat, unit)?;

        let relative_fee_reserve =
            (self.fee_reserve.percent_fee_reserve * u64::from(amount) as f32) as u64;
        let absolute_fee_reserve: u64 = self.fee_reserve.min_fee_reserve.into();
        let fee = max(relative_fee_reserve, absolute_fee_reserve);

        Ok(PaymentQuoteResponse {
            request_lookup_id,
            amount,
            fee: fee.into(),
            state: MeltQuoteState::Unpaid,
            unit: unit.clone(),
        })
    }

    #[instrument(skip_all)]
    async fn make_payment(
        &self,
        unit: &CurrencyUnit,
        options: OutgoingPaymentOptions,
    ) -> Result<MakePaymentResponse, Self::Err> {
        match options {
            OutgoingPaymentOptions::Bolt11(bolt11_options) => {
                let bolt11 = bolt11_options.bolt11;
                let payment_identifier =
                    PaymentIdentifier::PaymentHash(*bolt11.payment_hash().as_ref());

                self.check_outgoing_unpaid(&payment_identifier).await?;

                // Determine the amount to pay
                let amount_msat: u64 = if let Some(melt_options) = bolt11_options.melt_options {
                    melt_options.amount_msat().into()
                } else {
                    bolt11
                        .amount_milli_satoshis()
                        .ok_or_else(|| Error::UnknownInvoiceAmount)?
                };

                // Create pay invoice request with amount for amountless invoices
                let mut request = PayInvoiceRequest::new(bolt11.to_string());

                // If the invoice is amountless, set the amount
                if bolt11.amount_milli_satoshis().is_none() {
                    request.amount = Some(amount_msat);
                }

                // Make payment through NWC
                let response = self.nwc_client.pay_invoice(request).await.map_err(|e| {
                    tracing::error!("NWC payment failed: {}", e);
                    payment::Error::Lightning(Box::new(e))
                })?;

                let total_spent = to_unit(amount_msat, &CurrencyUnit::Msat, unit)?;
                let fee_paid = if let Some(fees) = response.fees_paid {
                    to_unit(fees, &CurrencyUnit::Msat, unit)?
                } else {
                    Amount::ZERO
                };

                Ok(MakePaymentResponse {
                    payment_proof: Some(response.preimage),
                    payment_lookup_id: payment_identifier,
                    status: MeltQuoteState::Paid,
                    total_spent: total_spent + fee_paid,
                    unit: unit.clone(),
                })
            }
            OutgoingPaymentOptions::Bolt12(_) => Err(payment::Error::UnsupportedUnit),
        }
    }

    #[instrument(skip_all)]
    async fn create_incoming_payment_request(
        &self,
        unit: &CurrencyUnit,
        options: IncomingPaymentOptions,
    ) -> Result<CreateIncomingPaymentResponse, Self::Err> {
        match options {
            IncomingPaymentOptions::Bolt11(bolt11_options) => {
                let description = bolt11_options.description.unwrap_or_default();
                let amount = bolt11_options.amount;
                let expiry = bolt11_options.unix_expiry;

                if amount == Amount::ZERO {
                    return Err(payment::Error::Custom(
                        "NWC requires invoice amount".to_string(),
                    ));
                }

                // Convert amount to millisatoshis
                let amount_msat = to_unit(amount, unit, &CurrencyUnit::Msat)?.into();

                let request = MakeInvoiceRequest {
                    amount: amount_msat,
                    description: if description.is_empty() {
                        None
                    } else {
                        Some(description)
                    },
                    description_hash: None,
                    expiry: None, // Expiry from bolt11_options is too long for NWC
                };

                let response = self.nwc_client.make_invoice(request).await.map_err(|e| {
                    tracing::error!("NWC create invoice failed: {}", e);
                    payment::Error::Lightning(Box::new(e))
                })?;

                let payment_hash = *Bolt11Invoice::from_str(&response.invoice)?
                    .payment_hash()
                    .as_ref();

                Ok(CreateIncomingPaymentResponse {
                    request_lookup_id: PaymentIdentifier::PaymentHash(payment_hash),
                    request: response.invoice,
                    expiry,
                })
            }
            IncomingPaymentOptions::Bolt12(_) => Err(payment::Error::UnsupportedUnit),
        }
    }

    #[instrument(skip_all)]
    async fn check_incoming_payment_status(
        &self,
        request_lookup_id: &PaymentIdentifier,
    ) -> Result<Vec<WaitPaymentResponse>, Self::Err> {
        // Handle only PaymentHash identifiers
        let payment_hash = match request_lookup_id {
            PaymentIdentifier::PaymentHash(hash) => hash,
            _ => {
                tracing::error!(
                    "NWC: Unsupported payment identifier type for check_incoming_payment_status"
                );
                return Err(payment::Error::UnknownPaymentState);
            }
        };

        let payment_hash_str = hex::encode(payment_hash);
        let lookup_request = LookupInvoiceRequest {
            payment_hash: Some(payment_hash_str),
            invoice: None,
        };

        let lookup_response = match self.nwc_client.lookup_invoice(lookup_request).await {
            Ok(invoice) => invoice,
            Err(_) => return Ok(vec![]), // Invoice not found
        };

        if !matches!(
            lookup_response.transaction_type,
            Some(TransactionType::Incoming)
        ) {
            // Not an incoming payment
            return Ok(vec![]);
        }

        if lookup_response.settled_at.is_none() {
            // Not settled
            return Ok(vec![]);
        }

        let response = WaitPaymentResponse {
            payment_identifier: request_lookup_id.clone(),
            payment_amount: to_unit(lookup_response.amount, &CurrencyUnit::Msat, &self.unit)?,
            unit: self.unit.clone(),
            payment_id: lookup_response.payment_hash,
        };

        Ok(vec![response])
    }

    #[instrument(skip_all)]
    async fn check_outgoing_payment(
        &self,
        request_lookup_id: &PaymentIdentifier,
    ) -> Result<MakePaymentResponse, Self::Err> {
        let payment_hash = match request_lookup_id {
            PaymentIdentifier::PaymentHash(hash) => hash,
            _ => {
                tracing::error!(
                    "NWC: Unsupported payment identifier type for check_outgoing_payment"
                );
                return Err(payment::Error::UnknownPaymentState);
            }
        };

        let payment_hash_str = hex::encode(payment_hash);
        let lookup_request = LookupInvoiceRequest {
            payment_hash: Some(payment_hash_str),
            invoice: None,
        };

        let lookup_response = match self.nwc_client.lookup_invoice(lookup_request).await {
            Ok(invoice) => invoice,
            Err(e) => {
                tracing::warn!("NWC: Failed to lookup payment: {}", e);
                return Ok(MakePaymentResponse {
                    payment_proof: None,
                    payment_lookup_id: request_lookup_id.clone(),
                    status: MeltQuoteState::Unknown,
                    total_spent: Amount::ZERO,
                    unit: self.unit.clone(),
                });
            }
        };

        if !matches!(
            lookup_response.transaction_type,
            Some(TransactionType::Outgoing)
        ) {
            // Not an outgoing payment
            return Err(payment::Error::UnknownPaymentState);
        }

        let status = if lookup_response.settled_at.is_some() || lookup_response.preimage.is_some() {
            MeltQuoteState::Paid
        } else {
            MeltQuoteState::Pending
        };

        let total_spent = if status == MeltQuoteState::Paid {
            to_unit(
                lookup_response.amount + lookup_response.fees_paid,
                &CurrencyUnit::Msat,
                &self.unit,
            )?
        } else {
            Amount::ZERO
        };

        Ok(MakePaymentResponse {
            payment_proof: lookup_response.preimage,
            payment_lookup_id: request_lookup_id.clone(),
            status,
            total_spent,
            unit: self.unit.clone(),
        })
    }
}

impl NWCWallet {
    async fn validate_supported_methods_and_notifications(
        client: &NWC,
        timeout_secs: u64,
        required_methods: &[&str],
        required_notifications: &[&str],
    ) -> Result<(), Error> {
        // Try to get info normally first
        let (methods, notifications) = match tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            client.get_info(),
        )
        .await
        {
            Ok(Ok(info)) => {
                // Normal case: deserialization succeeded
                (info.methods, info.notifications)
            }
            Ok(Err(nwc_error)) => {
                // Check if this is a deserialization error due to malformed pubkey
                let error_string = nwc_error.to_string();
                if error_string.contains("malformed public key")
                    || error_string.contains("Can't deserialize response")
                {
                    // Try to extract methods and notifications manually from raw response
                    tracing::warn!("NWC get_info deserialization failed due to malformed pubkey, attempting manual parsing");
                    Self::parse_methods_and_notifications_from_error(&error_string)?
                } else {
                    return Err(Error::Nwc(nwc_error));
                }
            }
            Err(_) => return Err(Error::Connection("Timeout during validation".to_string())),
        };

        let missing_methods: Vec<&str> = required_methods
            .iter()
            .filter(|&method| !methods.contains(&method.to_string()))
            .copied()
            .collect();

        if !missing_methods.is_empty() {
            return Err(Error::UnsupportedMethods(missing_methods.join(", ")));
        }

        let missing_notifications: Vec<&str> = required_notifications
            .iter()
            .filter(|&notification| !notifications.contains(&notification.to_string()))
            .copied()
            .collect();

        if !missing_notifications.is_empty() {
            return Err(Error::UnsupportedNotifications(
                missing_notifications.join(", "),
            ));
        }

        Ok(())
    }

    /// Parse methods and notifications from the error message containing the raw response
    fn parse_methods_and_notifications_from_error(
        error_string: &str,
    ) -> Result<(Vec<String>, Vec<String>), Error> {
        // Look for the response JSON in the error message
        if let Some(start) = error_string.find("response=") {
            let json_start = start + "response=".len();
            if let Some(end) = error_string[json_start..].find(", error=") {
                let json_str = &error_string[json_start..json_start + end];

                // Parse the JSON response
                let response: Value = serde_json::from_str(json_str)?;

                // Extract methods and notifications from the result
                if let Some(result) = response.get("result") {
                    let methods = result
                        .get("methods")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    let notifications = result
                        .get("notifications")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    tracing::debug!(
                        "Parsed methods: {:?}, notifications: {:?}",
                        methods,
                        notifications
                    );
                    return Ok((methods, notifications));
                }
            }
        }

        Err(Error::Connection(
            "Could not parse methods and notifications from error response".to_string(),
        ))
    }
}

impl Drop for NWCWallet {
    fn drop(&mut self) {
        tracing::info!("Drop called on NWCWallet");
        self.wait_invoice_cancel_token.cancel();

        // Cancel notification handler task if it exists
        // We need to use blocking approach since Drop is synchronous
        if let Some(handle) = self
            .notification_handle
            .try_lock()
            .ok()
            .and_then(|mut guard| guard.take())
        {
            handle.abort();
        }

        // Spawn background task to handle async unsubscription
        let client = self.nwc_client.clone();
        tokio::spawn(async move {
            if let Err(e) = client.unsubscribe_from_notifications().await {
                tracing::warn!(
                    "Failed to unsubscribe from NWC notifications during cleanup: {}",
                    e
                );
            }
        });
    }
}
