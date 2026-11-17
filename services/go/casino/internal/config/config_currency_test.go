package config

import "testing"

func TestDefaultCurrency(t *testing.T) {
	t.Setenv("DEFAULT_CURRENCY", "eur")
	if got := Load().DefaultCurrency; got != "EUR" {
		t.Fatalf("expected the configured currency to be normalised, got %q", got)
	}

	t.Setenv("DEFAULT_CURRENCY", "")
	if got := Load().DefaultCurrency; got != "USD" {
		t.Fatalf("expected the USD fallback, got %q", got)
	}

	// Whitespace-only values are a common compose mistake.
	t.Setenv("DEFAULT_CURRENCY", "   ")
	if got := Load().DefaultCurrency; got != "USD" {
		t.Fatalf("expected the USD fallback for a blank value, got %q", got)
	}
}
