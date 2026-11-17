use rust_decimal::Decimal;
use tracing::info;
// Needed for `PlaceBetRequest::validate()` (trait method from `#[derive(Validate)]`).
use validator::Validate;

use crate::domain::bet::*;
use crate::domain::odds::{calculate_combined_odds, calculate_potential_win};
use crate::errors::AppError;
use crate::repositories::bet_repo::{BetRepository, CreateBetParams, CreateSelectionParams};
use crate::wallet::{WalletClient, WalletClientError};

const MIN_STAKE: &str = "0.10";
const MAX_STAKE: &str = "10000.00";
const MAX_PAYOUT: &str = "100000.00";

#[derive(Clone)]
pub struct BetService {
    bet_repo: BetRepository,
    /// Money channel. `None` means the engine runs without wallet-core (local
    /// development only) — the bet is then booked without reserving funds.
    wallet: Option<WalletClient>,
    currency: String,
}

impl BetService {
    /// Service without a wallet connection (development / unit tests).
    pub fn new(bet_repo: BetRepository) -> Self {
        Self {
            bet_repo,
            wallet: None,
            currency: "USD".to_string(),
        }
    }

    /// Service that reserves the stake in wallet-core before booking the bet.
    pub fn with_wallet(bet_repo: BetRepository, wallet: WalletClient, currency: &str) -> Self {
        Self {
            bet_repo,
            wallet: Some(wallet),
            currency: currency.to_string(),
        }
    }

    #[tracing::instrument(name = "service.place_bet", skip(self, req), fields(user_id = %user_id))]
    pub async fn place_bet(&self, user_id: UserId, req: PlaceBetRequest) -> Result<Bet, AppError> {
        req.validate()?;

        if req.selections.is_empty() {
            return Err(AppError::Validation(vec![crate::errors::FieldError {
                field: "selections".into(),
                message: "At least one selection required".into(),
            }]));
        }

        let combined_odds = calculate_combined_odds(&req.selections, req.bet_type);
        let potential_win = calculate_potential_win(req.stake, combined_odds);

        self.validate_stake(req.stake, potential_win)?;

        let event_id = if req.selections.len() == 1 {
            Some(EventId(req.selections[0].event_id))
        } else {
            None
        };

        let selections: Vec<CreateSelectionParams> = req
            .selections
            .iter()
            .map(|s| CreateSelectionParams {
                event_id: EventId(s.event_id),
                market_id: MarketId(s.market_id),
                outcome_id: OutcomeId(s.outcome_id),
                odds: s.odds,
            })
            .collect();

        // A bet must always carry the currency it was priced in; falling back to
        // a hardcoded "USD" would book EUR stakes as dollars.
        let currency = if req.currency_code.trim().is_empty() {
            self.currency.clone()
        } else {
            req.currency_code.trim().to_uppercase()
        };

        let bet = self
            .bet_repo
            .create_bet(CreateBetParams {
                user_id,
                bet_type: req.bet_type,
                stake: req.stake,
                combined_odds,
                potential_win,
                currency_code: currency.clone(),
                sport_id: None,
                event_id,
                selections,
                idempotency_key: req.idempotency_key,
                ip_address: req.ip_address,
                device_fingerprint: req.device_fingerprint,
            })
            .await?;

        // Reserve the stake. The bet id is the wallet reference, so a retried
        // request reuses the existing lock instead of reserving twice.
        if let Some(wallet) = &self.wallet {
            if let Err(err) = wallet
                .lock_stake(user_id.0, bet.stake, &bet.currency_code, bet.id.0)
                .await
            {
                self.bet_repo
                    .reject_bet(bet.id, &format!("stake lock failed: {err}"))
                    .await;
                return Err(map_wallet_error(err));
            }
        }

        info!(
            user_id = %user_id,
            bet_id = %bet.id,
            stake = %bet.stake,
            odds = %combined_odds,
            %currency,
            "Bet placed"
        );

        Ok(bet)
    }

    /// Wallet client, when the engine is wired to wallet-core.
    pub fn wallet(&self) -> Option<&WalletClient> {
        self.wallet.as_ref()
    }

    #[tracing::instrument(name = "service.get_bet", skip(self))]
    pub async fn get_bet(&self, user_id: UserId, bet_id: BetId) -> Result<Bet, AppError> {
        self.bet_repo
            .get_bet_by_id(bet_id, user_id)
            .await?
            .ok_or_else(|| AppError::NotFound {
                entity: "Bet".into(),
                id: bet_id.to_string(),
            })
    }

    #[tracing::instrument(name = "service.get_history", skip(self))]
    pub async fn get_history(
        &self,
        user_id: UserId,
        limit: i64,
        cursor: Option<i64>,
        status: Option<BetStatus>,
    ) -> Result<PaginatedResponse<BetResponse>, AppError> {
        let (bets, total) = self
            .bet_repo
            .get_user_bets(user_id, limit, cursor, status)
            .await?;

        let next_cursor = bets.last().map(|b| b.id.0.to_string());

        Ok(PaginatedResponse {
            data: bets.into_iter().map(BetResponse::from).collect(),
            total,
            page_size: limit,
            cursor: next_cursor,
        })
    }

    fn validate_stake(&self, stake: Decimal, potential_win: Decimal) -> Result<(), AppError> {
        let min = Decimal::try_from(MIN_STAKE).unwrap();
        let max = Decimal::try_from(MAX_STAKE).unwrap();
        let max_payout = Decimal::try_from(MAX_PAYOUT).unwrap();

        if stake < min {
            return Err(AppError::BetStakeTooLow { min, actual: stake });
        }

        if stake > max {
            return Err(AppError::BetStakeTooHigh { max, actual: stake });
        }

        if potential_win > max_payout {
            return Err(AppError::BetMaxPayoutExceeded {
                max: max_payout,
                potential: potential_win,
            });
        }

        Ok(())
    }
}

/// Map a wallet failure onto an API error and a stable client-facing message.
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
    fn insufficient_funds_maps_to_a_client_error() {
        let err = map_wallet_error(WalletClientError::InsufficientFunds { user_id: 1 });
        assert!(matches!(err, AppError::InsufficientBalance { .. }));
    }

    #[test]
    fn wallet_outage_maps_to_service_unavailable() {
        let err = map_wallet_error(WalletClientError::Unavailable("down".into()));
        assert!(matches!(err, AppError::ServiceUnavailable(_)));
    }

    #[test]
    fn rejection_maps_to_bet_rejected() {
        let err = map_wallet_error(WalletClientError::Rejected("no".into()));
        assert!(matches!(err, AppError::BetRejected { .. }));
    }
}
