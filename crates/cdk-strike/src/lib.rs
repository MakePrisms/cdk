//! CDK lightning backend for Strike

#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail};
use api::{
    types::{
        Amount as StrikeAmount, Currency as StrikeCurrencyUnit, Filter, InvoiceQueryParams,
        InvoiceRequest, InvoiceState, PayInvoiceQuoteRequest,
    },
    StrikeApi,
};
use async_trait::async_trait;
use axum::Router;
use cdk_common::amount::Amount;
use cdk_common::database::DynKVStore;
use cdk_common::nuts::{CurrencyUnit, MeltQuoteState};
use cdk_common::payment::{
    self, Bolt11Settings, CreateIncomingPaymentResponse, Event, IncomingPaymentOptions,
    MakePaymentResponse, MintPayment, OutgoingPaymentOptions, PaymentIdentifier,
    PaymentQuoteResponse, SettingsResponse, WaitPaymentResponse,
};
use cdk_common::util::unix_time;
use cdk_common::Bolt11Invoice;
use error::Error;
use futures::stream::StreamExt;
use futures::Stream;
use tokio::sync::Mutex;
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub mod api;
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
        kv_store: DynKVStore,
    ) -> Result<Self, Error> {
        let strike_api = StrikeApi::new(&api_key, None, 30_000).map_err(Error::from)?;

        // Create broadcast channel for payment events (webhook notifications)
        let (sender, receiver) = tokio::sync::broadcast::channel::<String>(1000);
        let receiver = Arc::new(receiver);

        Ok(Self {
            strike_api,
            sender,
            receiver,
            unit,
            webhook_url,
            wait_invoice_cancel_token: CancellationToken::new(),
            wait_invoice_is_active: Arc::new(AtomicBool::new(false)),
            pending_invoices: Arc::new(Mutex::new(HashMap::new())),
            webhook_mode_active: Arc::new(AtomicBool::new(false)),
            kv_store,
        })
    }

    /// Get a sender for webhook notifications
    pub fn sender(&self) -> tokio::sync::broadcast::Sender<String> {
        self.sender.clone()
    }

    async fn lookup_invoice_by_correlation_id(
        &self,
        correlation_id: &str,
    ) -> Result<api::types::InvoiceListItem, Error> {
        let query_params =
            InvoiceQueryParams::new().filter(Filter::eq("correlationId", correlation_id));

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

    fn create_webhook_stream(
        &self,
        receiver: tokio::sync::broadcast::Receiver<String>,
        cancel_token: CancellationToken,
        is_active: Arc<AtomicBool>,
        strike_api: StrikeApi,
        unit: CurrencyUnit,
    ) -> Pin<Box<dyn Stream<Item = Event> + Send>> {
        let response_stream = BroadcastStream::new(receiver)
            .filter_map(move |result| {
                let unit = unit.clone();
                let strike_api = strike_api.clone();
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
                                            match Strike::from_strike_amount(invoice.amount, &unit) {
                                                Ok(amount) => {
                                                    is_active.store(false, Ordering::SeqCst);
                                                    Some(Event::PaymentReceived(WaitPaymentResponse {
                                                        payment_identifier: PaymentIdentifier::CustomId(invoice_id.clone()),
                                                        payment_amount: Amount::new(amount, unit.clone()),
                                                        payment_id: invoice_id,
                                                    }))
                                                }
                                                Err(_) => None,
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
        // Clone for separate branches to avoid move issues
        let strike_api_broadcast = strike_api.clone();
        let pending_invoices_broadcast = pending_invoices.clone();
        let unit_broadcast = unit.clone();

        let broadcast_stream = BroadcastStream::new(receiver)
            .filter_map(move |result| {
                let strike_api = strike_api_broadcast.clone();
                let pending_invoices = pending_invoices_broadcast.clone();
                let unit = unit_broadcast.clone();
                let cancel_token = cancel_token.clone();
                async move {
                    tokio::select! {
                        _ = cancel_token.cancelled() => None,
                        event = async {
                            match result {
                                Ok(invoice_id) => {
                                    Self::process_invoice_message(&strike_api, &invoice_id, &unit, &pending_invoices).await
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
        let polling_stream = futures::stream::unfold(
            (strike_api, pending_invoices, unit),
            |(strike_api, pending_invoices, unit)| async move {
                tokio::time::sleep(POLLING_INTERVAL).await;
                let event =
                    Self::poll_pending_invoices(&strike_api, &pending_invoices, &unit).await;

                Self::cleanup_expired_invoices(&pending_invoices).await;

                Some((event, (strike_api, pending_invoices, unit)))
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

                if let Ok(amount) = Strike::from_strike_amount(invoice.amount, unit) {
                    Some(Event::PaymentReceived(WaitPaymentResponse {
                        payment_identifier: PaymentIdentifier::CustomId(invoice_id.to_string()),
                        payment_amount: Amount::new(amount, unit.clone()),
                        payment_id: invoice_id.to_string(),
                    }))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    async fn poll_pending_invoices(
        strike_api: &StrikeApi,
        pending_invoices: &Arc<Mutex<HashMap<String, u64>>>,
        unit: &CurrencyUnit,
    ) -> Option<Event> {
        let invoices_to_check: Vec<String> = {
            let pending = pending_invoices.lock().await;
            pending.keys().cloned().collect()
        };

        for invoice_id in invoices_to_check {
            if let Some(event) =
                Self::process_invoice_message(strike_api, &invoice_id, unit, pending_invoices).await
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
        internal_invoice: api::types::InvoiceListItem,
        correlation_id: &str,
    ) -> Result<PaymentQuoteResponse, payment::Error> {
        let amount = Strike::from_strike_amount(internal_invoice.amount, &self.unit)?;
        Ok(PaymentQuoteResponse {
            request_lookup_id: Some(PaymentIdentifier::CustomId(format!(
                "internal:{}",
                correlation_id
            ))),
            amount: Amount::new(amount, self.unit.clone()),
            fee: Amount::new(0, self.unit.clone()),
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
            InvoiceState::Failed | InvoiceState::Cancelled => MeltQuoteState::Failed,
        };

        let total_spent = Strike::from_strike_amount(internal_invoice.amount, &self.unit)?;

        Ok(MakePaymentResponse {
            payment_lookup_id: payment_identifier.clone(),
            payment_proof: None,
            status: state,
            total_spent: Amount::new(total_spent, self.unit.clone()),
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
                    InvoiceState::Failed | InvoiceState::Cancelled => MeltQuoteState::Failed,
                };

                let total_spent = Strike::from_strike_amount(invoice.total_amount, &self.unit)?;

                Ok(MakePaymentResponse {
                    payment_lookup_id: payment_identifier.clone(),
                    payment_proof: None,
                    status: state,
                    total_spent: Amount::new(total_spent, self.unit.clone()),
                })
            }
            Err(api::error::Error::NotFound) => Ok(MakePaymentResponse {
                payment_lookup_id: payment_identifier.clone(),
                payment_proof: None,
                status: MeltQuoteState::Unknown,
                total_spent: Amount::new(0, self.unit.clone()),
            }),
            Err(err) => Err(Error::from(err).into()),
        }
    }
}

#[async_trait]
impl MintPayment for Strike {
    type Err = payment::Error;

    async fn get_settings(&self) -> Result<SettingsResponse, Self::Err> {
        Ok(SettingsResponse {
            unit: self.unit.to_string(),
            bolt11: Some(Bolt11Settings {
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

        // Check for internal invoice first
        if let Some(correlation_id) = correlation_id {
            if let Ok(internal_invoice) =
                self.lookup_invoice_by_correlation_id(correlation_id).await
            {
                return self
                    .handle_internal_payment_quote(internal_invoice, correlation_id)
                    .await;
            }
        }

        // Regular Lightning payment quote
        let payment_quote_request = PayInvoiceQuoteRequest {
            ln_invoice: bolt11.to_string(),
            source_currency,
            amount: None,
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
            amount: Amount::new(amount, self.unit.clone()),
            fee: Amount::new(fee, self.unit.clone()),
            state: MeltQuoteState::Unpaid,
        })
    }

    async fn make_payment(
        &self,
        unit: &CurrencyUnit,
        options: OutgoingPaymentOptions,
    ) -> Result<MakePaymentResponse, Self::Err> {
        let bolt11 = match options {
            OutgoingPaymentOptions::Bolt11(opts) => opts.bolt11,
            OutgoingPaymentOptions::Bolt12(_) | OutgoingPaymentOptions::Custom(_) => {
                return Err(Self::Err::UnsupportedPaymentOption);
            }
        };

        if unit != &self.unit {
            return Err(Self::Err::UnsupportedUnit);
        }

        let source_currency = to_strike_currency(unit)?;

        let payment_quote_request = PayInvoiceQuoteRequest {
            ln_invoice: bolt11.to_string(),
            source_currency,
            amount: None,
        };

        let quote = self
            .strike_api
            .payment_quote(payment_quote_request)
            .await
            .map_err(Error::from)?;

        let pay_response = self
            .strike_api
            .pay_quote(&quote.payment_quote_id)
            .await
            .map_err(Error::from)?;

        let state = match pay_response.state {
            InvoiceState::Paid | InvoiceState::Completed => MeltQuoteState::Paid,
            InvoiceState::Unpaid => MeltQuoteState::Unpaid,
            InvoiceState::Pending => MeltQuoteState::Pending,
            InvoiceState::Failed | InvoiceState::Cancelled => MeltQuoteState::Failed,
        };

        let total_spent = Strike::from_strike_amount(pay_response.total_amount, unit)?;

        Ok(MakePaymentResponse {
            payment_lookup_id: PaymentIdentifier::CustomId(format!(
                "payment:{}",
                pay_response.payment_id
            )),
            payment_proof: None,
            status: state,
            total_spent: Amount::new(total_spent, unit.clone()),
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
        let strike_amount = Strike::to_strike_unit(amount, unit)?;

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

        Ok(CreateIncomingPaymentResponse {
            request_lookup_id: PaymentIdentifier::CustomId(create_invoice_response.invoice_id),
            request: quote.ln_invoice,
            expiry,
            extra_json: None,
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
                let amount = Strike::from_strike_amount(invoice.amount, &self.unit)?;

                Ok(vec![WaitPaymentResponse {
                    payment_identifier: payment_identifier.clone(),
                    payment_amount: Amount::new(amount, self.unit.clone()),
                    payment_id: request_lookup_id,
                }])
            }
            Ok(false) => match invoice.state {
                InvoiceState::Paid | InvoiceState::Completed => {
                    let amount = Strike::from_strike_amount(invoice.amount, &self.unit)?;
                    Ok(vec![WaitPaymentResponse {
                        payment_identifier: payment_identifier.clone(),
                        payment_amount: Amount::new(amount, self.unit.clone()),
                        payment_id: invoice.invoice_id,
                    }])
                }
                InvoiceState::Unpaid
                | InvoiceState::Pending
                | InvoiceState::Failed
                | InvoiceState::Cancelled => Ok(vec![]),
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
            _ => self.check_regular_payment(payment_identifier, id).await,
        }
    }
}

impl Strike {
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
    pub fn create_invoice_webhook(&self, webhook_endpoint: &str) -> anyhow::Result<Router> {
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

        Ok(api::webhook::create_invoice_webhook_router(
            webhook_endpoint,
            mpsc_sender,
            self.strike_api.webhook_secret().to_string(),
        ))
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
            _ => bail!("Unsupported unit"),
        }
    }
}

#[cfg(test)]
mod tests {
    // Mock KV store for testing
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::api::types::{Amount as StrikeAmount, Currency as StrikeCurrencyUnit};
    use async_trait::async_trait;
    use cdk_common::database::Error as DatabaseError;
    use cdk_common::database::{
        DbTransactionFinalizer, DynKVStore, KVStore, KVStoreDatabase, KVStoreTransaction,
    };
    use cdk_common::nuts::CurrencyUnit;
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
    impl KVStoreTransaction<DatabaseError> for MockKVTransaction {
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
        async fn begin_transaction(
            &self,
        ) -> Result<Box<dyn KVStoreTransaction<Self::Err> + Send + Sync>, DatabaseError> {
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
        // Fiat currencies are no longer supported
        assert!(to_strike_currency(&CurrencyUnit::Usd).is_err());
        assert!(to_strike_currency(&CurrencyUnit::Eur).is_err());
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

        // Msats to BTC
        let result = Strike::to_strike_unit(100_000_000_000u64, &CurrencyUnit::Msat).unwrap();
        assert_eq!(result.currency, StrikeCurrencyUnit::BTC);
        assert_eq!(result.amount, 1.0);

        // Fiat currencies are no longer supported
        assert!(Strike::to_strike_unit(1050u64, &CurrencyUnit::Usd).is_err());
    }

    #[test]
    fn test_roundtrip_conversions() {
        // Test that conversions are lossless
        let original_sats = 12345678u64;
        let strike_amount = Strike::to_strike_unit(original_sats, &CurrencyUnit::Sat).unwrap();
        let converted_back = Strike::from_strike_amount(strike_amount, &CurrencyUnit::Sat).unwrap();
        assert_eq!(original_sats, converted_back);
    }

    // Strike instance tests
    #[tokio::test]
    async fn test_strike_creation() {
        let kv_store = create_mock_kv_store();
        let strike = Strike::new(
            "test_api_key".to_string(),
            CurrencyUnit::Sat,
            "http://localhost:3000/webhook".to_string(),
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
