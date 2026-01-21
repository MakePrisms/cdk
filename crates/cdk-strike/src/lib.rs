//! CDK lightning backend for Strike

#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail};
use async_trait::async_trait;
use axum::Router;
use cdk_agicash::{ClosedLoopConfig, ClosedLoopManager, FeeConfig, FeeManager};
use cdk_common::amount::Amount;
use cdk_common::database::DynKVStore;
use cdk_common::nuts::{CurrencyUnit, MeltQuoteState};
use cdk_common::payment::{
    self, CreateIncomingPaymentResponse, Event, IncomingPaymentOptions, MakePaymentResponse,
    MintPayment, OutgoingPaymentOptions, PaymentIdentifier, PaymentQuoteResponse,
    WaitPaymentResponse,
};
use cdk_common::util::unix_time;
use cdk_common::Bolt11Invoice;
use error::Error;
use futures::stream::StreamExt;
use futures::Stream;
use strike_rs::{
    Amount as StrikeAmount, Currency as StrikeCurrencyUnit, CurrencyExchangeQuoteRequest,
    ExchangeAmount, ExchangeQuoteState, FeePolicy, InvoiceQueryParams, InvoiceRequest,
    InvoiceState, PayInvoiceQuoteRequest, Strike as StrikeApi,
};
use tokio::sync::Mutex;
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub mod error;

const CORRELATION_ID_PREFIX: &str = "TXID:";
const POLLING_INTERVAL: Duration = Duration::from_secs(3);
const INVOICE_EXPIRY_HOURS: u64 = 24;

// KV Store constants for Strike
const STRIKE_KV_PRIMARY_NAMESPACE: &str = "cdk_strike_lightning_backend";
const STRIKE_KV_SECONDARY_NAMESPACE: &str = "internal_settlements";
const INTERNAL_SETTLEMENT_PREFIX: &str = "settlement_";

/// Extract correlation ID from bolt11 invoice description
fn extract_correlation_id(description: &str) -> Option<&str> {
    description
        .split(CORRELATION_ID_PREFIX)
        .nth(1)?
        .split_whitespace()
        .next()
        .filter(|id| !id.is_empty())
}

/// Create description with embedded correlation ID for Strike invoice tracking
fn create_invoice_description(base_description: &str, correlation_id: &Uuid) -> String {
    format!(
        "{} {}{}",
        base_description, CORRELATION_ID_PREFIX, correlation_id
    )
}

/// Convert CurrencyUnit to Strike's currency format
fn to_strike_currency(unit: &CurrencyUnit) -> Result<StrikeCurrencyUnit, payment::Error> {
    match unit {
        CurrencyUnit::Sat | CurrencyUnit::Msat => Ok(StrikeCurrencyUnit::BTC),
        CurrencyUnit::Usd => Ok(StrikeCurrencyUnit::USD),
        CurrencyUnit::Eur => Ok(StrikeCurrencyUnit::EUR),
        _ => Err(payment::Error::UnsupportedUnit),
    }
}

/// Strike lightning backend implementation
#[derive(Clone)]
pub struct Strike {
    strike_api: StrikeApi,
    unit: CurrencyUnit,
    webhook_url: String,
    sender: tokio::sync::broadcast::Sender<String>,
    receiver: Arc<tokio::sync::broadcast::Receiver<String>>,
    wait_invoice_cancel_token: CancellationToken,
    wait_invoice_is_active: Arc<AtomicBool>,
    pending_invoices: Arc<Mutex<HashMap<String, u64>>>,
    webhook_mode_active: Arc<AtomicBool>,
    kv_store: DynKVStore,
    closed_loop: Option<ClosedLoopManager>,
    fee_manager: Option<FeeManager>,
}

impl std::fmt::Debug for Strike {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Strike")
            .field("unit", &self.unit)
            .field("webhook_url", &self.webhook_url)
            .field(
                "wait_invoice_is_active",
                &self.wait_invoice_is_active.load(Ordering::SeqCst),
            )
            .field(
                "webhook_mode_active",
                &self.webhook_mode_active.load(Ordering::SeqCst),
            )
            .field(
                "pending_invoices_count",
                &self
                    .pending_invoices
                    .try_lock()
                    .map(|m| m.len())
                    .unwrap_or(0),
            )
            .finish()
    }
}

impl Strike {
    /// Create new [`Strike`] wallet
    pub async fn new(
        api_key: String,
        unit: CurrencyUnit,
        webhook_url: String,
        closed_loop_config: Option<ClosedLoopConfig>,
        fee_config: Option<FeeConfig>,
        kv_store: DynKVStore,
    ) -> Result<Self, Error> {
        let strike_api = StrikeApi::new(&api_key, None).map_err(Error::from)?;

        // Create broadcast channel for payment events (webhook notifications)
        let (sender, receiver) = tokio::sync::broadcast::channel::<String>(1000);
        let receiver = Arc::new(receiver);

        let closed_loop = if let Some(config) = closed_loop_config {
            Some(
                ClosedLoopManager::new(kv_store.clone(), config)
                    .await
                    .map_err(|e| {
                        Error::Anyhow(anyhow!("Failed to create closed loop manager: {}", e))
                    })?,
            )
        } else {
            None
        };

        // Create the Strike instance first so we can use it in the callback
        let strike = Self {
            strike_api,
            sender,
            receiver,
            unit: unit.clone(),
            webhook_url,
            wait_invoice_cancel_token: CancellationToken::new(),
            wait_invoice_is_active: Arc::new(AtomicBool::new(false)),
            pending_invoices: Arc::new(Mutex::new(HashMap::new())),
            webhook_mode_active: Arc::new(AtomicBool::new(false)),
            kv_store: kv_store.clone(),
            closed_loop,
            fee_manager: None, // Will be set after creation
        };

        // Create fee manager with Strike as the payment backend (if configured)
        let fee_manager = if let Some(config) = fee_config {
            let payment_backend: Arc<dyn cdk_agicash::FeePayoutBackend> = Arc::new(strike.clone());

            Some(
                FeeManager::new(kv_store.clone(), config, payment_backend)
                    .map_err(|e| Error::Anyhow(anyhow!("Failed to create fee manager: {}", e)))?,
            )
        } else {
            None
        };

        Ok(Self {
            fee_manager,
            ..strike
        })
    }

    /// Get a sender for webhook notifications
    pub fn sender(&self) -> tokio::sync::broadcast::Sender<String> {
        self.sender.clone()
    }

    async fn lookup_invoice_by_correlation_id(
        &self,
        correlation_id: &str,
    ) -> Result<strike_rs::InvoiceListItem, Error> {
        let query_params = InvoiceQueryParams::new()
            .filter(strike_rs::Filter::eq("correlationId", correlation_id));

        let invoice_list = self
            .strike_api
            .get_invoices(Some(query_params))
            .await
            .map_err(Error::from)?;

        invoice_list.items.first().cloned().ok_or_else(|| {
            Error::Anyhow(anyhow!(
                "Invoice not found for correlation ID: {}",
                correlation_id
            ))
        })
    }

