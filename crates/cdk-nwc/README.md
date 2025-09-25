# CDK NWC

[![crates.io](https://img.shields.io/crates/v/cdk-nwc.svg)](https://crates.io/crates/cdk-nwc)
[![Documentation](https://docs.rs/cdk-nwc/badge.svg)](https://docs.rs/cdk-nwc)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/cashubtc/cdk/blob/main/LICENSE)

Nostr Wallet Connect (NWC) backend implementation for the Cashu Development Kit (CDK). This backend implements the [NIP-47](https://github.com/nostr-protocol/nips/blob/master/47.md) specification for remote wallet control over Nostr.

## Features

- **Full Lightning Support**: Send and receive Lightning payments via NWC protocol
- **Real-time Notifications**: Uses NWC notifications to stream payment updates
- **BOLT11 Support**: Full support for standard Lightning invoices
- **Multi-Wallet Support**: Compatible with any NWC-enabled wallet that supports the required NIP-47 methods and notifications (ie. [Alby](https://getalby.com))

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
cdk-nwc = "*"
```

## Quick Start

```rust,no_run
use cdk_nwc::NWCWallet;
use cdk_common::common::FeeReserve;
use cdk_common::nuts::CurrencyUnit;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // NWC connection string from your wallet
    let nwc_uri = "nostr+walletconnect://pubkey?relay=wss://relay.example.com&secret=...";

    // Configure fee reserves
    let fee_reserve = FeeReserve {
        min_fee_reserve: 1.into(),
        percent_fee_reserve: 0.01,
    };

    // Create the NWC wallet backend
    let wallet = NWCWallet::new(
        nwc_uri,
        fee_reserve,
        CurrencyUnit::Sat,
    ).await?;

    Ok(())
}
```

## Required NIP-47 Methods and Notifications

For the NWC backend to work properly, the connected wallet must support the following NIP-47 methods:
- `pay_invoice` - Send Lightning payments
- `get_balance` - Query wallet balance
- `make_invoice` - Generate Lightning invoices
- `lookup_invoice` - Check payment status
- `list_transactions` - Transaction history
- `get_info` - Wallet capabilities

And the following notification:
- `payment_received` - Real-time payment notifications

## Supported Wallets

Any wallet that supports the NWC protocol and the required methods/notifications can be used:
- [Alby](https://getalby.com) ✅ Full support

## License

This project is licensed under the [MIT License](../../LICENSE).
