# Аудит проекта `dodep` (Opus Casino)

**Дата:** 2026-11-17
**Ветка:** `arena/bcac23cd-dodep` (от `096b1ee`)
**Объём:** 9 581 файл под git; Go ≈ 97k строк, Rust ≈ 8.8k, Python ≈ 7.5k, TS/TSX ≈ 25k, Dart ≈ 6.9k

---

## 0. Что реально запускалось при проверке

| Проверка | Результат |
|---|---|
| `npx tsc --noEmit` (apps/admin) | ✅ чисто |
| `npx vitest run` (apps/admin) | ✅ 96 тестов, 8 файлов |
| `npx tsc --noEmit` (apps/web) | ✅ чисто |
| `npx jest` (apps/web) | ✅ 6 тестов, 2 файла |
| `python tools/guards/architecture_guards.py` | ✅ G1–G4 пройдены |
| `python -m compileall services/python` | ✅ чисто |
| `npm audit` (web / admin) | ⚠️ 11 (1 critical) / 30 (2 critical) уязвимостей |
| Go / Cargo / Flutter сборка | ⛔ не проверялась — в песочнице нет `go`, `cargo`, `flutter`; выводы по этим стекам сделаны чтением кода и конфигов |

Вывод: **фронтенды в хорошем состоянии, инфраструктура сборки/деплоя и «денежное ядро» — нет.**

---

## 1. Блокеры (P0): продукт в таком виде не собирается и не поднимается

### 1.1 Все Go-образы собираются из одного сервиса
`infra/docker/Dockerfile.go:40` — `ARG SERVICE_PATH=services/go/auth`.
`docker-compose.prod.yml` передаёт аргумент **`SERVICE:`** (строки 246, 275, 339, 377, 410, 457, 509, 542, 577, 607), а не `SERVICE_PATH`. Аргумент `SERVICE` в Dockerfile не объявлен вообще.
Итог: `user`, `payment`, `casino`, `kyc`, `admin-bff` и т. д. запускаются с бинарником **auth**, слушающим порт 8083.

### 1.2 Пер-сервисные Dockerfile нерабочие
- `services/go/{auth,user,casino,notification}/Dockerfile` → `FROM golang:1.21-alpine`, а `go.mod` требует `go 1.25.0` / `1.26.0` → сборка падает сразу.
- `services/go/payment/Dockerfile` **отсутствует**, но `cd-production.yml:134` собирает именно `services/go/<service>/Dockerfile`, а дефолтная матрица включает `payment` → CD-пайплайн красный.
- `admin-bff`, `rg`, `affiliate`, `kyc` — Dockerfile нет вовсе.

### 1.3 Dockerfile админки не может собраться
`infra/docker/Dockerfile.admin` при `context: ../../apps/admin`:
- `COPY libs/shared/typescript ./libs/shared/typescript` — путь **вне** контекста сборки → ошибка;
- в `apps/admin/package.json` зависимость `"@dod/shared": "file:../../libs/shared/typescript"` — тоже вне контекста;
- `npm ci --omit=dev`, затем `npm run build` = `tsc && vite build`, но `typescript`/`vite` — `devDependencies` → `tsc: not found`.

### 1.4 Healthcheck'и не могут пройти
- Runtime-стадия `Dockerfile.go` — `gcr.io/distroless/static-debian12` (строка 98). В нём **нет `wget`**, а все Go-healthcheck'и в compose вызывают `wget --spider` (12 сервисов) → контейнеры вечно `unhealthy`.
- `apps/web` healthcheck: `http://localhost:3000/api/health`, но route handler `apps/web/src/app/api/**` отсутствует → 404 → `unhealthy`.
- `apps/admin` healthcheck бьёт в `:80/` (корень SPA) — работает, но игнорирует собственный `/health`, который отдаёт `apps/admin/nginx.conf:17`.

### 1.5 Переменные окружения не совпадают с кодом
| Compose передаёт | Код читает |
|---|---|
| `REDIS_URL` (10 сервисов) | `REDIS_ADDR` (auth, user, bonus, notification, admin-bff, affiliate), `REDIS_HOST/PORT` (casino) |
| `PORT: 8091` (admin-bff) | `HTTP_PORT` (`internal/config/config.go:39`) → сервис слушает 8080, nginx проксирует на 8091 → 502 |
| `AUTH_GRPC_ADDR`, `USER_GRPC_ADDR`, `PAYMENT_GRPC_ADDR`, `BETTING_ENGINE…` | `AUTH_SERVICE_ADDR`, `USER_SERVICE_ADDR`, `PAYMENT_SERVICE_ADDR`, `BETTING_ENGINE_ADDR` (`admin-bff/internal/config/config.go:53–65`) |