    /// Process a completed invoice, handling fee payouts and returning the base amount
    ///
    /// Helper function that encapsulates the logic of:
    /// 1. Notifying the fee manager that an invoice was paid
    /// 2. Getting the base amount (minus fees) if fees were configured
    /// 3. Falling back to the full invoice amount if no fees
    async fn process_completed_invoice(
        &self,
        invoice_id: &str,
        invoice_amount: StrikeAmount,
        unit: &CurrencyUnit,
    ) -> Result<u64, Error> {
        match &self.fee_manager {
            Some(fee_manager) => {
                match fee_manager.notify_invoice_paid(invoice_id).await {
                    Ok(Some(amount)) => Ok(amount),
                    Ok(None) => {
                        // No pending fee record, use full invoice amount as base
                        Strike::from_strike_amount(invoice_amount, unit).map_err(Error::from)
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to process pending fee for invoice {}: {}",
                            invoice_id,
                            e
                        );
                        Err(Error::Anyhow(anyhow!(
                            "Failed to process pending fee: {}",
                            e
                        )))
                    }
                }
            }
            None => {
                // No fee manager, use full invoice amount
                Strike::from_strike_amount(invoice_amount, unit).map_err(Error::from)
            }
        }
    }

    fn create_webhook_stream(
        &self,
        receiver: tokio::sync::broadcast::Receiver<String>,
        cancel_token: CancellationToken,
        is_active: Arc<AtomicBool>,
        strike_api: StrikeApi,
        unit: CurrencyUnit,
    ) -> Pin<Box<dyn Stream<Item = Event> + Send>> {
        let strike_self = self.clone();
        let response_stream = BroadcastStream::new(receiver)
            .filter_map(move |result| {
                let unit = unit.clone();
                let strike_api = strike_api.clone();
                let strike_self = strike_self.clone();
                let is_active = is_active.clone();
                let cancel_token = cancel_token.clone();
                async move {
                    tokio::select! {
                        _ = cancel_token.cancelled() => {
                            is_active.store(false, Ordering::SeqCst);
                            None
                        }
                        invoice_result = async {
                            match result {
                                Ok(invoice_id) if !invoice_id.is_empty() => {
                            match strike_api.get_incoming_invoice(&invoice_id).await {
                                Ok(invoice) if invoice.state == InvoiceState::Paid || invoice.state == InvoiceState::Completed => {
                                    // Process fee payout and get base amount
                                    match strike_self
                                        .process_completed_invoice(&invoice_id, invoice.amount, &unit)
                                        .await
                                    {
                                        Ok(base_amount) => {
                                            is_active.store(false, Ordering::SeqCst);
                                            Some(Event::PaymentReceived(WaitPaymentResponse {
                                                payment_identifier: PaymentIdentifier::CustomId(invoice_id.clone()),
                                                payment_amount: Amount::from(base_amount).with_unit(unit),
                                                payment_id: invoice_id,
                                            }))
                                        }
                                        Err(e) => {
                                            tracing::error!("Failed to resolve invoice base amount: {}", e);
                                            None
                                        }
                                    }
                                }
                                _ => None,
                            }
                                }
                                Err(err) => {
                                    tracing::warn!("Error in webhook broadcast stream: {}", err);
                                    None
                                }
                                _ => None,
                            }
                        } => invoice_result
                    }
                }
            });

        Box::pin(response_stream)
    }

    fn create_polling_stream(
        &self,
        receiver: tokio::sync::broadcast::Receiver<String>,
        cancel_token: CancellationToken,
        is_active: Arc<AtomicBool>,
        strike_api: StrikeApi,
        pending_invoices: Arc<Mutex<HashMap<String, u64>>>,
        unit: CurrencyUnit,
    ) -> Pin<Box<dyn Stream<Item = Event> + Send>> {
        // Clone for each stream branch before moving
        let strike_self_broadcast = self.clone();
        let strike_api_broadcast = strike_api.clone();
        let pending_invoices_broadcast = pending_invoices.clone();
        let unit_broadcast = unit.clone();
        let cancel_token_broadcast = cancel_token.clone();

        let broadcast_stream = BroadcastStream::new(receiver)
            .filter_map(move |result| {
                let strike_self = strike_self_broadcast.clone();
                let strike_api = strike_api_broadcast.clone();
                let pending_invoices = pending_invoices_broadcast.clone();
                let unit = unit_broadcast.clone();
                let cancel_token = cancel_token_broadcast.clone();
                async move {
                    tokio::select! {
                        _ = cancel_token.cancelled() => None,
                        event = async {
                            match result {
                                Ok(invoice_id) => {
                                    strike_self.process_invoice_message(&strike_api, &invoice_id, &unit, &pending_invoices).await
                                }
                                Err(err) => {
                                    tracing::warn!("Error in polling broadcast stream: {}", err);
                                    None
                                }
                            }
                        } => event
                    }
                }
            });

        // Combine broadcast stream with periodic polling
        let strike_self_polling = self.clone();
        let polling_stream = futures::stream::unfold(
            (strike_self_polling, strike_api, pending_invoices, unit),
            |(strike_self, strike_api, pending_invoices, unit)| async move {
                tokio::time::sleep(POLLING_INTERVAL).await;
                let event = strike_self
                    .poll_pending_invoices(&strike_api, &pending_invoices, &unit)
                    .await;

                Self::cleanup_expired_invoices(&pending_invoices).await;

                Some((event, (strike_self, strike_api, pending_invoices, unit)))
            },
        )
        .filter_map(|event| async move { event });

        let combined_stream =
            futures::stream::select(broadcast_stream, polling_stream).inspect(move |_| {
                is_active.store(false, Ordering::SeqCst);
            });

        Box::pin(combined_stream)
    }

    async fn process_invoice_message(
        &self,
        strike_api: &StrikeApi,
        invoice_id: &str,
        unit: &CurrencyUnit,
        pending_invoices: &Arc<Mutex<HashMap<String, u64>>>,
    ) -> Option<Event> {
        match strike_api.get_incoming_invoice(invoice_id).await {
            Ok(invoice)
                if invoice.state == InvoiceState::Paid
                    || invoice.state == InvoiceState::Completed =>
            {
                {
                    let mut pending = pending_invoices.lock().await;
                    pending.remove(invoice_id);
                }

                // Process fee payout and get base amount
                match self
                    .process_completed_invoice(invoice_id, invoice.amount, unit)
                    .await
                {
                    Ok(base_amount) => Some(Event::PaymentReceived(WaitPaymentResponse {
                        payment_identifier: PaymentIdentifier::CustomId(invoice_id.to_string()),
                        payment_amount: Amount::from(base_amount).with_unit(unit.clone()),
                        payment_id: invoice_id.to_string(),
                    })),
                    Err(e) => {
                        tracing::error!("Failed to resolve invoice base amount: {}", e);
                        None
                    }
                }
            }
            _ => None,
        }
    }

    async fn poll_pending_invoices(
        &self,
        strike_api: &StrikeApi,
        pending_invoices: &Arc<Mutex<HashMap<String, u64>>>,
        unit: &CurrencyUnit,
    ) -> Option<Event> {
        let invoices_to_check: Vec<String> = {
            let pending = pending_invoices.lock().await;
            pending.keys().cloned().collect()
        };

        for invoice_id in invoices_to_check {
            if let Some(event) = self
                .process_invoice_message(strike_api, &invoice_id, unit, pending_invoices)
                .await
            {
                return Some(event);
            }
        }
        None
    }

    async fn cleanup_expired_invoices(pending_invoices: &Arc<Mutex<HashMap<String, u64>>>) {
        let current_time = unix_time();
        let expiry_seconds = INVOICE_EXPIRY_HOURS * 60 * 60;

        let mut pending = pending_invoices.lock().await;
        pending.retain(|_, creation_time| current_time - *creation_time < expiry_seconds);
    }

    async fn handle_internal_payment_quote(
        &self,
        internal_invoice: strike_rs::InvoiceListItem,
        source_currency: StrikeCurrencyUnit,
        correlation_id: &str,
    ) -> Result<PaymentQuoteResponse, payment::Error> {
        if internal_invoice.amount.currency == source_currency {
            let amount = Strike::from_strike_amount(internal_invoice.amount, &self.unit)?;
            return Ok(PaymentQuoteResponse {
                request_lookup_id: Some(PaymentIdentifier::CustomId(format!(
                    "internal:{}",
                    correlation_id
                ))),
                amount: Amount::from(amount).with_unit(self.unit.clone()),
                fee: Amount::ZERO.with_unit(self.unit.clone()),
                state: MeltQuoteState::Unpaid,
            });
        }

        // Currency exchange needed
        let exchange_request = CurrencyExchangeQuoteRequest {
            sell: source_currency,
            buy: internal_invoice.amount.currency.clone(),
            amount: ExchangeAmount {
                amount: internal_invoice.amount.amount.to_string(),
                currency: internal_invoice.amount.currency,
                fee_policy: Some(FeePolicy::Exclusive),
            },
        };

        let currency_exchange_quote = self
            .strike_api
            .create_currency_exchange_quote(exchange_request)
            .await
            .map_err(Error::from)?;

        let converted_amount =
            Strike::from_strike_amount(currency_exchange_quote.source.clone(), &self.unit)?;
        let fee = if let Some(fee_info) = &currency_exchange_quote.fee {
            if Strike::currency_unit_eq_strike(&self.unit, &fee_info.currency) {
                Strike::from_strike_amount(fee_info.clone(), &self.unit)?
            } else {
                Strike::convert_fee_to_unit(
                    fee_info.clone(),
                    &self.unit,
                    currency_exchange_quote.conversion_rate.clone(),
                )?
            }
        } else {
            0
        };

        Ok(PaymentQuoteResponse {
            request_lookup_id: Some(PaymentIdentifier::CustomId(format!(
                "exchange:{}",
                currency_exchange_quote.id
            ))),
            amount: Amount::from(converted_amount).with_unit(self.unit.clone()),
            fee: Amount::from(fee).with_unit(self.unit.clone()),
            state: MeltQuoteState::Unpaid,
        })
    }

    async fn check_internal_payment(
        &self,
        payment_identifier: &PaymentIdentifier,
        correlation_id: &str,
    ) -> Result<MakePaymentResponse, payment::Error> {
        let internal_invoice = self
            .lookup_invoice_by_correlation_id(correlation_id)
            .await?;
        let state = match internal_invoice.state {
            InvoiceState::Paid | InvoiceState::Completed => MeltQuoteState::Paid,
            InvoiceState::Unpaid => MeltQuoteState::Unpaid,
            InvoiceState::Pending => MeltQuoteState::Pending,
            InvoiceState::Failed => MeltQuoteState::Failed,
        };

        let total_spent = Strike::from_strike_amount(internal_invoice.amount, &self.unit)?;

        Ok(MakePaymentResponse {
            payment_lookup_id: payment_identifier.clone(),
            payment_proof: None,
            status: state,
            total_spent: Amount::from(total_spent).with_unit(self.unit.clone()),
        })
    }

    async fn check_exchange_payment(
        &self,
        payment_identifier: &PaymentIdentifier,
        quote_id: &str,
    ) -> Result<MakePaymentResponse, payment::Error> {
        let quote = self
            .strike_api
            .get_currency_exchange_quote(quote_id)
            .await
            .map_err(Error::from)?;

        let state = match quote.state {
            ExchangeQuoteState::Completed => MeltQuoteState::Paid,
            ExchangeQuoteState::Failed => MeltQuoteState::Failed,
            ExchangeQuoteState::New => MeltQuoteState::Unpaid,
            ExchangeQuoteState::Pending => MeltQuoteState::Pending,
        };

        let total_spent = Strike::from_strike_amount(quote.source, &self.unit)?;

        Ok(MakePaymentResponse {
            payment_lookup_id: payment_identifier.clone(),
            payment_proof: None,
            status: state,
            total_spent: Amount::from(total_spent).with_unit(self.unit.clone()),
        })
    }

    async fn check_regular_payment(
        &self,
        payment_identifier: &PaymentIdentifier,
        payment_id: &str,
    ) -> Result<MakePaymentResponse, payment::Error> {
        match self.strike_api.get_outgoing_payment(payment_id).await {
            Ok(invoice) => {
                let state = match invoice.state {
                    InvoiceState::Paid | InvoiceState::Completed => MeltQuoteState::Paid,
                    InvoiceState::Unpaid => MeltQuoteState::Unpaid,
                    InvoiceState::Pending => MeltQuoteState::Pending,
                    InvoiceState::Failed => MeltQuoteState::Failed,
                };

                let total_spent = Strike::from_strike_amount(invoice.total_amount, &self.unit)?;

                Ok(MakePaymentResponse {
                    payment_lookup_id: payment_identifier.clone(),
                    payment_proof: None,
                    status: state,
                    total_spent: Amount::from(total_spent).with_unit(self.unit.clone()),
                })
            }
            Err(strike_rs::Error::NotFound) => Ok(MakePaymentResponse {
                payment_lookup_id: payment_identifier.clone(),
                payment_proof: None,
                status: MeltQuoteState::Unknown,
                total_spent: Amount::ZERO.with_unit(self.unit.clone()),
            }),
            Err(err) => Err(Error::from(err).into()),
        }
    }
}

