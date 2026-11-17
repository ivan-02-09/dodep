# dev-payment.ps1 — Запуск Payment Service в режиме разработки (Windows PowerShell)
# Использование: .\tools\dev-payment.ps1
# Или из корня проекта: pwsh -File tools\dev-payment.ps1

$env:PAYMENT_ENVIRONMENT                    = "development"
$env:PAYMENT_NOWPAYMENTS_BASE_URL           = "https://api.nowpayments.io/v1"
# Секреты больше не хранятся в скрипте: задайте их в своей оболочке или в
# локальном .env (файл в .gitignore). Скрипт не подставляет значения по
# умолчанию специально, чтобы забытый экспорт был виден сразу.
#   $env:PAYMENT_NOWPAYMENTS_API_KEY    = "..."
#   $env:PAYMENT_NOWPAYMENTS_IPN_SECRET = "..."
$env:PAYMENT_NOWPAYMENTS_IPN_CALLBACK_URL   = "http://localhost:8084/api/v1/payments/webhooks/nowpayments"

$env:PAYMENT_DATABASE_HOST                  = "localhost"
$env:PAYMENT_DATABASE_PORT                  = "5433"
$env:PAYMENT_DATABASE_USER                  = "postgres"
$env:PAYMENT_DATABASE_PASSWORD              = "changeme"
$env:PAYMENT_DATABASE_DATABASE              = "opus_casino"
$env:PAYMENT_DATABASE_SSL_MODE              = "disable"

$env:PAYMENT_REDIS_HOST                     = "localhost"
$env:PAYMENT_REDIS_PORT                     = "6379"
$env:PAYMENT_REDIS_PASSWORD                 = "changeme"

$env:PAYMENT_KAFKA_BROKERS                  = "localhost:9092"

$env:PAYMENT_TRACING_ENABLED                = "false"

$env:PAYMENT_WALLET_ADDRESS                 = "localhost:50053"
$env:PAYMENT_USER_ADDRESS                   = "localhost:50052"

# Dev JWT key (сгенерирован командой: go run tools/gen-ed25519.go)
# В production замени на реальный ключ из .env.production
$env:PAYMENT_AUTH_ED25519_PUBLIC_KEY        = "bq6FYZBelg7DJkYJdhv5eOkZaqQZ+0FPYNmfTOIev2I="

Write-Host "Starting Payment Service (port 8084)..." -ForegroundColor Cyan
Set-Location "$PSScriptRoot\..\services\go\payment"
go run ./cmd/server
