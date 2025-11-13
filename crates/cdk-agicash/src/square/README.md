# Square Payment Backend Module

This module provides Square Lightning payment tracking and webhook handling for internal payment detection in CDK payment processor backends.

## Features

- **Webhook Mode**: Real-time payment notifications with signature verification
- **Polling Mode**: Automatic fallback to polling every 5 seconds  
- **KV Store Integration**: Persistent storage of payment data
- **Payment Hash Tracking**: Maps Lightning payment hashes to Square payment IDs
- **OAuth Token Management**: Automatically refreshes OAuth tokens from PostgreSQL database

## Architecture

The Square backend consists of several components:

### API Layer
- **api.rs**: Low-level HTTP client for Square REST API calls (payments, merchants, webhooks)
  - Handles HTTP request building and error diagnostics
  - No business logic, only API communication

### Application Logic
- **client.rs**: Core business logic for invoice checking and merchant caching
- **sync.rs**: Payment synchronization orchestration with overlap buffer strategy
- **webhook.rs**: Webhook subscription management and signature verification

### Configuration & Storage
- **config.rs**: Configuration types for Square integration
- **db.rs**: PostgreSQL database connection for OAuth credentials
- **types.rs**: Square API request/response types
- **util.rs**: Utility functions for time conversion and KV store constants
- **error.rs**: Error types

## Setup

### Prerequisites

1. A Square developer account
2. OAuth access tokens stored in a PostgreSQL database
3. A CDK mint with KV store support

### Database Schema

Create the following PostgreSQL table to store OAuth credentials:

```sql
CREATE SCHEMA IF NOT EXISTS mints;

CREATE TABLE mints.square_merchant_credentials (
    access_token TEXT NOT NULL,
    refresh_token TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    updated_at TIMESTAMP DEFAULT NOW()
);
```

### Configuration

```rust
use cdk_agicash::square::{Square, SquareConfig};

let config = SquareConfig {
    api_token: "your-api-token".to_string(),
    environment: "SANDBOX".to_string(), // or "PRODUCTION"
    database_url: "postgresql://user:pass@host:5432/db?sslmode=require".to_string(),
    webhook_enabled: true,
    payment_expiry: 300, // 5 minutes
};

let square = Square::from_config(
    config,
    Some("https://your-mint.com/webhook/square".to_string()),
    kv_store,
)?;

// Start the Square backend
square.start().await?;
```

## Usage

### Checking Invoice Existence

```rust
use cdk_common::lightning_invoice::Bolt11Invoice;

let invoice: Bolt11Invoice = /* ... */;
let exists = square.check_invoice_exists(&invoice).await?;

if exists {
    println!("Payment found in Square!");
}
```

### Webhook Integration

If webhook mode is enabled, merge the webhook router into your main Axum router:

```rust
let mut app = Router::new()
    .route("/", get(|| async { "CDK Mint" }));

// Add Square webhook routes if available
if let Some(webhook_router) = square.create_webhook_router() {
    app = app.merge(webhook_router);
}
```

## Security

- **Webhook Signature Verification**: All webhook requests are verified using HMAC-SHA256
- **TLS Required**: All connections use TLS (rustls)
- **OAuth Token Refresh**: Tokens are automatically refreshed from the database every 60 seconds

## Environment Variables

While this module is primarily configured via code, ensure your PostgreSQL database URL includes proper SSL settings:

```
postgresql://user:password@host:5432/database?sslmode=require
```

## Limitations

- Square allows a maximum of 3 webhook subscriptions per account
- Payment expiry cleanup is handled at the database layer (KV TTL)
- The Square SDK (squareup crate v2.13.2) doesn't expose Lightning payment details or webhook management APIs, so we implemented a custom HTTP client in `api.rs`

## Testing

The module includes unit tests for time conversion utilities:

```bash
cargo test -p cdk-agicash --lib square::
```

## License

MIT