#[async_trait]
impl cdk_agicash::FeePayoutBackend for Strike {
    async fn pay_invoice(&self, bolt11: String) -> Result<String, cdk_agicash::FeeError> {
        let invoice: Bolt11Invoice = bolt11.parse()?;
        let result = self
            .pay_bolt11_invoice(&invoice, &self.unit)
            .await
            .map_err(|e| cdk_agicash::FeeError::Payment(e.into()))?;
        Ok(result.payment_id)
    }
}

#[async_trait]
impl MintPayment for Strike {
    type Err = payment::Error;

    async fn get_settings(&self) -> Result<payment::SettingsResponse, Self::Err> {
        Ok(payment::SettingsResponse {
            unit: self.unit.to_string(),
            bolt11: Some(payment::Bolt11Settings {
                mpp: false,
                amountless: false,
                invoice_description: true,
            }),
            bolt12: None,
            custom: std::collections::HashMap::new(),
        })
    }

    fn is_wait_invoice_active(&self) -> bool {
        self.wait_invoice_is_active.load(Ordering::SeqCst)
    }

    fn cancel_wait_invoice(&self) {
        self.wait_invoice_cancel_token.cancel();
        self.webhook_mode_active.store(false, Ordering::SeqCst);
    }

    #[allow(clippy::incompatible_msrv)]
    async fn wait_payment_event(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Event> + Send>>, Self::Err> {
        tracing::info!("Starting Strike payment event stream");

        let receiver = self.receiver.resubscribe();

        let strike_api = self.strike_api.clone();
        let cancel_token = self.wait_invoice_cancel_token.clone();
        let pending_invoices = Arc::clone(&self.pending_invoices);
        let is_active = Arc::clone(&self.wait_invoice_is_active);
        let unit = self.unit.clone();

        self.wait_invoice_is_active.store(true, Ordering::SeqCst);

        // Try webhook subscription first, fallback to polling
        match self
            .strike_api
            .subscribe_to_invoice_webhook(self.webhook_url.clone())
            .await
        {
            Ok(_) => {
                tracing::info!("Using webhook mode for payment events");
                self.webhook_mode_active.store(true, Ordering::SeqCst);
                Ok(self.create_webhook_stream(receiver, cancel_token, is_active, strike_api, unit))
            }
            Err(_) => {
                tracing::warn!("Webhook subscription failed, using polling mode");
                self.webhook_mode_active.store(false, Ordering::SeqCst);
                Ok(self.create_polling_stream(
                    receiver,
                    cancel_token,
                    is_active,
                    strike_api,
                    pending_invoices,
                    unit,
                ))
            }
        }
    }

    async fn get_payment_quote(
        &self,
        unit: &CurrencyUnit,
        options: OutgoingPaymentOptions,
    ) -> Result<PaymentQuoteResponse, Self::Err> {
        let bolt11 = match options {
            OutgoingPaymentOptions::Bolt11(opts) => opts.bolt11,
            OutgoingPaymentOptions::Bolt12(_) | OutgoingPaymentOptions::Custom(_) => {
                return Err(Self::Err::UnsupportedPaymentOption);
            }
        };

        if unit != &self.unit {
            return Err(Self::Err::UnsupportedUnit);
        }

        let description = bolt11.description().to_string();
        let correlation_id = extract_correlation_id(&description);
        let source_currency = to_strike_currency(unit)?;

        if let Some(ref closed_loop) = self.closed_loop {
            let payment_hash = bolt11.payment_hash().to_string();
            closed_loop
                .validate_payment(&payment_hash, Some(&bolt11))
                .await?;
        }

        if let Some(correlation_id) = correlation_id {
            if let Ok(internal_invoice) =
                self.lookup_invoice_by_correlation_id(correlation_id).await
            {
                return self
                    .handle_internal_payment_quote(
                        internal_invoice,
                        source_currency,
                        correlation_id,
                    )
                    .await;
            }
        }

        // Regular Lightning payment quote
        let payment_quote_request = PayInvoiceQuoteRequest {
            ln_invoice: bolt11.to_string(),
            source_currency,
        };

        let quote = self
            .strike_api
            .payment_quote(payment_quote_request)
            .await
            .map_err(Error::from)?;

        let fee = quote
            .lightning_network_fee
            .map(|fee| Strike::from_strike_amount(fee, unit))
            .transpose()?
            .unwrap_or(0);

        let amount = Strike::from_strike_amount(quote.amount, unit)?;

        Ok(PaymentQuoteResponse {
            request_lookup_id: Some(PaymentIdentifier::CustomId(format!(
                "payment:{}",
                quote.payment_quote_id
            ))),
            amount: Amount::from(amount).with_unit(self.unit.clone()),
            fee: Amount::from(fee).with_unit(self.unit.clone()),
            state: MeltQuoteState::Unpaid,
        })
    }

    async fn make_payment(
        &self,
        unit: &CurrencyUnit,
        options: OutgoingPaymentOptions,
    ) -> Result<MakePaymentResponse, Self::Err> {
        let bolt11 = match &options {
            OutgoingPaymentOptions::Bolt11(opts) => &opts.bolt11,
            OutgoingPaymentOptions::Bolt12(_) | OutgoingPaymentOptions::Custom(_) => {
                return Err(Self::Err::UnsupportedPaymentOption);
            }
        };

        // Check if this might be internal settlement
        let description = bolt11.description().to_string();
        let correlation_id = extract_correlation_id(&description);

        if let Some(correlation_id) = correlation_id {
            if let Ok(internal_invoice) =
                self.lookup_invoice_by_correlation_id(correlation_id).await
            {
                let source_currency = to_strike_currency(unit)?;
                // Only try internal settlement if currencies are different
                if internal_invoice.amount.currency != source_currency {
                    return self
                        .process_internal_settlement(internal_invoice, unit)
                        .await;
                } else {
                    // Same currency internal invoice should have been settled at mint level
                    tracing::error!(
                        "Internal invoice found for correlation ID {} with same currency {}. This should have been settled at the mint level, not Strike.",
                        correlation_id, source_currency
                    );
                    return Err(Self::Err::Custom(format!(
                        "Internal invoice with same currency {} should be settled at mint level, not Strike backend",
                        source_currency
                    )));
                }
            }
        }

        // If not internal, proceed with external payment
        let bolt11 = match options {
            OutgoingPaymentOptions::Bolt11(opts) => opts.bolt11,
            OutgoingPaymentOptions::Bolt12(_) | OutgoingPaymentOptions::Custom(_) => {
                return Err(Self::Err::UnsupportedPaymentOption);
            }
        };

        if unit != &self.unit {
            return Err(Self::Err::UnsupportedUnit);
        }

        let pay_response = self.pay_bolt11_invoice(&bolt11, unit).await?;

        let state = match pay_response.state {
            InvoiceState::Paid | InvoiceState::Completed => MeltQuoteState::Paid,
            InvoiceState::Unpaid => MeltQuoteState::Unpaid,
            InvoiceState::Pending => MeltQuoteState::Pending,
            InvoiceState::Failed => MeltQuoteState::Failed,
        };

        let total_spent = Strike::from_strike_amount(pay_response.total_amount, unit)?;

        Ok(MakePaymentResponse {
            payment_lookup_id: PaymentIdentifier::CustomId(format!(
                "payment:{}",
                pay_response.payment_id
            )),
            payment_proof: None,
            status: state,
            total_spent: Amount::from(total_spent).with_unit(unit.clone()),
        })
    }

    async fn create_incoming_payment_request(
        &self,
        unit: &CurrencyUnit,
        options: IncomingPaymentOptions,
    ) -> Result<CreateIncomingPaymentResponse, Self::Err> {
        let (amount, description, unix_expiry) = match options {
            IncomingPaymentOptions::Bolt11(opts) => (
                opts.amount,
                opts.description.unwrap_or_default(),
                opts.unix_expiry,
            ),
            IncomingPaymentOptions::Bolt12(_) | IncomingPaymentOptions::Custom(_) => {
                return Err(Self::Err::UnsupportedPaymentOption);
            }
        };

        let time_now = unix_time();
        if let Some(expiry) = unix_expiry {
            if expiry <= time_now {
                // Unlikely to happen, but just in case
                return Err(cdk_common::payment::Error::Custom(
                    "Payment request has expired".to_string(),
                ));
            }
        }

        let correlation_id = Uuid::new_v4();

        let (invoice_amount, fee_amount) = self.calculate_deposit_fee(amount);

        let strike_amount = Strike::to_strike_unit(invoice_amount, unit)?;

        let invoice_request = InvoiceRequest {
            correlation_id: Some(correlation_id.to_string()),
            amount: strike_amount,
            description: Some(create_invoice_description(&description, &correlation_id)),
        };

        let create_invoice_response = self
            .strike_api
            .create_invoice(invoice_request)
            .await
            .map_err(Error::from)?;

        let quote = self
            .strike_api
            .invoice_quote(&create_invoice_response.invoice_id)
            .await
            .map_err(Error::from)?;

        let request: Bolt11Invoice = quote.ln_invoice.parse()?;
        let expiry = request.expires_at().map(|t| t.as_secs());

        // Store the invoice ID for polling only if not in webhook mode
        if !self.webhook_mode_active.load(Ordering::SeqCst) {
            let mut pending_invoices = self.pending_invoices.lock().await;
            pending_invoices.insert(create_invoice_response.invoice_id.clone(), time_now);
        }

        self.register_deposit_fee(
            &create_invoice_response.invoice_id,
            amount.into(),
            fee_amount,
            unit,
            time_now + quote.expiration_in_sec,
        )
        .await?;

        if let Some(ref closed_loop) = self.closed_loop {
            // Only register payment if the closed loop type is Internal
            if closed_loop.is_internal() {
                let payment_hash = request.payment_hash().to_string();
                let expiry_secs = quote.expiration_in_sec;
                closed_loop
                    .register_payment(payment_hash, expiry_secs)
                    .await?;
            }
        }

        Ok(CreateIncomingPaymentResponse {
            request_lookup_id: PaymentIdentifier::CustomId(create_invoice_response.invoice_id),
            request: quote.ln_invoice,
            expiry,
            extra_json: None,
            fee: Some(fee_amount.into()),
        })
    }

    async fn check_incoming_payment_status(
        &self,
        payment_identifier: &PaymentIdentifier,
    ) -> Result<Vec<WaitPaymentResponse>, Self::Err> {
        let request_lookup_id = payment_identifier.to_string();
        let invoice = self
            .strike_api
            .get_incoming_invoice(&request_lookup_id)
            .await
            .map_err(Error::from)?;

        match self.check_internal_settlement(&request_lookup_id).await {
            Ok(true) => {
                let base_amount = self
                    .process_completed_invoice(&request_lookup_id, invoice.amount, &self.unit)
                    .await?;

                Ok(vec![WaitPaymentResponse {
                    payment_identifier: payment_identifier.clone(),
                    payment_amount: Amount::from(base_amount).with_unit(self.unit.clone()),
                    payment_id: request_lookup_id,
                }])
            }
            Ok(false) => match invoice.state {
                InvoiceState::Paid | InvoiceState::Completed => {
                    let base_amount = self
                        .process_completed_invoice(&invoice.invoice_id, invoice.amount, &self.unit)
                        .await?;

                    Ok(vec![WaitPaymentResponse {
                        payment_identifier: payment_identifier.clone(),
                        payment_amount: Amount::from(base_amount).with_unit(self.unit.clone()),
                        payment_id: invoice.invoice_id,
                    }])
                }
                InvoiceState::Unpaid | InvoiceState::Pending | InvoiceState::Failed => Ok(vec![]),
            },
            Err(err) => {
                tracing::error!(
                    "Failed to check internal settlement status for invoice {}: {}.",
                    request_lookup_id,
                    err
                );
                return Err(Self::Err::Custom(format!(
                    "KV store error checking internal settlement: {}",
                    err
                )));
            }
        }
    }

    async fn check_outgoing_payment(
        &self,
        payment_identifier: &PaymentIdentifier,
    ) -> Result<MakePaymentResponse, Self::Err> {
        let payment_lookup_id = payment_identifier.to_string();
        let (label, id) = payment_lookup_id
            .split_once(":")
            .unwrap_or(("payment", &payment_lookup_id));

        match label {
            "internal" => self.check_internal_payment(payment_identifier, id).await,
            "exchange" => self.check_exchange_payment(payment_identifier, id).await,
            _ => self.check_regular_payment(payment_identifier, id).await,
        }
    }
}

impl Strike {
    /// Calculate deposit fee and total invoice amount
    fn calculate_deposit_fee(&self, amount: cdk_common::amount::Amount) -> (u64, u64) {
        if let Some(ref fee_manager) = self.fee_manager {
            let fee = fee_manager.calculate_fee(amount.into());
            let total = u64::from(amount) + fee;
            tracing::debug!(
                "Applying deposit fee: base={}, fee={}, total={}",
                amount,
                fee,
                total
            );
            (total, fee)
        } else {
            (amount.into(), 0)
        }
    }

