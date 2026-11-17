package config

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"
)

// Env is the deployment environment (APP_ENV). "development" relaxes the
// startup secret checks; any other value must pass Validate.
type Config struct {
	Env             string
	HTTPPort        string
	DatabaseURL     string
	RedisAddr       string
	RedisPassword   string
	RedisDB         int
	JWTSecretKey    string
	ClickHouseDSN   string
	VaultAddr       string
	VaultToken      string
	VaultTransitKey string
	RedpandaEnabled bool
	RedpandaBrokers []string
	// CORSOrigins is the browser origin allowlist, comma-separated.
	CORSOrigins string
	// CookieSecure marks the admin session cookie Secure; must stay true in
	// production (plain HTTP would leak the session on any downgrade).
	CookieSecure bool

	AuthService    ServiceConfig
	UserService    ServiceConfig
	PaymentService ServiceConfig
	BettingEngine  ServiceConfig
}

type ServiceConfig struct {
	Address        string
	Timeout        time.Duration
	MaxRecvMsgSize int
	MaxSendMsgSize int
}

// isDevelopmentEnv reports whether the environment allows local defaults.
func isDevelopmentEnv(env string) bool {
	switch strings.ToLower(strings.TrimSpace(env)) {
	case "development", "dev", "local", "test":
		return true
	default:
		return false
	}
}

func Load() *Config {
	redpandaBrokers := splitAndTrim(getEnv("REDPANDA_BROKERS", ""))
	return &Config{
		Env:             getEnv("APP_ENV", "development"),
		HTTPPort:        getEnv("HTTP_PORT", "8080"),
		DatabaseURL:     getEnv("DATABASE_URL", "postgres://postgres:postgres@localhost:5432/opus?sslmode=disable"),
		RedisAddr:       getEnv("REDIS_ADDR", "localhost:6379"),
		RedisPassword:   getEnv("REDIS_PASSWORD", ""),
		RedisDB:         0,
		JWTSecretKey:    getEnv("JWT_SECRET_KEY", "dev-secret-change-me"),
		ClickHouseDSN:   getEnv("CLICKHOUSE_DSN", ""),
		VaultAddr:       getEnv("VAULT_ADDR", ""),
		VaultToken:      getEnv("VAULT_TOKEN", ""),
		VaultTransitKey: getEnv("VAULT_TRANSIT_KEY", ""),
		RedpandaEnabled: getEnv("REDPANDA_ENABLED", "false") == "true",
		RedpandaBrokers: redpandaBrokers,
		CORSOrigins:     getEnv("CORS_ORIGINS", ""),
		CookieSecure:    getEnv("COOKIE_SECURE", "true") == "true",

		AuthService: ServiceConfig{
			Address: getEnv("AUTH_SERVICE_ADDR", "localhost:50051"),
			Timeout: parseDuration(getEnv("AUTH_SERVICE_TIMEOUT", "2s")),
		},
		UserService: ServiceConfig{
			Address: getEnv("USER_SERVICE_ADDR", "localhost:50052"),
			Timeout: parseDuration(getEnv("USER_SERVICE_TIMEOUT", "3s")),
		},
		PaymentService: ServiceConfig{
			Address: getEnv("PAYMENT_SERVICE_ADDR", "localhost:50055"),
			Timeout: parseDuration(getEnv("PAYMENT_SERVICE_TIMEOUT", "10s")),
		},
		BettingEngine: ServiceConfig{
			Address: getEnv("BETTING_ENGINE_ADDR", "localhost:50054"),
			Timeout: parseDuration(getEnv("BETTING_ENGINE_TIMEOUT", "10s")),
		},
	}
}

// defaultJWTSecret is the placeholder that shipped in the deployment manifests.
// A service that starts with it accepts tokens anyone can mint, so outside
// development it is treated as "not configured" rather than as a valid secret.
const defaultJWTSecret = "dev-secret-change-me"

