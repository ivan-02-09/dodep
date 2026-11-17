//! gRPC service implementation

use std::sync::Arc;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::api::AppState;
use crate::domain::*;
// Aliased: the proto messages of the same names are glob-imported below.
use crate::infrastructure::repositories::{
    LedgerRepository, LockRepository, OutboxRepository, TransactionRepository, WalletRepository,
};
use crate::service::wallet_service::{
    CreditRequest as WalletCredit, DebitRequest as WalletDebit, TransferRequest as WalletTransfer,
};
use crate::service::*;

// Import generated proto types
use crate::proto::common::v1 as common_proto;
use crate::proto::common::v1::{Money, TransactionId, UserId};
use crate::proto::wallet::v1::wallet_core_service_server::{
    WalletCoreService, WalletCoreServiceServer,
};
use crate::proto::wallet::v1::*;

/// gRPC Wallet Core Service
pub struct WalletGrpcService {
    wallet_service: Arc<WalletService>,
    /// Fallback currency when a request omits Money.currency. Comes from
    /// APP__DEFAULT_CURRENCY so a multi-currency deployment does not silently
    /// open every new wallet in USD.
    default_currency: String,
}

impl WalletGrpcService {
    pub fn new(state: Arc<AppState>) -> Self {
        let wallet_repo = Arc::new(WalletRepository::new(state.db_pool.clone()));
        let transaction_repo = Arc::new(TransactionRepository::new(state.db_pool.clone()));
        let ledger_repo = Arc::new(LedgerRepository::new(state.db_pool.clone()));
        let lock_repo = Arc::new(LockRepository::new(state.db_pool.clone()));
        let outbox_repo = Arc::new(OutboxRepository::new(state.db_pool.clone()));
        let idempotency = Arc::new(IdempotencyService::new(
            state.redis_client.clone(),
            3600, // 1 hour TTL
        ));

        let wallet_service = Arc::new(WalletService::new(
            wallet_repo,
            transaction_repo,
            ledger_repo,
            lock_repo,
            outbox_repo,
            idempotency,
        ));

        let default_currency = state
            .config
            .app
            .default_currency
            .trim()
            .to_uppercase();

        Self {
            wallet_service,
            default_currency,
        }
    }

    /// Currency for a money-moving RPC: the request's ISO code when present,
    /// otherwise the deployment default. Never a hardcoded constant.
    fn resolve_currency(&self, requested: Option<String>) -> String {
        match requested {
            Some(code) if !code.trim().is_empty() => code.trim().to_uppercase(),
            _ => self.default_currency.clone(),
        }
    }

    pub fn into_service(self) -> WalletCoreServiceServer<Self> {
        WalletCoreServiceServer::new(self)
    }

    /// Serve gRPC server.
    ///
    /// The error type is boxed with `Send + Sync` so the future can be moved
    /// across threads by `tokio::spawn`.
    pub async fn serve(
        addr: std::net::SocketAddr,
        state: Arc<AppState>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let service = Self::new(state);

        tonic::transport::Server::builder()
            .add_service(service.into_service())
            .serve(addr)
            .await?;

        Ok(())
    }
}