Также `infra/k8s/configmap.yaml` объявляет `REDIS_HOST/PORT`, `GO_ENV`, `BETTING_ENGINE_GRPC`, `WALLET_CORE_GRPC`, `AUTH_GRPC` — ни одно из этих имён код не читает (`GO_ENV` — 0 вхождений в Go).

### 1.6 Прод-база не инициализируется
`001_extensions_and_enums.sql:10-13` требует `citus`, `pg_partman`, `pg_stat_statements`. В compose образ — `postgres:16-alpine` (строки 33 и 725) — этих расширений нет (Citus живёт только в k8s-манифесте `citusdata/citus:12.1`). `postgres`-entrypoint падает на первом же скрипте.
Плюс `libs/migrations/postgresql` монтируется целиком в `/docker-entrypoint-initdb.d` (compose:42) — вместе с `014_payments_core.down.sql`, который entrypoint выполнит **до** `014_payments_core.sql` (сортировка по имени). `tools/migrate.sh` это специально фильтрует, а compose — нет.

### 1.7 Веб-клиент ломается на относительном base URL
Прод собирается с `NEXT_PUBLIC_API_URL=/api` (compose:641, `Dockerfile.web:15`), а `apps/web/src/lib/api/client.ts:96` делает `new URL(\`${baseUrl}${path}\`)`. С относительным base это `TypeError: Invalid URL` (проверено в node) — т. е. **любой** запрос из веба падает ещё до сети.
Дополнительно в `lib/api-client.ts:7` baseURL `/api`, а пути — `/api/v1/...` → двойной префикс `/api/api/v1/...`.

---

## 2. Критично для денег и безопасности (P1)

### 2.1 Ставки и выплаты не проходят через кошелёк
`services/rust/betting-engine` не имеет зависимости на `wallet-core` (нет ни в `Cargo.toml`, ни в `src/`): `place_bet` не проверяет и не списывает баланс (`services/bet_service.rs:36-39` — только валидация min/max, `WALLET_INSUFFICIENT_BALANCE` есть лишь как текст ошибки), `settle_bet` просто пишет `actual_win` в таблицу `bets` (`settlement_service.rs:44-71`), никакого credit в ledger нет.
Итог: **играть можно бесконечно без денег, а выигрыш никогда не зачисляется.** Это ядро продукта.

### 2.2 wallet-core игнорирует мультивалютность
`wallet-core/src/api/grpc.rs:98-101, 125, 163, 208, 327` — `"USD"` захардкожен для балансов, транзакций и `get_or_create_wallet`. При этом в БД валюта есть, а веб отдаёт `RUB` (`components/pages/wallet.tsx`).

### 2.3 Админ-панель: RBAC только на фронте
`middleware.RequirePermission` объявлен (`internal/middleware/admin_auth.go:83`), но **не используется ни в одном роуте** (0 вхождений). Все 225 админ-эндпоинтов доступны любому валидному админ-токену, включая роль `VIEWER`. Фронт (`ProtectedRoute permission=...`) — единственная защита, обходится вызовом API напрямую.

### 2.4 Корректировка баланса без проверки TOTP
`handlers/players.go:173-196`: TOTP-код требуется лишь на наличие (`403` если пусто), но **не валидируется**, и вместо вызова wallet стоит `// TODO: call Wallet Service gRPC CreditDebit` + `{"success": true}`. Там же заглушки на `tags`, `group`, `block` (`TODO: call User Service gRPC BlockPlayer`), `merge` (`TODO: implement transactional merge`).
Итого 7 роутов в `players.go` отвечают успехом, ничего не меняя.

### 2.5 Одобрение/отклонение выводов
`handlers/withdrawals.go:55-110`:
- `db.Model(...).Updates(...)` без проверки `err` и `RowsAffected` → ответ `{"success": true}` даже при ошибке/отсутствии записи;
- нет аудита (в файле 0 вызовов `logAudit`), нет проверки текущего статуса → двойной approve, approve после decline;
- нет обращения к платёжному шлюзу/кошельку — только поле в БД.
Дополнительно фронт бьёт не туда: `apps/admin/src/services/finance.service.ts:46,50` вызывает `/admin/finance/withdrawals/:id/approve|reject` и `/admin/finance/users/:id/adjust-balance`, а на сервере это `/admin/withdrawals/:id/{approve,decline}` и `/admin/players/:id/adjust-balance` (`finance.go:17` монтирует `/finance` только с `deposits|withdrawals|summary`) → 404 на всех действиях финансиста.

