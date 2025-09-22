//! On Subscription
//!
//! This module contains the code that is triggered when a new subscription is created.

use std::collections::HashMap;

use cdk_common::amount::to_unit;
use cdk_common::common::PaymentProcessorKey;
use cdk_common::database::DynMintDatabase;
use cdk_common::mint::MintQuote;
use cdk_common::nut17::Notification;
use cdk_common::payment::DynMintPayment;
use cdk_common::pub_sub::OnNewSubscription;
use cdk_common::quote_id::QuoteId;
use cdk_common::util::unix_time;
use cdk_common::{
    Amount, MintQuoteBolt12Response, MintQuoteState, NotificationPayload, PaymentMethod,
};
use tracing::instrument;

use crate::nuts::{MeltQuoteBolt11Response, MintQuoteBolt11Response, ProofState, PublicKey};

#[derive(Default)]
/// Subscription Init
///
/// This struct triggers code when a new subscription is created.
///
/// It is used to send the initial state of the subscription to the client.
pub struct OnSubscription {
    pub(crate) localstore: Option<DynMintDatabase>,
    pub(crate) payment_processors: Option<HashMap<PaymentProcessorKey, DynMintPayment>>,
}

impl OnSubscription {
    /// Check the status of an ln payment for a quote
    #[instrument(skip_all)]
    async fn check_mint_quote_paid(&self, quote: &mut MintQuote) -> Result<(), String> {
        let state = quote.state();

        // We can just return here and do not need to check with ln node.
        // If quote is issued it is already in a final state,
        // If it is paid ln node will only tell us what we already know
        if quote.payment_method == PaymentMethod::Bolt11
            && (state == MintQuoteState::Issued || state == MintQuoteState::Paid)
        {
            return Ok(());
        }

        let payment_processors = match &self.payment_processors {
            Some(processors) => processors,
            None => return Ok(()),
        };

        let localstore = match &self.localstore {
            Some(store) => store,
            None => return Ok(()),
        };

        let ln = match payment_processors.get(&PaymentProcessorKey::new(
            quote.unit.clone(),
            quote.payment_method.clone(),
        )) {
            Some(ln) => ln,
            None => {
                tracing::info!("Could not get ln backend for {}, bolt11 ", quote.unit);
                return Ok(());
            }
        };

        let ln_status = ln
            .check_incoming_payment_status(&quote.request_lookup_id)
            .await
            .map_err(|e| e.to_string())?;

        if ln_status.is_empty() {
            return Ok(());
        }

        let mut tx = localstore
            .begin_transaction()
            .await
            .map_err(|e| e.to_string())?;

        for payment in ln_status {
            if payment.payment_amount > Amount::ZERO {
                tracing::debug!(
                    "Found payment of {} {} for quote {} when checking.",
                    payment.payment_amount,
                    payment.unit,
                    quote.id
                );

                let amount_paid = to_unit(payment.payment_amount, &payment.unit, &quote.unit)
                    .map_err(|e| e.to_string())?;

                quote
                    .increment_amount_paid(amount_paid)
                    .map_err(|e| e.to_string())?;
                quote
                    .add_payment(amount_paid, payment.payment_id.clone(), unix_time())
                    .map_err(|e| e.to_string())?;

                let _total_paid = tx
                    .increment_mint_quote_amount_paid(&quote.id, amount_paid, payment.payment_id)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }

        tx.commit().await.map_err(|e| e.to_string())?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl OnNewSubscription for OnSubscription {
    type Event = NotificationPayload<QuoteId>;
    type Index = Notification;

    async fn on_new_subscription(
        &self,
        request: &[&Self::Index],
    ) -> Result<Vec<Self::Event>, String> {
        let datastore = if let Some(localstore) = self.localstore.as_ref() {
            localstore
        } else {
            return Ok(vec![]);
        };

        let mut to_return = vec![];
        let mut public_keys: Vec<PublicKey> = Vec::new();
        let mut melt_queries = Vec::new();
        let mut mint_queries = Vec::new();

        for idx in request.iter() {
            match idx {
                Notification::ProofState(pk) => public_keys.push(*pk),
                Notification::MeltQuoteBolt11(uuid) => {
                    melt_queries.push(datastore.get_melt_quote(uuid))
                }
                Notification::MintQuoteBolt11(uuid) => {
                    mint_queries.push(datastore.get_mint_quote(uuid))
                }
                Notification::MintQuoteBolt12(uuid) => {
                    mint_queries.push(datastore.get_mint_quote(uuid))
                }
                Notification::MeltQuoteBolt12(uuid) => {
                    melt_queries.push(datastore.get_melt_quote(uuid))
                }
            }
        }

        if !melt_queries.is_empty() {
            to_return.extend(
                futures::future::try_join_all(melt_queries)
                    .await
                    .map(|quotes| {
                        quotes
                            .into_iter()
                            .filter_map(|quote| quote.map(|x| x.into()))
                            .map(|x: MeltQuoteBolt11Response<QuoteId>| x.into())
                            .collect::<Vec<_>>()
                    })
                    .map_err(|e| e.to_string())?,
            );
        }

        if !mint_queries.is_empty() {
            let quotes = futures::future::try_join_all(mint_queries)
                .await
                .map_err(|e| e.to_string())?;

            for mut quote in quotes.into_iter().flatten() {
                // Check payment status similar to route handler
                if quote.payment_method == PaymentMethod::Bolt11 {
                    if let Err(err) = self.check_mint_quote_paid(&mut quote).await {
                        tracing::warn!(
                            "Could not check payment status for mint quote {}: {}",
                            quote.id,
                            err
                        );
                    }
                }

                // Convert to response and add to return list
                match quote.payment_method {
                    PaymentMethod::Bolt11 => {
                        let response: MintQuoteBolt11Response<QuoteId> = quote.into();
                        to_return.push(response.into());
                    }
                    PaymentMethod::Bolt12 => match quote.try_into() {
                        Ok(response) => {
                            let response: MintQuoteBolt12Response<QuoteId> = response;
                            to_return.push(response.into());
                        }
                        Err(_) => {
                            tracing::warn!("Could not convert Bolt12 quote to response");
                        }
                    },
                    PaymentMethod::Custom(_) => {
                        tracing::debug!("Skipping custom payment method quote {}", quote.id);
                    }
                }
            }
        }

        if !public_keys.is_empty() {
            to_return.extend(
                datastore
                    .get_proofs_states(public_keys.as_slice())
                    .await
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .enumerate()
                    .filter_map(|(idx, state)| state.map(|state| (public_keys[idx], state).into()))
                    .map(|state: ProofState| state.into()),
            );
        }

        Ok(to_return)
    }
}
