package main

import (
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/gofiber/fiber/v2"

	"github.com/opus-casino/auth/internal/domain"
)

// Session cookies.
//
// The web client used to keep the access and refresh tokens in localStorage:
// any XSS on the casino origin could read both and keep the session alive
// indefinitely. Tokens are now also delivered as HttpOnly cookies scoped to the
// API path, so the browser attaches them automatically and JavaScript never sees
// them. The JSON response still carries the tokens for native clients.
const (
	AccessCookieName  = "access_token"
	RefreshCookieName = "refresh_token"

	// The access token must also reach the WebSocket handshake at /ws: it is
	// the only credential a browser can attach to it, because a WebSocket
	// upgrade cannot carry an Authorization header. Scoping it to /api made the
	// gateway see an anonymous handshake.
	accessCookiePath = "/"

	// The refresh token is only ever needed by POST /api/v1/auth/refresh, so it
	// stays scoped to the API prefix: a stolen refresh cookie from a request to
	// any other path is not possible in the first place.
	refreshCookiePath = "/api"
)

// cookieSecure reports whether cookies must carry the Secure flag.
//
// Enabled unless the deployment is explicitly a local one: an admin/casino
// session must never travel over plain HTTP.
func cookieSecure() bool {
	switch strings.ToLower(strings.TrimSpace(os.Getenv("APP_ENV"))) {
	case "development", "dev", "local", "test":
		// Still honour an explicit override, e.g. when testing over HTTP with a
		// production-like CORS setup.
		return envBool("COOKIE_SECURE", false)
	default:
		return envBool("COOKIE_SECURE", true)
	}
}

// cookieSameSite keeps the default Strict posture but allows an explicit
// override for a web app served from a different site than the API.
func cookieSameSite() string {
	switch strings.ToLower(strings.TrimSpace(os.Getenv("COOKIE_SAMESITE"))) {
	case "none":
		return fiber.CookieSameSiteNoneMode
	case "lax":
		return fiber.CookieSameSiteLaxMode
	case "strict":
		return fiber.CookieSameSiteStrictMode
	default:
		return fiber.CookieSameSiteStrictMode
	}
}

func envBool(key string, fallback bool) bool {
	raw := strings.TrimSpace(os.Getenv(key))
	if raw == "" {
		return fallback
	}
	value, err := strconv.ParseBool(raw)
	if err != nil {
		return fallback
	}
	return value
}

// setSessionCookies stores both tokens for the given lifetimes.
func setSessionCookies(c *fiber.Ctx, accessToken, refreshToken string, accessTTL, refreshTTL time.Duration) {
	secure := cookieSecure()
	sameSite := cookieSameSite()

	c.Cookie(&fiber.Cookie{
		Name:     AccessCookieName,
		Value:    accessToken,
		Path:     accessCookiePath,
		Expires:  time.Now().Add(accessTTL),
		MaxAge:   int(accessTTL.Seconds()),
		Secure:   secure,
		HTTPOnly: true,
		SameSite: sameSite,
	})
	c.Cookie(&fiber.Cookie{
		Name:     RefreshCookieName,
		Value:    refreshToken,
		Path:     refreshCookiePath,
		Expires:  time.Now().Add(refreshTTL),
		MaxAge:   int(refreshTTL.Seconds()),
		Secure:   secure,
		HTTPOnly: true,
		SameSite: sameSite,
	})
}

// clearSessionCookies expires both cookies (logout). Each cookie is expired on
// the path it was set with; a mismatch leaves the old cookie in the browser.
func clearSessionCookies(c *fiber.Ctx) {
	cookies := []struct{ name, path string }{
		{AccessCookieName, accessCookiePath},
		{RefreshCookieName, refreshCookiePath},
	}
	for _, cookie := range cookies {
		c.Cookie(&fiber.Cookie{
			Name:     cookie.name,
			Value:    "",
			Path:     cookie.path,
			Expires:  time.Now().Add(-time.Hour),
			MaxAge:   -1,
			Secure:   cookieSecure(),
			HTTPOnly: true,
			SameSite: cookieSameSite(),
		})
	}
}

// tokenTTLs returns the HTTP cookie lifetimes for a login result.
func tokenTTLs(tokens *domain.TokenPair) (access, refresh time.Duration) {
	access = time.Duration(tokens.ExpiresIn) * time.Second
	if access <= 0 {
		access = 15 * time.Minute
	}
	refresh = time.Duration(tokens.RefreshExpiresIn) * time.Second
	if refresh <= 0 {
		refresh = 7 * 24 * time.Hour
	}
	return access, refresh
}