// Validate reports configuration the process must not run with. It mirrors
// services/go/user: outside APP_ENV=development a usable JWT verification key
// is mandatory, because the admin BFF is the only gate in front of every
// privileged operation (balance adjustments, withdrawals, player data).
func (c *Config) Validate() error {
	if c == nil {
		return fmt.Errorf("config is nil")
	}
	// Treat anything that is not explicitly a development/local environment as
	// production: the previous check only skipped validation for the exact string
	// "development", so APP_ENV=dev or "" ran with the production rules anyway —
	// while an operator setting APP_ENV=DEV expected the relaxed ones.
	if isDevelopmentEnv(c.Env) {
		return nil
	}

	misconfigured := []string{}
	for name, value := range map[string]string{
		"DATABASE_URL": c.DatabaseURL,
		"REDIS_ADDR":   c.RedisAddr,
	} {
		if strings.TrimSpace(value) == "" {
			misconfigured = append(misconfigured, name+" is empty")
		}
	}

	hmacUsable := c.JWTSecretKey != "" && c.JWTSecretKey != defaultJWTSecret
	// EdDSA is the preferred mode. It is usable when either the base64 key pair
	// (what auth and the deployment templates use) or the PEM file form is
	// configured; the middleware's loader accepts both.
	eddsaUsable := (os.Getenv("JWT_ED25519_PUBLIC_KEY") != "" &&
		os.Getenv("JWT_ED25519_PRIVATE_KEY") != "") ||
		os.Getenv("JWT_ED25519_PUBLIC_KEY_FILE") != ""
	if !hmacUsable && !eddsaUsable {
		if c.JWTSecretKey == defaultJWTSecret {
			misconfigured = append(misconfigured,
				"JWT_SECRET_KEY is still the default placeholder")
		}
		misconfigured = append(misconfigured,
			"configure JWT_ED25519_PRIVATE_KEY + JWT_ED25519_PUBLIC_KEY "+
				"(base64) or JWT_ED25519_PUBLIC_KEY_FILE; a non-default "+
				"JWT_SECRET_KEY alone leaves tokens unverifiable across services")
	}

	for _, svc := range []struct {
		name string
		addr string
	}{
		{"AUTH_SERVICE_ADDR", c.AuthService.Address},
		{"USER_SERVICE_ADDR", c.UserService.Address},
		{"PAYMENT_SERVICE_ADDR", c.PaymentService.Address},
		{"BETTING_ENGINE_ADDR", c.BettingEngine.Address},
	} {
		if strings.HasPrefix(svc.addr, "localhost") || strings.HasPrefix(svc.addr, "127.0.0.1") {
			misconfigured = append(misconfigured,
				fmt.Sprintf("%s resolves to the loopback interface (%s)", svc.name, svc.addr))
		}
	}

	if !c.CookieSecure {
		misconfigured = append(misconfigured,
			"COOKIE_SECURE must be true outside development")
	}
	if port, err := strconv.Atoi(strings.TrimSpace(c.HTTPPort)); err != nil || port < 1 || port > 65535 {
		misconfigured = append(misconfigured,
			fmt.Sprintf("HTTP_PORT must be a port number, got %q", c.HTTPPort))
	}
	if strings.TrimSpace(c.CORSOrigins) == "" {
		misconfigured = append(misconfigured, "CORS_ORIGINS is empty")
	} else if strings.Contains(c.CORSOrigins, "*") {
		// cors.New(AllowOrigins: "*", AllowCredentials: true) makes Fiber reflect
		// the caller's Origin, which for a cookie-authenticated admin API is a
		// straight CSRF hole.
		misconfigured = append(misconfigured,
			`CORS_ORIGINS must list explicit origins, not "*"`)
	}

	if len(misconfigured) > 0 {
		return fmt.Errorf("invalid admin-bff configuration: %s", strings.Join(misconfigured, "; "))
	}
	return nil
}

func getEnv(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

func splitAndTrim(raw string) []string {
	if raw == "" {
		return nil
	}
	parts := strings.Split(raw, ",")
	out := make([]string, 0, len(parts))
	for _, part := range parts {
		if value := strings.TrimSpace(part); value != "" {
			out = append(out, value)
		}
	}
	return out
}

func parseDuration(s string) time.Duration {
	d, err := time.ParseDuration(s)
	if err != nil {
		return 5 * time.Second
	}
	return d
}
