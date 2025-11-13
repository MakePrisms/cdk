# cdk-agicash

Extended functionality for the Cashu Development Kit (CDK) as part of the Agicash project.

This crate provides the additional features required by Agicash implementations that are beyond
the core cashu protocol.

## Features

### Closed Loop Payment Management

- **Internal Payment Validation**: Register and validate payments within a closed loop system
- **Automatic Cleanup**: Background task to remove expired payment registrations
- **Node Pubkey Validation**: Support for validating payments against specific node public keys
- **KV Store Integration**: Uses the CDK KV store for persistent payment tracking

## License

MIT