    /// Register a pending deposit fee
    async fn register_deposit_fee(
        &self,
        invoice_id: &str,
        base_amount: u64,
        fee_amount: u64,
        unit: &CurrencyUnit,
        expires_at: u64,
    ) -> Result<(), Error> {
        if let Some(ref fee_manager) = self.fee_manager {
            if fee_amount > 0 {
                fee_manager
                    .register_pending_fee(
                        invoice_id.to_string(),
                        base_amount,
                        fee_amount,
                        unit.clone(),
                        expires_at,
                    )
                    .await
                    .map_err(|e| {
                        tracing::error!(
                            "Failed to register pending fee for invoice {}: {}",
                            invoice_id,
                            e
                        );
                        Error::Anyhow(anyhow!("Failed to register pending fee: {}", e))
                    })?;
            }
        }
        Ok(())
    }
}

/// Result of paying a lightning invoice via Strike
#[derive(Debug, Clone)]
struct PaymentResult {
    payment_id: String,
    state: InvoiceState,
    total_amount: StrikeAmount,
}

impl Strike {
    /// Pay a lightning invoice using Strike's payment API (internal static version)
    ///
    /// This method handles the common flow of:
    /// 1. Creating a payment quote for the bolt11 invoice
    /// 2. Executing the payment via Strike
    ///
    /// Returns payment details on success
    async fn pay_bolt11_invoice_internal(
        strike_api: &StrikeApi,
        bolt11: &Bolt11Invoice,
        unit: &CurrencyUnit,
    ) -> Result<PaymentResult, Error> {
        let source_currency = to_strike_currency(unit)
            .map_err(|e| Error::Anyhow(anyhow!("Failed to convert currency unit: {}", e)))?;

        let payment_quote_request = PayInvoiceQuoteRequest {
            ln_invoice: bolt11.to_string(),
            source_currency,
        };

        let quote = strike_api
            .payment_quote(payment_quote_request)
            .await
            .map_err(Error::from)?;

        let pay_response = strike_api
            .pay_quote(&quote.payment_quote_id)
            .await
            .map_err(Error::from)?;

        Ok(PaymentResult {
            payment_id: pay_response.payment_id,
            state: pay_response.state,
            total_amount: pay_response.total_amount,
        })
    }

