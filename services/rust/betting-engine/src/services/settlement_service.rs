//! Bet settlement.
//!
//! Settlement used to be a pure status update: `bets.actual_win` was written and
//! no money moved, so a winning player was never paid. The service now performs
//! the wallet side of the settlement in this order:
//!
//! 1. read the bet (stake, currency, current status),
//! 2. move the money in wallet-core,
//! 3. flip the status in one transaction.
//!
//! Money first, status second: a retry after a crash between the two steps is
//! safe because every wallet call is idempotent on the bet id, while the status
//! update is guarded by `WHERE status = 'active'`. Doing it the other way round
//! would leave a settled bet whose payout was never attempted.

use rust_decimal::Decimal;
use tracing::{info, warn};

use crate::domain::bet::*;
use crate::errors::AppError;
use crate::repositories::bet_repo::BetRepository;
use crate::wallet::{WalletClient, WalletClientError};

#[derive(Clone)]
pub struct SettlementService {
    bet_repo: BetRepository,
    /// `None` when the engine runs without wallet-core (development only).
    wallet: Option<WalletClient>,
}

impl SettlementService {
    /// Settlement without money movements (unit tests / local development).
    pub fn new(bet_repo: BetRepository) -> Self {
        Self {
            bet_repo,
            wallet: None,
        }
    }

    /// Settlement that pays out through wallet-core.
    pub fn with_wallet(bet_repo: BetRepository, wallet: WalletClient) -> Self {
        Self {
            bet_repo,
            wallet: Some(wallet),
        }
    }

    #[tracing::instrument(name = "service.settle_bet", skip(self), fields(bet_id = %bet_id))]
    pub async fn settle_bet(
        &self,
        bet_id: BetId,
        result: &str,
        actual_win: Decimal,
    ) -> Result<Bet, AppError> {
        let target_status = match result {
            "won" => BetStatus::Won,
            "lost" => BetStatus::Lost,
            "void" => BetStatus::Void,
            _ => {
                return Err(AppError::Validation(vec![crate::errors::FieldError {
                    field: "result".into(),
                    message: "Must be 'won', 'lost', or 'void'".into(),
                }]))
            }
        };

        // `actual_win` is what the player receives; for a lost bet it must be 0
        // and for a won bet it must be positive. Rejecting nonsense here keeps
        // the ledger free of zero/negative payouts.
        match target_status {
            BetStatus::Won if actual_win <= Decimal::ZERO => {
                return Err(AppError::Validation(vec![crate::errors::FieldError {
                    field: "actual_win".into(),
                    message: "A winning bet must pay out a positive amount".into(),
                }]))
            }
            BetStatus::Lost | BetStatus::Void if actual_win != Decimal::ZERO => {
                // Voided bets refund the stake through `unlock`, not through
                // `actual_win`; a non-zero value would pay the player twice.
                warn!(bet_id = %bet_id, %actual_win, "Ignoring actual_win for a non-winning settlement");
            }
            _ => {}
        }

        let bet = self
            .bet_repo
            .get_bet(bet_id)
            .await
            .map_err(AppError::Database)?
            .ok_or_else(|| AppError::NotFound {
                entity: "Bet".into(),
                id: bet_id.to_string(),
            })?;

        if !bet.status.can_transition_to(target_status) {
            return Err(AppError::BetAlreadySettled);
        }

        // 1. Move the money. The stake was locked on placement, so every branch
        //    starts by releasing that lock.
        if let Some(wallet) = &self.wallet {
            self.settle_money(wallet, &bet, target_status, actual_win)
                .await
                .map_err(map_wallet_error)?;
        } else {
            warn!(bet_id = %bet_id, "wallet not configured: settlement moves no money");
        }

        // 2. Flip the status. `update_bet_status` matches on the previous status,
        //    so a concurrent settlement loses the race instead of paying twice.
        let mut tx = self
            .bet_repo
            .pool_ref()
            .begin()
            .await
            .map_err(AppError::Database)?;

        let settled = self
            .bet_repo
            .update_bet_status(
                &mut tx,
                bet_id,
                bet.status,
                target_status,
                Some(actual_win),
            )
            .await
            .map_err(|e| match e {
                sqlx::Error::RowNotFound => AppError::Conflict {
                    reason: "Bet was settled concurrently".into(),
                },
                other => AppError::Database(other),
            })?;

        tx.commit().await.map_err(AppError::Database)?;

        info!(
            bet_id = %bet_id,
            result = result,
            actual_win = %actual_win,
            "Bet settled"
        );

        Ok(settled)
    }

    /// Void a bet: release the stake back to the player.
    pub async fn void_bet(&self, bet_id: BetId) -> Result<Bet, AppError> {
        self.settle_bet(bet_id, "void", Decimal::ZERO).await
    }

    /// Wallet movements for a settlement.
    ///
    /// * won  — release the lock, then credit the payout;
    /// * lost — release the lock, then debit the stake;
    /// * void — release the lock (the stake returns to the player).
    async fn settle_money(
        &self,
        wallet: &WalletClient,
        bet: &Bet,
        target_status: BetStatus,
        actual_win: Decimal,
    ) -> Result<(), WalletClientError> {
        let user_id = bet.user_id.0;
        let bet_id = bet.id.0;

        match target_status {
            BetStatus::Won => {
                wallet.unlock_stake(user_id, bet_id).await?;
                wallet
                    .credit_win(user_id, actual_win, &bet.currency_code, bet_id)
                    .await
            }
            BetStatus::Lost => {
                wallet.unlock_stake(user_id, bet_id).await?;
                wallet
                    .debit_stake(user_id, bet.stake, &bet.currency_code, bet_id)
                    .await
            }
            // The stake goes back to available; nothing is credited.
            BetStatus::Void => wallet.unlock_stake(user_id, bet_id).await,
            other => {
                // Cashout has its own flow (CashoutService) and must never be
                // reached through settlement.
                Err(WalletClientError::Rejected(format!(
                    "unsupported settlement status: {other:?}"
                )))
            }
        }
    }
}

/// Map a wallet failure onto an API error.
fn map_wallet_error(err: WalletClientError) -> AppError {
    match err {
        WalletClientError::InsufficientFunds { .. } => AppError::InsufficientBalance {
            required: Decimal::ZERO,
            available: Decimal::ZERO,
        },
        WalletClientError::Rejected(reason) => AppError::BetRejected { reason },
        WalletClientError::Unavailable(reason) => AppError::ServiceUnavailable(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voided_bet_maps_to_a_client_error_when_the_wallet_rejects_it() {
        let err = map_wallet_error(WalletClientError::Rejected("no lock".into()));
        assert!(matches!(err, AppError::BetRejected { .. }));
    }

    #[test]
    fn wallet_outage_is_not_reported_as_a_client_error() {
        let err = map_wallet_error(WalletClientError::Unavailable("timeout".into()));
        assert!(matches!(err, AppError::ServiceUnavailable(_)));
    }
}
