//! gRPC client for `wallet-core`.
//!
//! Before this module the betting engine was pure bookkeeping: `place_bet`
//! validated the stake and inserted a row, `settle_bet` wrote `actual_win`, and
//! no money moved anywhere. A player could therefore bet without funds and win
//! without being paid — the two services never talked to each other.
//!
//! Money now follows this flow (all calls are idempotent on the bet id):
//!
//! * place  — `Lock` the stake (available → locked) with `reference_id = bet id`;
//! * win    — `Unlock` the stake, then `Credit` the payout;
//! * lost   — `Unlock` the stake, then `Debit` it (the stake is spent);
//! * void   — `Unlock` the stake (money returns to available).

use std::sync::Arc;

use rust_decimal::Decimal;
use tonic::transport::{Channel, Endpoint};
use tracing::{info, warn};

use crate::grpc::proto::common::v1::{Money, UserId};
use crate::grpc::proto::wallet::v1::wallet_core_service_client::WalletCoreServiceClient;
use crate::grpc::proto::wallet::v1::{
    CreditRequest, DebitRequest, LockRequest, UnlockRequest,
};

/// `common.v1.WalletType::Main`. Bets are always settled against real money.
const WALLET_TYPE_MAIN: i32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum WalletClientError {
    #[error("insufficient balance for user {user_id}")]
    InsufficientFunds { user_id: i64 },
    #[error("wallet rejected the operation: {0}")]
    Rejected(String),
    #[error("wallet-core is unavailable: {0}")]
    Unavailable(String),
}

/// Cloneable wrapper around the generated gRPC client.
#[derive(Clone)]
pub struct WalletClient {
    inner: Arc<WalletCoreServiceClient<Channel>>,
    enabled: bool,
}

impl WalletClient {
    /// Configure a client for `addr` (e.g. `http://wallet-core:50053`).
    ///
    /// The channel is created lazily: `connect_lazy` does not open a socket, so
    /// the betting engine still starts when wallet-core is booting.
    pub fn connect(addr: &str) -> Result<Self, WalletClientError> {
        let endpoint = Endpoint::from_shared(addr.to_string())
            .map_err(|e| WalletClientError::Unavailable(format!("invalid address {addr}: {e}")))?
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(5))
            .tcp_nodelay(true);

        let channel = endpoint
            .connect_lazy()
            .map_err(|e| WalletClientError::Unavailable(e.to_string()))?;