    /// Pay a lightning invoice using Strike's payment API
    ///
    /// This is a convenience wrapper around pay_bolt11_invoice_internal
    async fn pay_bolt11_invoice(
        &self,
        bolt11: &Bolt11Invoice,
        unit: &CurrencyUnit,
    ) -> Result<PaymentResult, Error> {
        Self::pay_bolt11_invoice_internal(&self.strike_api, bolt11, unit).await
    }

    /// Record an internal settlement in the KV store
    async fn record_internal_settlement(
        &self,
        invoice_id: &str,
    ) -> Result<(), cdk_common::database::Error> {
        let key = format!("{}{}", INTERNAL_SETTLEMENT_PREFIX, invoice_id);
        let settlement_data = serde_json::json!({
            "settled_at": unix_time(),
            "invoice_id": invoice_id
        });
        let value = serde_json::to_vec(&settlement_data)?;

        let mut tx = self.kv_store.begin_transaction().await?;
        tx.kv_write(
            STRIKE_KV_PRIMARY_NAMESPACE,
            STRIKE_KV_SECONDARY_NAMESPACE,
            &key,
            &value,
        )
        .await?;
        tx.commit().await?;

        Ok(())
    }

    /// Check if an invoice was settled internally
    async fn check_internal_settlement(
        &self,
        invoice_id: &str,
    ) -> Result<bool, cdk_common::database::Error> {
        let key = format!("{}{}", INTERNAL_SETTLEMENT_PREFIX, invoice_id);

        let settlement_exists = self
            .kv_store
            .kv_read(
                STRIKE_KV_PRIMARY_NAMESPACE,
                STRIKE_KV_SECONDARY_NAMESPACE,
                &key,
            )
            .await?
            .is_some();

        Ok(settlement_exists)
    }

