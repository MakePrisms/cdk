# CDK Square

Square payment backend integration for Cashu Development Kit (CDK).

## Overview

`cdk-square` provides Square Lightning payment tracking and webhook handling for payment detection. It monitors Lightning invoices paid through Square's payment system and stores payment hash mappings in a persistent KV store.

## Features

- **Webhook Mode**: Real-time payment notifications with HMAC signature verification
- **Polling Mode**: Automatic fallback to polling every 5 seconds when webhooks unavailable
- **Payment Hash Tracking**: Maps Lightning payment hashes to Square payment IDs
- **KV Store Integration**: Persistent storage for invoice hashes and configuration

## Usage

```rust
use cdk_square::{Square, SquareConfig};

let config = SquareConfig {
    api_token: "your-square-api-token".to_string(),
    environment: "SANDBOX".to_string(), // or "PRODUCTION"
    webhook_enabled: true,
    payment_expiry: 300, // seconds - how far back to sync payments
};

// Initialize with optional webhook URL
let webhook_url = Some("https://your-mint.com/webhook/square/payment".to_string());

let square = Square::from_config(config, webhook_url, kv_store)?;

// Start payment tracking (sets up webhooks or starts polling)
square.start().await?;

// Check if an invoice has been paid
let invoice = Bolt11Invoice::from_str(&bolt11_string)?;
let paid = square.check_invoice_exists(&invoice).await?;

// Create webhook router (if using webhook mode)
if let Some(router) = square.create_webhook_router() {
    // Merge into your Axum app
    app = app.merge(router);
}
```

## Webhook vs Polling Mode

**Webhook Mode (Recommended)**
- Real-time notifications when payments are received
- Lower latency and resource usage
- Requires HTTPS endpoint accessible by Square
- Automatic signature verification

**Polling Mode**
- Polls Square API every 5 seconds for new payments

Enable webhook mode by setting `webhook_enabled: true` and providing a `webhook_url`. Otherwise, polling mode is used automatically.

## Configuration

```rust
pub struct SquareConfig {
    /// Square API token
    pub api_token: String,
    /// Square environment (SANDBOX or PRODUCTION)
    pub environment: String,
    /// Enable webhook mode (if false, uses polling)
    pub webhook_enabled: bool,
    /// Payment expiry time in seconds (default: 300)
    pub payment_expiry: u64,
}
```

## Security

Webhook requests are verified using HMAC-SHA256 signatures:
1. Square provides a `signature_key` when creating webhook subscriptions
2. Each webhook includes an `x-square-hmacsha256-signature` header
3. Signature verified as: `HMAC-SHA256(webhook_url + request_body, signature_key)`
4. Invalid signatures are rejected with 401 Unauthorized

See [Square Webhook Documentation](https://developer.squareup.com/docs/webhooks/step3validate).

## License

MIT