        info!(%addr, "wallet-core client configured");
        Ok(Self {
            inner: Arc::new(WalletCoreServiceClient::new(channel)),
            enabled: true,
        })
    }

    /// A client that reports success without moving money.
    ///
    /// Only valid for `APP__WALLET_ENABLED=false`, i.e. local development on a
    /// stack without wallet-core. Every call logs a warning.
    pub fn disabled() -> Self {
        warn!("wallet-core disabled: bets will NOT move money (development only)");
        Self {
            // Never dialled.
            inner: Arc::new(WalletCoreServiceClient::new(
                Channel::from_static("http://127.0.0.1:1").connect_lazy(),
            )),
            enabled: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Reserve the stake for a new bet.
    ///
    /// `reference_id` is the bet id. Used as the idempotency key too: replaying
    /// a placement returns the existing lock instead of reserving twice.
    pub async fn lock_stake(
        &self,
        user_id: i64,
        stake: Decimal,
        currency: &str,
        bet_id: i64,
    ) -> Result<(), WalletClientError> {
        if !self.enabled {
            warn!(%user_id, %stake, %currency, %bet_id, "wallet disabled: stake not locked");
            return Ok(());
        }

        let reference_id = bet_reference(bet_id);
        let mut client = self.inner.clone();
        let response = client
            .lock(LockRequest {
                user_id: Some(user_id_proto(user_id)),
                wallet_type: WALLET_TYPE_MAIN,
                amount: Some(money(stake, currency)),
                reference_id: reference_id.clone(),
                idempotency_key: reference_id,
            })
            .await
            .map_err(|status| map_status(status, user_id))?;

        let body = response.into_inner();
        if !body.success {
            return Err(WalletClientError::Rejected(error_message(body.error)));
        }
        Ok(())
    }

    /// Release a lock and return the money to the available balance.
    pub async fn unlock_stake(
        &self,
        user_id: i64,
        bet_id: i64,
    ) -> Result<(), WalletClientError> {
        if !self.enabled {
            warn!(%user_id, %bet_id, "wallet disabled: stake not unlocked");
            return Ok(());
        }

        let mut client = self.inner.clone();
        client
            .unlock(UnlockRequest {
                user_id: Some(user_id_proto(user_id)),
                reference_id: bet_reference(bet_id),
            })
            .await
            .map_err(|status| map_status(status, user_id))?;

        Ok(())
    }

    /// Credit a winning bet's payout.
    ///
    /// Must be preceded by [`Self::unlock_stake`] so the stake leaves the locked
    /// bucket; otherwise the player's locked balance would stay reserved.
    pub async fn credit_win(
        &self,
        user_id: i64,
        amount: Decimal,
        currency: &str,
        bet_id: i64,
    ) -> Result<(), WalletClientError> {
        if amount <= Decimal::ZERO {
            // Nothing to pay: a zero payout must not create a ledger row.
            return Ok(());
        }
        if !self.enabled {
            warn!(%user_id, %amount, "wallet disabled: payout not credited");
            return Ok(());
        }

        let mut client = self.inner.clone();
        let response = client
            .credit(CreditRequest {
                user_id: Some(user_id_proto(user_id)),
                wallet_type: WALLET_TYPE_MAIN,
                amount: Some(money(amount, currency)),
                reference_id: bet_reference(bet_id),
                reference_type: "bet_win".to_string(),
                // Distinct from the lock key so the two movements cannot collide.
                idempotency_key: format!("bet:{bet_id}:win"),
                description: "bet payout".to_string(),
                metadata: Default::default(),
            })
            .await
            .map_err(|status| map_status(status, user_id))?;

        if let Some(error) = response.into_inner().error {
            return Err(WalletClientError::Rejected(error_message(Some(error))));
        }
        Ok(())
    }

    /// Spend a lost bet's stake (already unlocked).
    pub async fn debit_stake(
        &self,
        user_id: i64,
        stake: Decimal,
        currency: &str,
        bet_id: i64,
    ) -> Result<(), WalletClientError> {
        if !self.enabled {
            warn!(%user_id, %stake, "wallet disabled: stake not debited");
            return Ok(());
        }

        let mut client = self.inner.clone();
        let response = client
            .debit(DebitRequest {
                user_id: Some(user_id_proto(user_id)),
                wallet_type: WALLET_TYPE_MAIN,
                amount: Some(money(stake, currency)),
                reference_id: bet_reference(bet_id),
                reference_type: "bet".to_string(),
                idempotency_key: format!("bet:{bet_id}:stake"),
                description: "bet stake settled".to_string(),
                metadata: Default::default(),
            })
            .await
            .map_err(|status| map_status(status, user_id))?;

        if let Some(error) = response.into_inner().error {
            return Err(WalletClientError::Rejected(error_message(Some(error))));
        }
        Ok(())
    }
}

/// Bet ids are BIGSERIAL, so the reference is plain decimal text.
fn bet_reference(bet_id: i64) -> String {
    bet_id.to_string()
}

fn user_id_proto(user_id: i64) -> UserId {
    // wallet-core parses this as `users.id` (BIGINT).
    UserId {
        value: user_id.to_string(),
    }
}

fn money(amount: Decimal, currency: &str) -> Money {
    Money {
        amount: amount.to_string(),
        currency: currency.to_string(),
    }
}

fn error_message(error: Option<crate::grpc::proto::common::v1::ErrorDetails>) -> String {
    error
        .map(|e| e.error_message)
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "wallet rejected the operation".to_string())
}

/// Keep "insufficient funds" distinguishable from transport failures so the API
/// can answer 422 instead of 500.
fn map_status(status: tonic::Status, user_id: i64) -> WalletClientError {
    match status.code() {
        tonic::Code::FailedPrecondition => WalletClientError::InsufficientFunds { user_id },
        tonic::Code::InvalidArgument | tonic::Code::NotFound => {
            WalletClientError::Rejected(status.message().to_string())
        }
        _ => WalletClientError::Unavailable(status.message().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bet_reference_is_the_plain_bet_id() {
        // wallet-core stores reference_id as VARCHAR(255); a UUID was required
        // before migration 023, which rejected every bet id.
        assert_eq!(bet_reference(12345), "12345");
    }

    #[test]
    fn user_id_is_sent_in_decimal_form() {
        assert_eq!(user_id_proto(42).value, "42");
    }

    #[test]
    fn money_keeps_eight_decimals() {
        let amount: Decimal = "10.12345678".parse().unwrap();
        let encoded = money(amount, "EUR");
        assert_eq!(encoded.amount, "10.12345678");
        assert_eq!(encoded.currency, "EUR");
    }

    #[test]
    fn insufficient_funds_is_distinguishable_from_outage() {
        let funds = map_status(tonic::Status::failed_precondition("no funds"), 7);
        assert!(matches!(funds, WalletClientError::InsufficientFunds { user_id: 7 }));

        let outage = map_status(tonic::Status::unavailable("down"), 7);
        assert!(matches!(outage, WalletClientError::Unavailable(_)));
    }

    #[tokio::test]
    async fn disabled_client_never_talks_to_the_network() {
        let client = WalletClient::disabled();
        let stake: Decimal = "5".parse().unwrap();
        assert!(client.lock_stake(1, stake, "USD", 9).await.is_ok());
        assert!(client.unlock_stake(1, 9).await.is_ok());
        assert!(client.debit_stake(1, stake, "USD", 9).await.is_ok());
        assert!(client.credit_win(1, stake, "USD", 9).await.is_ok());
    }
}
