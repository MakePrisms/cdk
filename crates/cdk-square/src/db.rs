//! PostgreSQL database integration for Square OAuth credentials

use std::sync::Arc;

use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::Client;

use crate::error::Error;

/// OAuth credentials from Square
#[derive(Debug, Clone)]
pub struct OAuthCredentials {
    /// Square OAuth access token
    pub access_token: String,
    /// Square OAuth refresh token
    pub refresh_token: String,
    /// Token expiration timestamp (RFC 3339 format)
    pub expires_at: String,
}

/// PostgreSQL database connection for Square OAuth credentials
#[derive(Clone)]
pub struct SquareDatabase {
    client: Arc<Client>,
}

impl SquareDatabase {
    /// Create a new database connection
    pub async fn new(database_url: &str) -> Result<Self, Error> {
        // Create TLS connector with lenient settings for development
        let builder = TlsConnector::builder();

        let connector = builder.build().map_err(|e| {
            Error::DatabaseConnection(format!("Failed to build TLS connector: {}", e))
        })?;
        let tls = MakeTlsConnector::new(connector);

        let (client, connection) =
            tokio_postgres::connect(database_url, tls)
                .await
                .map_err(|e| {
                    Error::DatabaseConnection(format!("Failed to connect to database: {}", e))
                })?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!("PostgreSQL connection error: {}", e);
            }
        });

        Ok(Self {
            client: Arc::new(client),
        })
    }

    /// Read OAuth credentials from the database
    ///
    /// Queries the `mints.square_merchant_credentials` table.
    pub async fn read_credentials(&self) -> Result<OAuthCredentials, Error> {
        let row = self
            .client
            .query_one(
                "SELECT access_token, refresh_token, expires_at FROM mints.square_merchant_credentials",
                &[],
            )
            .await
            .map_err(|e| {
                Error::DatabaseQuery(format!("Failed to read OAuth credentials: {}", e))
            })?;

        Ok(OAuthCredentials {
            access_token: row.get(0),
            refresh_token: row.get(1),
            expires_at: row.get(2),
        })
    }

    /// Update OAuth credentials in the database
    ///
    /// Updates the `mints.square_merchant_credentials` table with new tokens
    /// and sets `updated_at` to the current timestamp.
    pub async fn update_credentials(
        &self,
        access_token: &str,
        refresh_token: &str,
        expires_at: &str,
    ) -> Result<u64, Error> {
        let rows_affected = self
            .client
            .execute(
                "UPDATE mints.square_merchant_credentials SET access_token = $1, refresh_token = $2, expires_at = $3, updated_at = now()",
                &[&access_token, &refresh_token, &expires_at],
            )
            .await?;

        Ok(rows_affected)
    }
}