#[tonic::async_trait]
impl WalletCoreService for WalletGrpcService {
    async fn get_balance(
        &self,
        request: Request<GetBalanceRequest>,
    ) -> Result<Response<GetBalanceResponse>, Status> {
        let req = request.into_inner();

        let user_id =
            parse_user_id(&req.user_id).map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        let wallet_type = parse_wallet_type(
            common_proto::WalletType::try_from(req.wallet_type)
                .map_err(|_| Status::invalid_argument("Invalid wallet_type"))?,
        )?;

        // The wallet's currency, not a hardcoded "USD": clients display the
        // Balance.currency they receive, and the wallet row is authoritative.
        // GetBalanceRequest carries no currency field, so the wallet itself is
        // the source (get_balance returns amounts only).
        let currency = match self
            .wallet_service
            .get_currency(user_id, wallet_type)
            .await
        {
            Ok(currency) => currency,
            Err(WalletError::NotFound { .. }) => {
                return Err(Status::not_found("Wallet not found"))
            }
            Err(e) => return Err(Status::internal(e.to_string())),
        };

        match self.wallet_service.get_balance(user_id, wallet_type).await {
            Ok(balance) => {
                let response = GetBalanceResponse {
                    balance: Some(common_proto::Balance {
                        available: Some(money_from_decimal(balance.available, &currency)),
                        locked: Some(money_from_decimal(balance.locked, &currency)),
                        bonus: Some(money_from_decimal(balance.bonus, &currency)),
                        total: Some(money_from_decimal(balance.total(), &currency)),
                    }),
                    version: 0,
                    updated_at: None,
                };
                Ok(Response::new(response))
            }
            Err(WalletError::NotFound { .. }) => Err(Status::not_found("Wallet not found")),
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn get_wallets(
        &self,
        request: Request<GetWalletsRequest>,
    ) -> Result<Response<GetWalletsResponse>, Status> {
        let req = request.into_inner();

        let user_id =
            parse_user_id(&req.user_id).map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        // Get all wallets for user. Errors used to be swallowed by
        // `unwrap_or_default()`, which turned a broken database into an empty
        // (and therefore plausible) wallet list.
        let wallet = self
            .wallet_service
            .get_or_create_wallet(user_id, WalletType::Main, &self.default_currency)
            .await
            .map_err(|e| match e {
                WalletError::CurrencyMismatch { .. } => Status::invalid_argument(e.to_string()),
                _ => Status::internal(e.to_string()),
            })?;

        let response = GetWalletsResponse {
            wallets: vec![wallet_to_proto(&wallet)],
        };

        Ok(Response::new(response))
    }

    async fn credit(
        &self,
        request: Request<CreditRequest>,
    ) -> Result<Response<CreditResponse>, Status> {
        let req = request.into_inner();

        let user_id =
            parse_user_id(&req.user_id).map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        let wallet_type = parse_wallet_type(
            common_proto::WalletType::try_from(req.wallet_type)
                .map_err(|_| Status::invalid_argument("Invalid wallet_type"))?,
        )?;

        let (amount, currency) =
            parse_money(&req.amount).map_err(|_| Status::invalid_argument("Invalid amount"))?;

        let reference_id = parse_reference_id(&req.reference_id)?;

        match self
            .wallet_service
            .credit(WalletCredit {
                user_id,
                wallet_type,
                amount,
                currency: self.resolve_currency(currency),
                reference_id,
                reference_type: req.reference_type.clone(),
                idempotency_key: req.idempotency_key.clone(),
            })
            .await
        {
            Ok(txn) => {
                let response = CreditResponse {
                    transaction: Some(transaction_to_proto(&txn)),
                    new_balance: None,
                    error: None,
                };
                Ok(Response::new(response))
            }
            Err(e @ WalletError::CurrencyMismatch { .. }) => {
                Err(Status::invalid_argument(e.to_string()))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn debit(
        &self,
        request: Request<DebitRequest>,
    ) -> Result<Response<DebitResponse>, Status> {
        let req = request.into_inner();

        let user_id =
            parse_user_id(&req.user_id).map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        let wallet_type = parse_wallet_type(
            common_proto::WalletType::try_from(req.wallet_type)
                .map_err(|_| Status::invalid_argument("Invalid wallet_type"))?,
        )?;

        let (amount, currency) =
            parse_money(&req.amount).map_err(|_| Status::invalid_argument("Invalid amount"))?;

        let reference_id = parse_reference_id(&req.reference_id)?;

        match self
            .wallet_service
            .debit(WalletDebit {
                user_id,
                wallet_type,
                amount,
                currency: self.resolve_currency(currency),
                reference_id,
                reference_type: req.reference_type.clone(),
                idempotency_key: req.idempotency_key.clone(),
            })
            .await
        {
            Ok(txn) => {
                let response = DebitResponse {
                    transaction: Some(transaction_to_proto(&txn)),
                    new_balance: None,
                    error: None,
                };
                Ok(Response::new(response))
            }
            Err(WalletError::InsufficientAvailableBalance { .. })
            | Err(WalletError::InsufficientBalance { .. }) => {
                Err(Status::failed_precondition("Insufficient balance"))
            }
            Err(e @ WalletError::CurrencyMismatch { .. }) => {
                Err(Status::invalid_argument(e.to_string()))
            }
            Err(e @ WalletError::NotFound { .. }) => Err(Status::not_found(e.to_string())),
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn lock(&self, request: Request<LockRequest>) -> Result<Response<LockResponse>, Status> {
        let req = request.into_inner();

        let user_id =
            parse_user_id(&req.user_id).map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        let wallet_type = parse_wallet_type(
            common_proto::WalletType::try_from(req.wallet_type)
                .map_err(|_| Status::invalid_argument("Invalid wallet_type"))?,
        )?;

        // Locking only moves funds between sub-balances of one wallet, so the
        // currency comes from the wallet and never from the request.
        let amount = parse_money_amount(&req.amount)
            .map_err(|_| Status::invalid_argument("Invalid amount"))?;

        let reference_id = parse_reference_id(&req.reference_id)?;

        match self
            .wallet_service
            .lock(user_id, wallet_type, amount, reference_id)
            .await
        {
            Ok(lock) => {
                let response = LockResponse {
                    success: true,
                    new_available: None,
                    new_locked: None,
                    error: None,
                };
                tracing::debug!(lock_id = %lock.id, amount = %lock.amount, "Funds locked");
                Ok(Response::new(response))
            }
            Err(WalletError::InsufficientAvailableBalance { .. }) => {
                Err(Status::failed_precondition("Insufficient balance"))
            }
            Err(e @ WalletError::NotFound { .. }) => Err(Status::not_found(e.to_string())),
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn unlock(
        &self,
        request: Request<UnlockRequest>,
    ) -> Result<Response<UnlockResponse>, Status> {
        let req = request.into_inner();

        let user_id =
            parse_user_id(&req.user_id).map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        let reference_id = parse_reference_id(&req.reference_id)?;

        match self.wallet_service.unlock(user_id, &reference_id).await {
            Ok(_) => {
                let response = UnlockResponse {
                    success: true,
                    new_available: None,
                    new_locked: None,
                    error: None,
                };
                Ok(Response::new(response))
            }
            Err(WalletError::LockReferenceNotFound(_)) => Err(Status::not_found("Lock not found")),
            Err(e @ WalletError::NotFound { .. }) => Err(Status::not_found(e.to_string())),
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn transfer(
        &self,
        request: Request<TransferRequest>,
    ) -> Result<Response<TransferResponse>, Status> {
        let req = request.into_inner();

        let user_id =
            parse_user_id(&req.user_id).map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        let from_wallet_type = parse_wallet_type(
            common_proto::WalletType::try_from(req.from_wallet)
                .map_err(|_| Status::invalid_argument("Invalid from_wallet"))?,
        )?;
        let to_wallet_type = parse_wallet_type(
            common_proto::WalletType::try_from(req.to_wallet)
                .map_err(|_| Status::invalid_argument("Invalid to_wallet"))?,
        )?;

        let (amount, currency) =
            parse_money(&req.amount).map_err(|_| Status::invalid_argument("Invalid amount"))?;

        let reference_id = parse_reference_id(&req.reference_id)?;

        match self
            .wallet_service
            .transfer(WalletTransfer {
                user_id,
                from_wallet_type,
                to_wallet_type,
                amount,
                currency: self.resolve_currency(currency),
                reference_id,
                idempotency_key: (!req.idempotency_key.is_empty())
                    .then(|| req.idempotency_key.clone()),
            })
            .await
        {
            Ok((debit_txn, credit_txn)) => {
                let response = TransferResponse {
                    debit_transaction: Some(transaction_to_proto(&debit_txn)),
                    credit_transaction: Some(transaction_to_proto(&credit_txn)),
                    error: None,
                };
                Ok(Response::new(response))
            }
            Err(e @ WalletError::CurrencyMismatch { .. }) => {
                Err(Status::invalid_argument(e.to_string()))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn get_transactions(
        &self,
        request: Request<GetTransactionsRequest>,
    ) -> Result<Response<GetTransactionsResponse>, Status> {
        let req = request.into_inner();

        let user_id =
            parse_user_id(&req.user_id).map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        let (limit, offset) = match req.pagination {
            Some(p) => {
                let l = p.page_size.clamp(1, 100) as i64;
                let o = p.cursor.parse::<i64>().unwrap_or(0).max(0);
                (l, o)
            }
            None => (20, 0),
        };

        // `unwrap_or_default()` used to turn a failing query into an empty page,
        // which looks like "the user has no transactions" instead of an error.
        let transactions = self
            .wallet_service
            .get_transactions(user_id, limit, offset)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let response = GetTransactionsResponse {
            transactions: transactions.iter().map(transaction_to_proto).collect(),
            pagination: Some(common_proto::PageResponse {
                next_cursor: (offset + limit).to_string(),
                prev_cursor: offset.to_string(),
                has_more: transactions.len() as i64 == limit,
                total_count: Some(0),
                current_page: Some((offset / limit + 1) as i32),
                total_pages: Some(1),
            }),
        };

        Ok(Response::new(response))
    }

    async fn get_transaction(
        &self,
        request: Request<GetTransactionRequest>,
    ) -> Result<Response<GetTransactionResponse>, Status> {
        let req = request.into_inner();

        let tx_id = req
            .transaction_id
            .as_ref()
            .and_then(|id| Uuid::parse_str(&id.value).ok())
            .ok_or_else(|| Status::invalid_argument("Invalid transaction_id"))?;

        let transaction =
            self.wallet_service
                .get_transaction(tx_id)
                .await
                .map_err(|e| match e {
                    WalletError::TransactionNotFound(_) => {
                        Status::not_found("Transaction not found")
                    }
                    _ => Status::internal(e.to_string()),
                })?;

        let response = GetTransactionResponse {
            transaction: Some(transaction_to_proto(&transaction)),
        };

        Ok(Response::new(response))
    }
}

// Helper functions

/// Invalid-argument detail for the request parsers.
///
/// `tonic::Status` is ~176 bytes, so returning it directly from these helpers
/// would bloat every `Result`; this small type converts at the call sites via
/// `?`.
#[derive(Debug)]
struct InvalidArg(&'static str);

impl From<InvalidArg> for Status {
    fn from(err: InvalidArg) -> Self {
        Status::invalid_argument(err.0)
    }
}

/// Parse helpers return `InvalidArg` so `?` works in the RPC bodies.
///
/// Users are keyed by `users.id BIGSERIAL` and every client (Go services,
/// betting-engine) sends the decimal form. Reading it as a UUID made every
/// wallet call fail with `INVALID_ARGUMENT: Invalid user_id` before any ledger
/// work happened; `023_wallet_lock_settlement.sql` already retyped the columns.
fn parse_user_id(proto: &Option<UserId>) -> Result<i64, InvalidArg> {
    proto
        .as_ref()
        .map(|id| id.value.trim())
        .and_then(|raw| raw.parse::<i64>().ok())
        .ok_or(InvalidArg("Invalid user_id"))
}

/// `reference_id` is a VARCHAR(255) business key (bet id, withdrawal id, …),
/// not necessarily a UUID, so it is passed through as-is. It must not be empty
/// because locks are de-duplicated by it.
fn parse_reference_id(raw: &str) -> Result<String, Status> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Status::invalid_argument("Invalid reference_id"));
    }
    Ok(trimmed.to_string())
}

fn parse_wallet_type(proto: common_proto::WalletType) -> Result<WalletType, InvalidArg> {
    match proto {
        common_proto::WalletType::Unspecified => Ok(WalletType::Main),
        common_proto::WalletType::Main => Ok(WalletType::Main),
        common_proto::WalletType::Bonus => Ok(WalletType::Bonus),
        common_proto::WalletType::FreeSpins => Ok(WalletType::FreeSpins),
        common_proto::WalletType::Cashback => Ok(WalletType::Cashback),
    }
}

/// Split a `Money` into its amount and its optional ISO currency.
///
/// The currency used to be dropped (and replaced with "USD" further down), so a
/// wallet created in EUR recorded every subsequent movement as USD while the
/// ledger account id - which embeds the currency - disagreed with the wallet row.
fn parse_money(proto: &Option<Money>) -> Result<(rust_decimal::Decimal, Option<String>), InvalidArg> {
    let money = proto.as_ref().ok_or(InvalidArg("Invalid amount"))?;
    let amount = rust_decimal::Decimal::from_str_exact(&money.amount)
        .map_err(|_| InvalidArg("Invalid amount"))?;
    let currency = (!money.currency.trim().is_empty()).then(|| money.currency.clone());
    Ok((amount, currency))
}

/// Amount only, for RPCs that cannot change the currency (lock/unlock).
fn parse_money_amount(proto: &Option<Money>) -> Result<rust_decimal::Decimal, InvalidArg> {
    parse_money(proto).map(|(amount, _)| amount)
}

fn money_from_decimal(amount: rust_decimal::Decimal, currency: &str) -> Money {
    Money {
        amount: amount.to_string(),
        currency: currency.to_string(),
    }
}

fn wallet_to_proto(wallet: &Wallet) -> common_proto::Wallet {
    common_proto::Wallet {
        user_id: Some(UserId {
            value: wallet.user_id.to_string(),
        }),
        r#type: wallet_type_to_proto(wallet.wallet_type),
        balance: Some(common_proto::Balance {
            available: Some(money_from_decimal(
                wallet.balance_available,
                &wallet.currency,
            )),
            locked: Some(money_from_decimal(wallet.balance_locked, &wallet.currency)),
            bonus: Some(money_from_decimal(
                rust_decimal::Decimal::ZERO,
                &wallet.currency,
            )),
            total: Some(money_from_decimal(
                wallet.balance_available + wallet.balance_locked,
                &wallet.currency,
            )),
        }),
        currency: wallet.currency.clone(),
        created_at: Some(dt_to_timestamp(wallet.created_at)),
        updated_at: Some(dt_to_timestamp(wallet.updated_at)),
        is_active: wallet.is_active,
    }
}

fn dt_to_timestamp(dt: chrono::DateTime<chrono::Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: dt.timestamp(),
        nanos: dt.timestamp_subsec_nanos() as i32,
    }
}

fn wallet_type_to_proto(wallet_type: WalletType) -> i32 {
    match wallet_type {
        WalletType::Main => common_proto::WalletType::Main as i32,
        WalletType::Bonus => common_proto::WalletType::Bonus as i32,
        WalletType::FreeSpins => common_proto::WalletType::FreeSpins as i32,
        WalletType::Cashback => common_proto::WalletType::Cashback as i32,
    }
}

fn transaction_to_proto(txn: &Transaction) -> common_proto::Transaction {
    common_proto::Transaction {
        id: Some(TransactionId {
            value: txn.id.to_string(),
        }),
        user_id: Some(UserId {
            value: txn.user_id.to_string(),
        }),
        wallet_type: wallet_type_to_proto(txn.wallet_type),
        amount: Some(money_from_decimal(txn.amount, &txn.currency)),
        r#type: transaction_type_to_proto(txn.transaction_type) as i32,
        reference_id: txn.reference_id.clone().unwrap_or_default(),
        reference_type: txn.reference_type.clone().unwrap_or_default(),
        idempotency_key: txn.idempotency_key.clone().unwrap_or_default(),
        created_at: Some(dt_to_timestamp(txn.created_at)),
        updated_at: Some(dt_to_timestamp(txn.updated_at)),
        status: transaction_status_to_proto(&txn.status) as i32,
        description: txn.description.clone().unwrap_or_default(),
        metadata: std::collections::HashMap::new(),
    }
}

fn transaction_type_to_proto(t: TransactionType) -> common_proto::TransactionType {
    match t {
        TransactionType::Deposit => common_proto::TransactionType::Deposit,
        TransactionType::Withdrawal => common_proto::TransactionType::Withdrawal,
        TransactionType::BetPlace => common_proto::TransactionType::BetPlace,
        TransactionType::BetWin => common_proto::TransactionType::BetWin,
        TransactionType::BetRefund => common_proto::TransactionType::BetRefund,
        TransactionType::BonusCredit => common_proto::TransactionType::BonusCredit,
        TransactionType::BonusDebit => common_proto::TransactionType::BonusDebit,
        TransactionType::Transfer => common_proto::TransactionType::Transfer,
        TransactionType::Adjustment => common_proto::TransactionType::Adjustment,
        // The proto enum has no dedicated wire value for the directional
        // transfer legs or fees; they all travel as `TRANSFER`.
        TransactionType::TransferIn | TransactionType::TransferOut => {
            common_proto::TransactionType::Transfer
        }
        TransactionType::Fee => common_proto::TransactionType::Adjustment,
    }
}

fn transaction_status_to_proto(s: &TransactionStatus) -> common_proto::TransactionStatus {
    match s {
        TransactionStatus::Pending | TransactionStatus::Processing => {
            common_proto::TransactionStatus::Pending
        }
        TransactionStatus::Completed => common_proto::TransactionStatus::Completed,
        TransactionStatus::Failed => common_proto::TransactionStatus::Failed,
        TransactionStatus::Cancelled => common_proto::TransactionStatus::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(value: &str) -> Option<UserId> {
        Some(UserId {
            value: value.to_string(),
        })
    }

    #[test]
    fn user_id_accepts_decimal_ids_sent_by_go_services() {
        // `payment` and `casino` send strconv.FormatInt(userID, 10).
        assert_eq!(parse_user_id(&user("42")).unwrap(), 42);
        assert_eq!(parse_user_id(&user(" 7 ")).unwrap(), 7);
    }

    #[test]
    fn user_id_rejects_anything_that_is_not_a_number() {
        assert!(parse_user_id(&user("abc")).is_err());
        assert!(parse_user_id(&user("")).is_err());
        assert!(parse_user_id(&None).is_err());
    }

    #[test]
    fn reference_id_is_a_business_key_not_a_uuid() {
        // Bet ids are BIGSERIAL; they used to be rejected as "Invalid
        // reference_id" because the parser demanded a UUID.
        assert_eq!(parse_reference_id("123").unwrap(), "123");
        assert_eq!(parse_reference_id(" bet-9 ").unwrap(), "bet-9");
        assert!(parse_reference_id("").is_err());
        assert!(parse_reference_id("   ").is_err());
    }

    #[test]
    fn money_currency_is_optional_and_amount_is_exact() {
        let (amount, currency) = parse_money(&Some(Money {
            amount: "10.50000000".to_string(),
            currency: "eur".to_string(),
        }))
        .unwrap();
        assert_eq!(amount.to_string(), "10.50000000");
        assert_eq!(currency.as_deref(), Some("eur"));

        let (_, currency) = parse_money(&Some(Money {
            amount: "1".to_string(),
            currency: "  ".to_string(),
        }))
        .unwrap();
        assert!(currency.is_none(), "blank currency must fall back to default");
    }

    #[test]
    fn lock_ignores_currency_because_it_cannot_change_it() {
        let amount = parse_money_amount(&Some(Money {
            amount: "3".to_string(),
            currency: "GBP".to_string(),
        }))
        .unwrap();
        assert_eq!(amount.to_string(), "3");
    }
}
