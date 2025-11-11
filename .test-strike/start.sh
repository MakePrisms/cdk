#!/bin/bash
# Quick start script for fake wallet

set -e

# Change to repository root
cd "$(dirname "$0")/.."

echo "Starting cdk-mintd with fake wallet..."
echo "Work directory: .test2"
echo "URL: http://127.0.0.1:8085"
echo ""

cargo run -p cdk-mintd --features strike -- --work-dir .test-strike --config .test-strike/config.toml