### 2.6 admin-bff запускается без fail-fast и с дефолтным секретом
`internal/config/config.go:44` — `JWTSecretKey: getEnv("JWT_SECRET_KEY", "dev-secret-change-me")`, проверки `APP_ENV` нет (в `services/go/auth` она есть — `auth/internal/config/config.go:74-77`).
`middleware/admin_auth.go:44-56` принимает и `EdDSA`, и `HMAC`, откатываясь на HMAC, если PEM-файл не смонтирован. В проде это означает возможность подписи токенов общеизвестной строкой.
Там же: `admin_name` в токен не кладётся (`service/admin_auth_service.go:418-423`), поэтому `approved_by_name` в `withdrawals` всегда `NULL`.

### 2.7 Токены веб-клиента в `localStorage`
`apps/web/src/stores/auth-store.ts:189-199` — `persist` с `accessToken`, `refreshToken`, `isAuthenticated` в `localStorage`. Любой XSS = полный захват сессии. Админка сделана правильно (HttpOnly cookie + `withCredentials`), веб — нет.
Там же баг: `lib/websocket.ts:169-173` берёт токен из `document.cookie` (`auth_token=...`), но **никто этот cookie не выставляет** (0 присваиваний `document.cookie`) → WebSocket-подписки молча не работают.

### 2.8 WebSocket-gateway не проверяет токен
`services/rust/websocket-gateway/src/api/handlers/ws_handler.rs:74-81`: `extract_user_id` парсит строку `user_123`, иначе `0`. JWT не декодируется и не верифицируется. Зная формат, клиент получает доступ к `Topic::UserBets(id)` / `UserBalance(id)` чужого пользователя — проверка `is_topic_visible_to` сравнивает с уже подделанным `user_id`.

### 2.9 RLS в БД не работает (и не может работать)
`libs/migrations/postgresql/008_rls_and_triggers.sql:13-46` включает RLS с политиками на `current_setting('app.current_user_id')`, но `set_current_user()` (строка 104) **не вызывается ни из Go, ни из Rust** (0 вхождений). Переменная не выставляется → политика = `id = NULL` → при роли без `BYPASSRLS` все выборки пустые, а при суперпользователе (а compose ходит `opus_app` без явных грантов) RLS обходится. Роли/`BYPASSRLS` в миграциях не создаются — только комментарий «Run as superuser».

### 2.10 Секреты в репозитории
Реальные ключи NOWPayments (API key и IPN secret, одинаковые) закоммичены в трёх местах:
- `.env.production.example:71-72, 78-79`
- `services/go/payment/config/dev.yaml:29-30` (в комментарии)
- `tools/dev-payment.ps1:7-8`

Плюс `.githooks/pre-commit` ссылается на `.gitleaks.toml`, которого в репозитории нет.
Сид админа: `services/go/admin-bff/cmd/seed/main.go:24` — пароль по умолчанию `admin123`, роль `super_admin`, права `['*']`.

### 2.11 Ответственная игра не подключена
Сервис `rg` (лимиты, self-exclusion) не входит в `docker-compose.prod.yml` и не имеет upstream в nginx; `betting-engine` и `bonus` его не вызывают. Для гемблинг-продукта это регуляторный блокер, а не «фича на потом».

---

## 3. Незавершённый функционал, который выглядит завершённым