    /// Create invoice webhook router
    pub async fn create_invoice_webhook(&self, webhook_endpoint: &str) -> anyhow::Result<Router> {
        // Create an adapter channel to bridge mpsc -> broadcast
        let (mpsc_sender, mut mpsc_receiver) = tokio::sync::mpsc::channel::<String>(1000);
        let broadcast_sender = self.sender();

        // Spawn a task to forward messages from mpsc to broadcast
        tokio::spawn(async move {
            while let Some(invoice_id) = mpsc_receiver.recv().await {
                if let Err(err) = broadcast_sender.send(invoice_id) {
                    tracing::warn!(
                        "Failed to forward webhook message to broadcast channel: {}",
                        err
                    );
                }
            }
        });

        self.strike_api
            .create_invoice_webhook_router(webhook_endpoint, mpsc_sender)
            .await
    }

    /// Execute currency exchange for internal payment (by quote id only)
    async fn execute_currency_exchange_by_id(&self, quote_id: &str) -> Result<(u64, u64), Error> {
        match self
            .strike_api
            .execute_currency_exchange_quote(quote_id)
            .await
        {
            Ok(_) => (),
            Err(strike_rs::Error::ApiError(api_error)) => {
                if api_error
                    .is_error_code(&strike_rs::StrikeErrorCode::CurrencyExchangeQuoteExpired)
                {
                    tracing::warn!("Currency exchange quote {} has expired", quote_id);
                    return Err(Error::Anyhow(anyhow!(
                        "Currency exchange quote has expired"
                    )));
                } else {
                    return Err(strike_rs::Error::ApiError(api_error).into());
                }
            }
            Err(e) => return Err(e.into()),
        }
        // After execution, fetch the quote to get the amounts/fees
        let quote = self
            .strike_api
            .get_currency_exchange_quote(quote_id)
            .await
            .map_err(Error::from)?;
        let converted_amount = Strike::from_strike_amount(quote.source.clone(), &self.unit)?;
        let fee = if let Some(fee_info) = quote.fee.clone() {
            if Strike::currency_unit_eq_strike(&self.unit, &fee_info.currency) {
                Strike::from_strike_amount(fee_info.clone(), &self.unit)?
            } else {
                Strike::convert_fee_to_unit(fee_info, &self.unit, quote.conversion_rate)?
            }
        } else {
            0
        };
        Ok((converted_amount, fee))
    }

