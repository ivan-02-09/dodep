package service

import "testing"

func TestNewCasinoServiceNormalisesCurrency(t *testing.T) {
	cases := map[string]string{
		"eur":     "EUR",
		"":        "USD",
		"  gbp  ": "GBP",
	}
	for input, want := range cases {
		svc := NewCasinoService(nil, nil, nil, nil, nil, input)
		if svc.currency != want {
			t.Fatalf("NewCasinoService(%q).currency = %q, want %q", input, svc.currency, want)
		}
	}
}
