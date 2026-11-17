use rust_decimal::Decimal;
use serde::Serialize;
use tracing::info;

use crate::domain::bet::*;
use crate::domain::odds::calculate_cashout_value;
use crate::errors::AppError;
use crate::repositories::bet_repo::BetRepository;
use crate::wallet::{WalletClient, WalletClientError};

const CASHOUT_MARGIN: &str = "0.05"; // 5% house margin on cashout

#[derive(Debug, Serialize)]
pub struct CashoutResponse {
    pub bet_id: i64,
    pub cashout_value: String,
    pub original_stake: String,
    pub status: String,
}

#[derive(Clone)]
pub struct CashoutService {
    bet_repo: BetRepository,
    /// `None` when the engine runs without wallet-core (development only).
    wallet: Option<WalletClient>,
}

impl CashoutService {
    pub fn new(bet_repo: BetRepository) -> Self {
        Self {
            bet_repo,
            wallet: None,
        }
    }

    /// Cashout that actually pays the player through wallet-core.
    pub fn with_wallet(bet_repo: BetRepository, wallet: WalletClient) -> Self {
        Self {
            bet_repo,
            wallet: Some(wallet),
        }
    }

    #[tracing::instrument(name = "service.cashout", skip(self), fields(user_id = %user_id, bet_id = %bet_id))]
    pub async fn cashout(
        &self,
        user_id: UserId,
        bet_id: BetId,
    ) -> Result<CashoutResponse, AppError> {
        let bet = self
            .bet_repo
            .get_bet_by_id(bet_id, user_id)
            .await?
            .ok_or_else(|| AppError::NotFound {
                entity: "Bet".into(),
                id: bet_id.to_string(),
            })?;

        // Bets are created as `pending` and are only ever activated explicitly,
        // so requiring `active` here made cashout impossible for real bets.
        if !matches!(bet.status, BetStatus::Active | BetStatus::Pending) {
            return Err(AppError::CashoutUnavailable);
        }

        let margin = Decimal::try_from(CASHOUT_MARGIN).unwrap();

        // In production, current_odds would come from odds feed service
        // For now, use a simplified calculation: assume odds dropped by 20%
        let current_odds = bet.combined_odds * Decimal::try_from("0.80").unwrap();

        let cashout_value =
            calculate_cashout_value(current_odds, bet.combined_odds, bet.stake, margin);

        if cashout_value <= Decimal::ZERO {
            return Err(AppError::CashoutUnavailable);
        }

        // Move the money first: release the locked stake, then credit the
        // cashout value. Both calls are idempotent on the bet id, so a retry is
        // safe; doing it after the status update would risk a cancelled bet that
        // was never paid.
        if let Some(wallet) = &self.wallet {
            wallet
                .unlock_stake(user_id.0, bet_id.0)
                .await
                .map_err(map_wallet_error)?;
            wallet
                .credit_win(user_id.0, cashout_value, &bet.currency_code, bet_id.0)
                .await
                .map_err(map_wallet_error)?;
        }

        // Transition bet to Cashout status
        let mut tx = self
            .bet_repo
            .pool_ref()
            .begin()
            .await
            .map_err(AppError::Database)?;

        self.bet_repo
            .update_bet_status(
                &mut tx,
                bet_id,
                bet.status,
                BetStatus::Cashout,
                Some(cashout_value),
            )
            .await
            .map_err(|e| match e {
                sqlx::Error::RowNotFound => AppError::Conflict {
                    reason: "Bet was cashed out concurrently".into(),
                },
                other => AppError::Database(other),
            })?;

        tx.commit().await.map_err(AppError::Database)?;

        info!(
            bet_id = %bet_id,
            cashout_value = %cashout_value,
            original_stake = %bet.stake,
            "Bet cashed out"
        );

        Ok(CashoutResponse {
            bet_id: bet_id.0,
            cashout_value: cashout_value.to_string(),
            original_stake: bet.stake.to_string(),
            status: "cashout".into(),
        })
    }
}

/// Map a wallet failure onto an API error.
fn map_wallet_error(err: WalletClientError) -> AppError {
    match err {
        WalletClientError::InsufficientFunds { .. } => AppError::CashoutUnavailable,
        WalletClientError::Rejected(reason) => AppError::BetRejected { reason },
        WalletClientError::Unavailable(reason) => AppError::ServiceUnavailable(reason),
    }
}