    /// Process internal settlement for cross-currency payments only
    async fn process_internal_settlement(
        &self,
        internal_invoice: strike_rs::InvoiceListItem,
        unit: &CurrencyUnit,
    ) -> Result<MakePaymentResponse, payment::Error> {
        let source_currency = to_strike_currency(unit)?;

        let invoice_amount = internal_invoice.amount.clone();
        let exchange_request = CurrencyExchangeQuoteRequest {
            sell: source_currency,
            buy: invoice_amount.currency.clone(),
            amount: ExchangeAmount {
                amount: invoice_amount.amount.to_string(),
                currency: invoice_amount.currency.clone(),
                fee_policy: Some(FeePolicy::Exclusive),
            },
        };

        let quote = self
            .strike_api
            .create_currency_exchange_quote(exchange_request)
            .await
            .map_err(Error::from)?;

        let (converted_amount, _fee) = self.execute_currency_exchange_by_id(&quote.id).await?;

        let response = MakePaymentResponse {
            payment_lookup_id: PaymentIdentifier::CustomId(format!("exchange:{}", quote.id)),
            payment_proof: None,
            status: MeltQuoteState::Paid,
            total_spent: Amount::from(converted_amount).with_unit(unit.clone()),
        };

        if let Err(err) = self
            .record_internal_settlement(&internal_invoice.invoice_id)
            .await
        {
            tracing::warn!("Failed to record internal settlement: {}", err);
        }

        // Notify the payment stream that this invoice has been settled
        if let Err(err) = self.sender.send(internal_invoice.invoice_id.clone()) {
            tracing::warn!(
                "Failed to notify payment stream of internal settlement: {}",
                err
            );
        }

        Ok(response)
    }
}

impl Strike {
    fn from_strike_amount(
        strike_amount: StrikeAmount,
        target_unit: &CurrencyUnit,
    ) -> anyhow::Result<u64> {
        match target_unit {
            CurrencyUnit::Sat => {
                if strike_amount.currency == StrikeCurrencyUnit::BTC {
                    strike_amount.to_sats()
                } else {
                    bail!("Cannot convert Strike amount: expected BTC currency for Sat unit, got {:?} currency with amount {}", 
                          strike_amount.currency, strike_amount.amount);
                }
            }
            CurrencyUnit::Msat => {
                if strike_amount.currency == StrikeCurrencyUnit::BTC {
                    Ok(strike_amount.to_sats()? * 1000)
                } else {
                    bail!("Cannot convert Strike amount: expected BTC currency for Msat unit, got {:?} currency with amount {}", 
                          strike_amount.currency, strike_amount.amount);
                }
            }
            CurrencyUnit::Usd => {
                if strike_amount.currency == StrikeCurrencyUnit::USD {
                    Ok((strike_amount.amount * 100.0).round() as u64)
                } else {
                    bail!("Cannot convert Strike amount: expected USD currency for Usd unit, got {:?} currency with amount {}", 
                          strike_amount.currency, strike_amount.amount);
                }
            }
            CurrencyUnit::Eur => {
                if strike_amount.currency == StrikeCurrencyUnit::EUR {
                    Ok((strike_amount.amount * 100.0).round() as u64)
                } else {
                    bail!("Cannot convert Strike amount: expected EUR currency for Eur unit, got {:?} currency with amount {}", 
                          strike_amount.currency, strike_amount.amount);
                }
            }
            _ => bail!("Unsupported unit: {:?}", target_unit),
        }
    }

    fn to_strike_unit<T: Into<u64>>(
        amount: T,
        current_unit: &CurrencyUnit,
    ) -> anyhow::Result<StrikeAmount> {
        let amount = amount.into();
        match current_unit {
            CurrencyUnit::Sat => Ok(StrikeAmount::from_sats(amount)),
            CurrencyUnit::Msat => Ok(StrikeAmount::from_sats(amount / 1000)),
            CurrencyUnit::Usd => {
                let dollars = amount as f64 / 100.0;
                Ok(StrikeAmount {
                    currency: StrikeCurrencyUnit::USD,
                    amount: dollars,
                })
            }
            CurrencyUnit::Eur => {
                let euro = amount as f64 / 100.0;
                Ok(StrikeAmount {
                    currency: StrikeCurrencyUnit::EUR,
                    amount: euro,
                })
            }
            _ => bail!("Unsupported unit"),
        }
    }

    fn currency_unit_eq_strike(unit: &CurrencyUnit, strike: &StrikeCurrencyUnit) -> bool {
        match (unit, strike) {
            (CurrencyUnit::Sat, StrikeCurrencyUnit::BTC) => true,
            (CurrencyUnit::Msat, StrikeCurrencyUnit::BTC) => true, // msat is subunit of BTC
            (CurrencyUnit::Usd, StrikeCurrencyUnit::USD) => true,
            (CurrencyUnit::Eur, StrikeCurrencyUnit::EUR) => true,
            _ => false,
        }
    }