| Компонент | Фактическое состояние |
|---|---|
| `services/go/kyc` | `main.go` — 6 строк, `// TODO: Implement kyc service`; при этом nginx проксирует `/api/v1/kyc/` на него (port 8089) |
| `notification` | `repository/notification_repository.go`: `CreateNotification` → `nil` без записи, `GetNotification` → `nil, nil`; `service`: `sendEmail`/`sendSMS`/`sendPush` — TODO, только лог |
| `payment` gRPC | сервер стартует, но `RegisterPaymentServiceServer` закомментирован (`cmd/server/main.go:415`); при этом `admin-bff` ходит в payment **по gRPC** → все финансовые списки падают |
| `casino` gRPC | регистрация сервиса закомментирована (`main.go:123`) |
| `admin-bff` finance | `ApproveWithdrawal`/`RejectWithdrawal` → `error "not implemented: payment proto lacks ApproveWithdrawal RPC"` |
| `admin` CMS/аналитика | 22 из 164 эндпоинтов SPA не имеют серверной реализации: `/admin/content/*` (CMS-страницы и промо), `/admin/system/{config,health,maintenance,maintenance/schedule,settings,feature-flags}` (сервер отдаёт `/admin/system`, `/admin/audit-logs`, `/admin/users/admin`, `/admin/system/frequency-caps`), `/admin/finance/transactions`, `/admin/bonuses/grant`, `/admin/users/:id/{kyc-documents,support-chats,linked-accounts,communications}` |
| Несовпадение путей | SPA: `/admin/finance/withdrawals/:id/approve`, сервер: `/admin/withdrawals/:id/approve`; SPA: `/admin/finance/users/:id/adjust-balance`, сервер: `/admin/players/:id/adjust-balance` |
| Веб-страницы | `casino.tsx` (306 стр.), `sportsbook.tsx` (238), `bonuses.tsx`, `wallet.tsx` рисуют `mockGames` / `mockEvents` / `mockTransactions` с датами 2024 г. вместо API |

---

## 4. Документация расходится с кодом

- `README.md:15,21` — «500K DAU, p99 < 10 ms», `tools/k6/betting-engine.js:26-28` — порог `p99 < 100 ms`. Метрики в README ничем не подтверждены.
- `STAGES.md` и `docs/stages/*` помечают этапы «Завершён ✅ / 100%», включая KYC, notification, wallet — см. раздел 3.
- `README.md` описывает 60 skill-файлов и структуру `tools/ci`, `libs/migrations` — `tools/ci` не существует.

---

## 5. Инженерия: CI, зависимости, тесты

### 5.1 CI покрывает не всё
- `admin-bff` (11 тыс. строк, 225 роутов, самая нагруженная точка входа) **не упоминается ни в одном** workflow (0 вхождений в `.github/workflows/`), и для него нет Dockerfile.
- `ci-go.yml` матрица — `auth,user,payment,bonus,casino,notification,kyc`; `affiliate`, `rg`, `admin-bff` вынесены/пропущены.
- 22 workflow-файла, часть дублирует друг друга (`ci.yaml` прямо пишет, что он «catch-all» дубль).
- `continue-on-error: true` — 10 раз (в основном upload артефактов, но ослабляет сигнал).

### 5.2 Тесты
- Go: 474 функции, Rust: 50, Python: 174, admin: 96 ✅, web: **6** на 2 файла, mobile: **4** теста на 45 файлов.
- `wallet-core` — 1 unit-тест при всей критичности денежной логики; интеграционных тестов нет.
- `services/go/payment/internal/client/nowpayments_client_test.go:6` — `t.Skip("TODO: add tests")`.
- Нет ни одного coverage-гейта (только `-coverprofile` без порога).

### 5.3 Nx-обёртка не работает
`package.json` (`dev/build/test/lint`) вызывает `nx run-many --target=…`, но: `project.json` в репозитории — **0**, `workspaces` не объявлены, `node_modules` не установлены. `Makefile` (`dev`, `build`, `test`, `lint`, `format`) делегирует туда же → корневые команды из README не работают.

### 5.4 Зависимости
- `npm audit` (web): 11 (1 critical — `next@14.1.0`, cам npm пишет «This version has a security vulnerability»), high: `axios`, `ws`, `form-data`, `lodash`, `zod`, `nanoid`, `postcss`.
- `npm audit` (admin): 30 (2 critical — `tinypool` RCE через prototype pollution).
- 12 разных `go`-директив на 10 модулей: 1.21, 1.22, 1.23, 1.24.1, 1.25×5, 1.26×2 — общий тулчейн и `go.work` отсутствуют.
- `libs/shared/go` (1.21) не импортируется ни одним сервисом (0 вхождений).

### 5.5 Инфраструктура
- Terraform: `infra/terraform/main.tf` (GKE/`google`, `>= 1.7`) и `environments/production/main.tf` (AWS, `>= 1.6`) — разные провайдеры в одном каталоге; `dev`/`staging`/`production` используют **один** state bucket `opus-casino-terraform-state` и один lock-table.
- k8s: манифесты есть только для `auth`, `betting-engine`, `wallet-core` (+ monitoring); остальные 8 сервисов — только Helm-чарты, которые нигде не подключены как release.
- `docker-compose.prod.yml` — 24 сервиса, но `affiliate` и `rg` отсутствуют.

