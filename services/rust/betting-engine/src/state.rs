use std::sync::Arc;

use crate::config::AppConfig;
use crate::events::EventProducer;
use crate::repositories::bet_repo::BetRepository;
use crate::services::bet_service::BetService;
use crate::services::cashout_service::CashoutService;
use crate::services::settlement_service::SettlementService;
use crate::wallet::WalletClient;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    config: AppConfig,
    db_pool: sqlx::PgPool,
    bet_service: BetService,
    cashout_service: CashoutService,
    settlement_service: SettlementService,
    event_producer: Option<EventProducer>,
    /// `None` when `APP__WALLET_ENABLED=false` (development only).
    wallet: Option<WalletClient>,
}

impl AppState {
    pub fn new(config: AppConfig, db_pool: sqlx::PgPool) -> Self {
        let bet_repo = BetRepository::new(db_pool.clone());

        // Money movements are wired in from the start: a betting engine without a
        // wallet books stakes it never collected and wins it never paid.
        let wallet = Self::build_wallet(&config);

        let bet_service = match &wallet {
            Some(wallet) => {
                BetService::with_wallet(bet_repo.clone(), wallet.clone(), &config.currency())
            }
            None => BetService::new(bet_repo.clone()),
        };
        let cashout_service = match &wallet {
            Some(wallet) => CashoutService::with_wallet(bet_repo.clone(), wallet.clone()),
            None => CashoutService::new(bet_repo.clone()),
        };
        let settlement_service = match &wallet {
            Some(wallet) => SettlementService::with_wallet(bet_repo, wallet.clone()),
            None => SettlementService::new(bet_repo),
        };

        let event_producer = EventProducer::new(&config.redpanda_brokers).ok();

        Self {
            inner: Arc::new(AppStateInner {
                config,
                db_pool,
                bet_service,
                cashout_service,
                settlement_service,
                event_producer,
                wallet,
            }),
        }
    }

    /// Build the wallet client.
    ///
    /// `APP__WALLET_ENABLED=false` yields a no-op client that logs a warning on
    /// every call (development only; `AppConfig::validate` rejects it in
    /// production). `WalletClient::connect` builds a lazy channel, so an
    /// unreachable wallet-core does not prevent start-up — it surfaces as a 503
    /// on the first money movement instead of a silently unpaid bet.
    fn build_wallet(config: &AppConfig) -> Option<WalletClient> {
        if !config.wallet_enabled {
            return Some(WalletClient::disabled());
        }
        match WalletClient::connect(&config.wallet_service_addr) {
            Ok(client) => Some(client),
            Err(error) => {
                tracing::error!(%error, "Failed to configure wallet-core client");
                None
            }
        }
    }

    pub fn config(&self) -> &AppConfig {
        &self.inner.config
    }

    pub fn db_pool(&self) -> &sqlx::PgPool {
        &self.inner.db_pool
    }

    pub fn bet_service(&self) -> &BetService {
        &self.inner.bet_service
    }

    pub fn cashout_service(&self) -> &CashoutService {
        &self.inner.cashout_service
    }

    pub fn settlement_service(&self) -> &SettlementService {
        &self.inner.settlement_service
    }

    pub fn wallet(&self) -> Option<&WalletClient> {
        self.inner.wallet.as_ref()
    }

    pub fn event_producer(&self) -> Option<&EventProducer> {
        self.inner.event_producer.as_ref()
    }

    /// The shared `BetRepository`, so additional transports can build their own
    /// service instances without opening a second connection pool.
    pub fn bet_repository(&self) -> BetRepository {
        BetRepository::new(self.inner.db_pool.clone())
    }
}