    fn convert_fee_to_unit(
        fee_amount: StrikeAmount,
        target_unit: &CurrencyUnit,
        rate: strike_rs::ConversionRate,
    ) -> anyhow::Result<u64> {
        // Only support conversion between BTC (sats) and USD/EUR for now
        let rate = rate.amount;
        match (&fee_amount.currency, target_unit) {
            (StrikeCurrencyUnit::USD, CurrencyUnit::Sat)
            | (StrikeCurrencyUnit::EUR, CurrencyUnit::Sat) => {
                // rate: X USD per BTC, so 1 USD = 1/X BTC = 100_000_000/X sats
                let sats = (fee_amount.amount * 100_000_000.0 / rate).round() as u64;
                Ok(sats)
            }
            (StrikeCurrencyUnit::USD, CurrencyUnit::Msat)
            | (StrikeCurrencyUnit::EUR, CurrencyUnit::Msat) => {
                let msats = (fee_amount.amount * 100_000_000_000.0 / rate).round() as u64;
                Ok(msats)
            }
            (StrikeCurrencyUnit::USD, CurrencyUnit::Usd)
            | (StrikeCurrencyUnit::EUR, CurrencyUnit::Eur) => {
                // fee is already in correct fiat unit, return as cents
                Ok((fee_amount.amount * 100.0).round() as u64)
            }
            _ => Err(anyhow!(
                "Unsupported fee currency/unit conversion: {:?} -> {:?}",
                fee_amount.currency,
                target_unit
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    // Mock KV store for testing
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use cdk_common::database::mint::{
        DbTransactionFinalizer, KVStore, KVStoreDatabase, KVStoreTransaction,
    };
    use cdk_common::database::Error as DatabaseError;
    use cdk_common::nuts::CurrencyUnit;
    use strike_rs::{Amount as StrikeAmount, Currency as StrikeCurrencyUnit};
    use tokio::sync::Mutex;
    use uuid::Uuid;

    use super::*;

    #[derive(Debug, Default)]
    struct MockKVStore {
        data: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    }

    #[async_trait]
    impl KVStoreDatabase for MockKVStore {
        type Err = DatabaseError;

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
            _primary_namespace: &str,
            _secondary_namespace: &str,
        ) -> Result<Vec<String>, Self::Err> {
            Ok(vec![])
        }
    }

    struct MockKVTransaction {
        store: Arc<MockKVStore>,
        changes: HashMap<String, Option<Vec<u8>>>,
    }

    #[async_trait]
    impl<'a> KVStoreTransaction<'a, DatabaseError> for MockKVTransaction {
        async fn kv_read(
            &mut self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
        ) -> Result<Option<Vec<u8>>, DatabaseError> {
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
        ) -> Result<(), DatabaseError> {
            let full_key = format!("{}:{}:{}", primary_namespace, secondary_namespace, key);
            self.changes.insert(full_key, Some(value.to_vec()));
            Ok(())
        }

        async fn kv_remove(
            &mut self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
        ) -> Result<(), DatabaseError> {
            let full_key = format!("{}:{}:{}", primary_namespace, secondary_namespace, key);
            self.changes.insert(full_key, None);
            Ok(())
        }

        async fn kv_list(
            &mut self,
            _primary_namespace: &str,
            _secondary_namespace: &str,
        ) -> Result<Vec<String>, DatabaseError> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl DbTransactionFinalizer for MockKVTransaction {
        type Err = DatabaseError;

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
        ) -> Result<Box<dyn KVStoreTransaction<'a, Self::Err> + Send + Sync + 'a>, DatabaseError>
        {
            Ok(Box::new(MockKVTransaction {
                store: Arc::new(MockKVStore {
                    data: self.data.clone(),
                }),
                changes: HashMap::new(),
            }))
        }
    }

    fn create_mock_kv_store() -> DynKVStore {
        Arc::new(MockKVStore::default())
    }

    // Helper function tests
    #[test]
    fn test_extract_correlation_id() {
        // Test valid extraction
        assert_eq!(
            extract_correlation_id("Payment TXID:abc123 text"),
            Some("abc123")
        );

        // Test no correlation ID
        assert_eq!(extract_correlation_id("Payment description"), None);

        // Test empty correlation ID
        assert_eq!(extract_correlation_id("Payment TXID:"), None);
    }

    #[test]
    fn test_create_invoice_description() {
        let correlation_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let result = create_invoice_description("Payment", &correlation_id);

        assert!(result.contains("Payment"));
        assert!(result.contains("TXID:550e8400-e29b-41d4-a716-446655440000"));
    }

    #[test]
    fn test_to_strike_currency() {
        assert_eq!(
            to_strike_currency(&CurrencyUnit::Sat).unwrap(),
            StrikeCurrencyUnit::BTC
        );
        assert_eq!(
            to_strike_currency(&CurrencyUnit::Msat).unwrap(),
            StrikeCurrencyUnit::BTC
        );
        assert_eq!(
            to_strike_currency(&CurrencyUnit::Usd).unwrap(),
            StrikeCurrencyUnit::USD
        );
        assert_eq!(
            to_strike_currency(&CurrencyUnit::Eur).unwrap(),
            StrikeCurrencyUnit::EUR
        );
    }

    // Amount conversion tests - core functionality
    #[test]
    fn test_from_strike_amount_btc() {
        // BTC to sats
        let amount = StrikeAmount {
            currency: StrikeCurrencyUnit::BTC,
            amount: 1.0,
        };
        assert_eq!(
            Strike::from_strike_amount(amount, &CurrencyUnit::Sat).unwrap(),
            100_000_000
        );

        // BTC to msats
        let amount = StrikeAmount {
            currency: StrikeCurrencyUnit::BTC,
            amount: 0.001,
        };
        assert_eq!(
            Strike::from_strike_amount(amount, &CurrencyUnit::Msat).unwrap(),
            100_000_000
        );
    }

    #[test]
    fn test_from_strike_amount_fiat() {
        // USD to cents
        let amount = StrikeAmount {
            currency: StrikeCurrencyUnit::USD,
            amount: 10.50,
        };
        assert_eq!(
            Strike::from_strike_amount(amount, &CurrencyUnit::Usd).unwrap(),
            1050
        );

        // EUR to cents
        let amount = StrikeAmount {
            currency: StrikeCurrencyUnit::EUR,
            amount: 25.75,
        };
        assert_eq!(
            Strike::from_strike_amount(amount, &CurrencyUnit::Eur).unwrap(),
            2575
        );
    }

    #[test]
    fn test_from_strike_amount_currency_mismatch() {
        let amount = StrikeAmount {
            currency: StrikeCurrencyUnit::USD,
            amount: 10.0,
        };
        // USD to BTC should fail
        assert!(Strike::from_strike_amount(amount, &CurrencyUnit::Sat).is_err());
    }

    #[test]
    fn test_to_strike_unit() {
        // Sats to BTC
        let result = Strike::to_strike_unit(100_000_000u64, &CurrencyUnit::Sat).unwrap();
        assert_eq!(result.currency, StrikeCurrencyUnit::BTC);
        assert_eq!(result.amount, 1.0);

        // USD cents to dollars
        let result = Strike::to_strike_unit(1050u64, &CurrencyUnit::Usd).unwrap();
        assert_eq!(result.currency, StrikeCurrencyUnit::USD);
        assert_eq!(result.amount, 10.50);
    }

    #[test]
    fn test_roundtrip_conversions() {
        // Test that conversions are lossless
        let original_sats = 12345678u64;
        let strike_amount = Strike::to_strike_unit(original_sats, &CurrencyUnit::Sat).unwrap();
        let converted_back = Strike::from_strike_amount(strike_amount, &CurrencyUnit::Sat).unwrap();
        assert_eq!(original_sats, converted_back);
    }

    #[test]
    fn test_currency_unit_eq_strike() {
        assert!(Strike::currency_unit_eq_strike(
            &CurrencyUnit::Sat,
            &StrikeCurrencyUnit::BTC
        ));
        assert!(Strike::currency_unit_eq_strike(
            &CurrencyUnit::Usd,
            &StrikeCurrencyUnit::USD
        ));
        assert!(!Strike::currency_unit_eq_strike(
            &CurrencyUnit::Sat,
            &StrikeCurrencyUnit::USD
        ));
    }

    // Fee conversion test
    #[test]
    fn test_convert_fee_to_unit() {
        let fee_amount = StrikeAmount {
            currency: StrikeCurrencyUnit::USD,
            amount: 1.0,
        };

        let rate = strike_rs::ConversionRate {
            amount: 50000.0,
            source_currency: StrikeCurrencyUnit::USD,
            target_currency: StrikeCurrencyUnit::BTC,
        };

        let result = Strike::convert_fee_to_unit(fee_amount, &CurrencyUnit::Sat, rate).unwrap();
        assert_eq!(result, 2000); // $1 at $50k/BTC = 2000 sats
    }

    // Strike instance tests
    #[tokio::test]
    async fn test_strike_creation() {
        let kv_store = create_mock_kv_store();
        let strike = Strike::new(
            "test_api_key".to_string(),
            CurrencyUnit::Sat,
            "http://localhost:3000/webhook".to_string(),
            None,
            None,
            kv_store,
        )
        .await;

        assert!(strike.is_ok());
        let strike = strike.unwrap();
        assert_eq!(strike.unit, CurrencyUnit::Sat);
        assert!(!strike.is_wait_invoice_active());
    }

    #[tokio::test]
    async fn test_wait_payment_event_multiple_calls() {
        let kv_store = create_mock_kv_store();
        let strike = Strike::new(
            "test_api_key".to_string(),
            CurrencyUnit::Sat,
            "http://localhost:3000/webhook".to_string(),
            None,
            None,
            kv_store,
        )
        .await
        .unwrap();

        // Multiple calls should succeed
        let result1 = strike.wait_payment_event().await;
        assert!(result1.is_ok());

        strike.cancel_wait_invoice();

        let result2 = strike.wait_payment_event().await;
        assert!(result2.is_ok());
    }

    #[test]
    fn test_zero_amounts() {
        let zero_btc = StrikeAmount {
            currency: StrikeCurrencyUnit::BTC,
            amount: 0.0,
        };
        assert_eq!(
            Strike::from_strike_amount(zero_btc, &CurrencyUnit::Sat).unwrap(),
            0
        );

        let result = Strike::to_strike_unit(0u64, &CurrencyUnit::Sat).unwrap();
        assert_eq!(result.amount, 0.0);
    }
}