---

## 6. Гигиена репозитория

- **83% отслеживаемых файлов — копии каталога skills**: `7931` из `9581` файлов лежат в `.agents/`, `.claude/`, `.continue/`, `.factory/`, `.junie/`, `.kiro/`, `.openhands/`, `.qoder/`, `.qwen/`, `.trae/`, `.vibe/`, `.windsurf/` — по ~4.3 МБ каждая, включая `Archive.zip` и PNG ×12 копий. Любой `git clone` тянет ~50 МБ идентичных markdown-файлов.
- `gen/` — дубль `libs/proto/gen/go` (27 файлов, `diff -rq` → идентичны) плюс 381 неиспользуемый файл `gen/typescript` (0 импортов). Go-сгенерированный код при этом задвоен (5.3 МБ + 1.4 МБ).
- `services/go/admin-bff/cmd/` — `seed/check`, `seed/debug`, `seed/fix`, `seed/update`, `seed/verify` — 5 «одноразовых» утилит отладки в проде-репозитории.
- `apps/web/src/lib/api-client.ts` (187 строк) — мёртвый код, 0 импортов; рядом живой `lib/api/client.ts` (211 строк). Два клиента с разной логикой токенов.
- `services/go/notification/internal/service/notification_service.go:448+` и `repository` — заглушки, которые прошли ревью и CI.

---

## 7. Что сделано хорошо

1. **Админка**: 60 страниц, 16 сервисов, типизация без `as any`-свалки (25 на 60 файлов), 96 зелёных тестов, `tsc` без ошибок, HttpOnly-cookie для refresh, `withCredentials`, отдельный `ProtectedRoute` с реестром прав.
2. **Аутентификация (Go)**: Argon2id с параметрами OWASP, access/refresh разделены по `token_type`, детекция reuse refresh-токена, lockout по аккаунту и IP в Redis, TOTP, fail-fast по JWT-секрету в не-dev окружении, RS/EdDSA-ключи проверяются на соответствие друг другу.
3. **wallet-core (Rust)**: `Decimal` вместо float, транзакции + двойная запись в ledger, идемпотентность до бизнес-логики, transactional outbox — правильная архитектура (пусть и не связанная с остальными сервисами).
4. **Инфраструктурные «мелочи», которые часто забывают**: rate-limit с lockout по спецификации, IP-whitelist админки, security-headers в nginx/CSP, `runAsNonRoot` + distroless, Cosign-подпись образов, gitleaks-хук, architecture guards G1–G4 (реально проходят).
5. **Proto-контракты** и миграции ClickHouse/PostgreSQL оформлены аккуратно, деньги в БД — `NUMERIC(18,8)`, не float.

---

## 8. Порядок исправления (предложение)

1. **Сборка и запуск** (§1.1–1.7): `ARG SERVICE_PATH` + передача `SERVICE_PATH` из compose; один параметризованный Dockerfile; `HEALTHCHECK` без `wget` (использовать сам сервисный бинарь или `curl` в distroless-«debug»-варианте); выровнять имена env; `postgres` → `citusdata/citus`; монтировать миграции через `migrate.sh`, а не `initdb`.
2. **Веб-клиент** (§1.7): поддержать относительный base (не `new URL` на относительном пути), удалить `lib/api-client.ts`.
3. **Деньги** (§2.1–2.2): `betting-engine` → gRPC `wallet-core` (lock при place, credit при settle), убрать хардкод `"USD"`.
4. **Админка** (§2.3–2.6): включить `RequirePermission` на группу, валидировать TOTP, выровнять пути SPA↔BFF, добавить аудит и проверку `RowsAffected` в withdrawals, fail-fast по `APP_ENV`/`JWT_SECRET_KEY`.
5. **Сессии и WS** (§2.7–2.8): HttpOnly cookie для веба (как в админке), JWT-верификация в `websocket-gateway`.
6. **Секреты** (§2.10): ротировать NOWPayments-ключи, вычистить из трёх файлов, добавить `.gitleaks.toml`, запретить дефолтный пароль сида.
7. **Репо-гигиена** (§6): оставить один каталог skills (остальные — в `.gitignore`), удалить `gen/`, мёртвый код и `cmd/seed/*`.
8. **CI** (§5.1): добавить `admin-bff` (lint+test+image) и `affiliate`/`rg` в матрицу, включить coverage-гейты.
