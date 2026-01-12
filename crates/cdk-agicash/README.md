# cdk-agicash

Extended CDK functionality for Agicash-specific features beyond the core Cashu protocol.

## Protocol Extensions

### NUT04 Mint Quote Fee Extension

Extends mint quote responses (POST `/v1/mint/quote/bolt11`) with an optional `fee` field:

```json
{
  "quote": "quote_id_123",
  "request": "lnbc...",
  "paid": false,
  "expiry": 1234567890,
  "fee": 30
}
```

- **`fee`**: Optional deposit fee in the quote's unit

### NUT06 Agicash Field

Extends mint info with Agicash-specific settings advertised via the `/v1/info` endpoint:

```json
{
  "agicash": {
    "closed_loop": true,
    "deposit_fee": {
      "type": "basis_points",
      "value": 300,
      "minimum": 10
    }
  }
}
```

- **`closed_loop`**: Indicates the mint only processes payments within a defined payment network
- **`deposit_fee`**: Optional deposit fee structure in basis points (300 = 3%)

## Features

### Closed-Loop Payment Management

Restrict payments to a defined network using `ClosedLoopManager`:

- **Internal validation**: Mint will only pay invoices that it generated
- **Node pubkey validation**: Mint will only pay invoices from specific Lightning node(s)
- **Square integration**: Mint will only pay invoices from a specific Square merchant's POS

```rust,ignore
use cdk_agicash::{ClosedLoopManager, ClosedLoopConfig};

// Internal payments only
let manager = ClosedLoopManager::new(
    kv_store,
    ClosedLoopConfig::internal("My Payment Network"),
)?;

// Validate before processing quote requests
let valid = manager.check_invoice_valid(&bolt11_invoice).await?;
```

### Deposit Fee Collection

Calculate and automatically pay out deposit fees to a Lightning address:

```rust,ignore
use cdk_agicash::{FeeManager, FeeConfig};

// 3% fee with 10 sat minimum
let config = FeeConfig::basis_points(
    300, 
    10, 
    "fees@getalby.com".to_string(), 
    "My Mint".to_string()
);
let fee_manager = FeeManager::new(kv_store, config.clone(), pay_invoice_callback)?;

// Register fee when quote is created
let fee = config.calculator().calculate_fee(amount);
fee_manager.register_pending_fee(quote_id, amount, fee, unit, expires_at).await?;

// Notify when invoice is paid to trigger fee payout
fee_manager.notify_invoice_paid(quote_id).await?;
```

### Square Payment Integration

Track Square Lightning payments via webhooks or polling with `Square` client. See [square/README.md](src/square/README.md) for details.

### Utilities

- **`PeriodicSupervisor`**: Run periodic background tasks (cleanup, sync, etc.)
- **LNURL client**: Resolve Lightning addresses to BOLT11 invoices

## License

MIT
