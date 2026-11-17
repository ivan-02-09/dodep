package config

import (
	"reflect"
	"strings"
	"testing"
)

func TestSplitAndTrim(t *testing.T) {
	got := splitAndTrim(" broker1:9092, ,broker2:9092 , broker3:9092 ")
	want := []string{"broker1:9092", "broker2:9092", "broker3:9092"}

	if !reflect.DeepEqual(got, want) {
		t.Fatalf("unexpected brokers: got %v want %v", got, want)
	}
}

func TestLoadRedpandaConfig(t *testing.T) {
	t.Setenv("REDPANDA_ENABLED", "true")
	t.Setenv("REDPANDA_BROKERS", "broker1:9092, broker2:9092")

	cfg := Load()
	if !cfg.RedpandaEnabled {
		t.Fatal("expected RedpandaEnabled to be true")
	}
	want := []string{"broker1:9092", "broker2:9092"}
	if !reflect.DeepEqual(cfg.RedpandaBrokers, want) {
		t.Fatalf("unexpected brokers: got %v want %v", cfg.RedpandaBrokers, want)
	}
}

func TestValidateRejectsPlaceholderSecretOutsideDevelopment(t *testing.T) {
	t.Setenv("APP_ENV", "production")
	t.Setenv("JWT_SECRET_KEY", "")
	t.Setenv("JWT_ED25519_PUBLIC_KEY", "")
	t.Setenv("JWT_ED25519_PRIVATE_KEY", "")
	t.Setenv("JWT_ED25519_PUBLIC_KEY_FILE", "")

	cfg := Load()
	cfg.AuthService.Address = "auth:50051"
	cfg.UserService.Address = "user:50052"
	cfg.PaymentService.Address = "payment:50055"
	cfg.BettingEngine.Address = "betting-engine:50054"

	err := cfg.Validate()
	if err == nil {
		t.Fatal("expected an error when no verification key is configured")
	}
	if !strings.Contains(err.Error(), "JWT_SECRET_KEY") {
		t.Fatalf("error should name the placeholder secret, got %v", err)
	}
}

func TestValidateRejectsLoopbackServiceAddresses(t *testing.T) {
	t.Setenv("APP_ENV", "production")
	t.Setenv("JWT_ED25519_PUBLIC_KEY", "cHVibGlj")
	t.Setenv("JWT_ED25519_PRIVATE_KEY", "cHJpdmF0ZQ==")

	cfg := Load()
	// Defaults point at localhost; in production that means the BFF is wired to
	// nothing and every admin page silently fails.
	if err := cfg.Validate(); err == nil {
		t.Fatal("expected loopback addresses to be rejected")
	}
}

func TestValidateSkipsChecksInDevelopment(t *testing.T) {
	t.Setenv("APP_ENV", "development")
	if err := Load().Validate(); err != nil {
		t.Fatalf("development mode must not fail validation: %v", err)
	}
}
