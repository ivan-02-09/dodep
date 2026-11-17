package middleware

import (
	"crypto/ed25519"
	"crypto/x509"
	"encoding/base64"
	"encoding/pem"
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/gofiber/fiber/v2"
	"github.com/golang-jwt/jwt/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

// loadEd25519PublicKey resolves the token verification key from:
//
//  1. JWT_ED25519_PUBLIC_KEY_FILE — a PEM (or raw DER) file, and
//  2. JWT_ED25519_PUBLIC_KEY — the base64 form the auth service issues and
//     .env.production.example documents.
//
// Only the file form used to be read, so a deployment that configured the
// base64 variable (the documented contract) silently fell back to HMAC and
// rejected every EdDSA token minted by the admin service.
//
// Returns nil when nothing usable is configured; the caller then falls back to
// HMAC.
func loadEd25519PublicKey(path string) ed25519.PublicKey {
	if path != "" {
		if raw, err := os.ReadFile(path); err == nil {
			if key := parseEd25519PublicKeyPEM(raw); key != nil {
				return key
			}
		}
	}
	return parseEd25519PublicKeyBase64(os.Getenv("JWT_ED25519_PUBLIC_KEY"))
}

func parseEd25519PublicKeyPEM(raw []byte) ed25519.PublicKey {
	block, _ := pem.Decode(raw)
	if block != nil {
		raw = block.Bytes
	}
	return parseEd25519PublicKeyDER(raw)
}

func parseEd25519PublicKeyBase64(encoded string) ed25519.PublicKey {
	encoded = strings.TrimSpace(encoded)
	if encoded == "" {
		return nil
	}
	decoded, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil {
		// Tolerate the URL-safe / unpadded form too, so an operator copy-paste
		// cannot silently disable token verification.
		decoded, err = base64.RawURLEncoding.DecodeString(strings.TrimRight(encoded, "="))
		if err != nil {
			return nil
		}
	}
	return parseEd25519PublicKeyDER(decoded)
}

// parseEd25519PublicKeyDER accepts either the raw 32-byte key (what the auth
// service emits after base64) or a DER-encoded SubjectPublicKeyInfo (what
// `openssl pkey -pubout -outform DER` produces).
func parseEd25519PublicKeyDER(raw []byte) ed25519.PublicKey {
	if len(raw) == ed25519.PublicKeySize {
		return ed25519.PublicKey(raw)
	}
	parsed, err := x509.ParsePKIXPublicKey(raw)
	if err != nil {
		return nil
	}
	pub, ok := parsed.(ed25519.PublicKey)
	if !ok {
		return nil
	}
	return pub
}

func AdminAuth(jwtSecret string, db *pgxpool.Pool) fiber.Handler {
	// Load Ed25519 public key once at startup (optional).
	ed25519Pub := loadEd25519PublicKey(os.Getenv("JWT_ED25519_PUBLIC_KEY_FILE"))

	return func(c *fiber.Ctx) error {
		authHeader := c.Get("Authorization")
		tokenStr := ""
		if authHeader != "" {
			tokenStr = strings.TrimPrefix(authHeader, "Bearer ")
			if tokenStr == authHeader {
				return c.Status(401).JSON(fiber.Map{"error": "invalid authorization format"})
			}
		} else {
			// Fallback for WebSocket / SSE clients that cannot set custom headers.
			tokenStr = c.Query("token", "")
			if tokenStr == "" {
				return c.Status(401).JSON(fiber.Map{"error": "missing authorization header"})
			}
		}

		token, err := jwt.Parse(tokenStr, func(t *jwt.Token) (interface{}, error) {
			switch t.Method.(type) {
			case *jwt.SigningMethodEd25519:
				if ed25519Pub == nil {
					return nil, errors.New("ed25519 public key not configured")
				}
				return ed25519Pub, nil
			case *jwt.SigningMethodHMAC:
				return []byte(jwtSecret), nil
			default:
				return nil, fmt.Errorf("unexpected signing method: %v", t.Header["alg"])
			}
		})
		if err != nil || !token.Valid {
			return c.Status(401).JSON(fiber.Map{"error": "invalid token"})
		}

		claims, ok := token.Claims.(jwt.MapClaims)
		if !ok {
			return c.Status(401).JSON(fiber.Map{"error": "invalid claims"})
		}

		c.Locals("admin_id", claims["admin_id"])
		c.Locals("admin_role", claims["role"])
		// Handlers record who approved/awarded what; without this locals entry
		// every `approved_by_name` was written as NULL.
		if email, ok := claims["email"].(string); ok {
			c.Locals("admin_name", email)
		}
		c.Locals("permissions", claims["permissions"])

		return c.Next()
	}
}

// RolePermissions is the permission set implied by a role.
//
// `RequirePermission` used to be dead code: no route ever applied it, so every
// authenticated admin — including a `viewer` — could approve withdrawals and
// adjust balances. Roles are the coarse contract used by the existing seed data;
// explicit `admin_users.permissions` entries extend the role, they never
// subtract from it (an operator cannot lock themselves out by accident).
var RolePermissions = map[string][]string{
	"super_admin": {"*"},
	"admin": {
		"players.read", "players.write", "players.adjust_balance",
		"withdrawals.read", "withdrawals.approve", "withdrawals.decline",
		"deposits.read", "deposits.write",
		"finance.read", "finance.write",
		"kyc.read", "kyc.write",
		"bonuses.read", "bonuses.write",
		"casino.read", "casino.write",
		"sports.read", "sports.write",
		"risk.read", "risk.write",
		"crm.read", "crm.write",
		"reports.read", "settings.read", "settings.write",
		"affiliates.read", "affiliates.write",
		"audit.read", "admin.manage",
	},
	// Support can look at players and tickets but must not move money.
	"support": {
		"players.read", "withdrawals.read", "deposits.read",
		"kyc.read", "bonuses.read", "crm.read", "crm.write",
		"support.read", "support.write", "reports.read",
	},
	"finance": {
		"players.read", "withdrawals.read", "withdrawals.approve",
		"withdrawals.decline", "deposits.read", "deposits.write",
		"finance.read", "finance.write", "reports.read", "audit.read",
	},
	"risk": {
		"players.read", "withdrawals.read", "risk.read", "risk.write",
		"reports.read", "audit.read",
	},
	"viewer": {
		"players.read", "withdrawals.read", "deposits.read", "finance.read",
		"bonuses.read", "casino.read", "sports.read", "risk.read", "crm.read",
		"reports.read", "audit.read",
	},
}

// RoleFromLocal resolves the admin role placed in the request context by
// `AdminAuth`. It is exported so handlers can apply role-level rules.
func RoleFromLocal(c *fiber.Ctx) string {
	role, _ := c.Locals("admin_role").(string)
	return role
}

// RequirePermission rejects the request unless the admin holds `perm`.
//
// The JWT stores `permissions` as a JSON array, which the jwt v5 decoder yields
// as `[]interface{}`. The previous implementation only understood that shape and
// therefore denied every request whenever the claim was absent, while a token
// carrying a single string slipped through untyped.
func RequirePermission(perm string) fiber.Handler {
	return func(c *fiber.Ctx) error {
		if hasPermission(c, perm) {
			return c.Next()
		}
		return c.Status(403).JSON(fiber.Map{"error": "permission denied", "required": perm})
	}
}

// RequireAnyPermission passes when the admin holds at least one of `perms`.
func RequireAnyPermission(perms ...string) fiber.Handler {
	return func(c *fiber.Ctx) error {
		for _, perm := range perms {
			if hasPermission(c, perm) {
				return c.Next()
			}
		}
		return c.Status(403).JSON(fiber.Map{"error": "permission denied", "required_any": perms})
	}
}

func hasPermission(c *fiber.Ctx, perm string) bool {
	role, _ := c.Locals("admin_role").(string)
	// The database stores snake_case (`super_admin`, see cmd/seed), while the
	// admin UI and some tokens use UPPER_SNAKE. Normalise so both forms resolve.
	role = strings.ToLower(strings.TrimSpace(role))
	granted := map[string]struct{}{}

	for _, p := range RolePermissions[role] {
		if p == "*" {
			return true
		}
		granted[p] = struct{}{}
	}

	for _, p := range permissionStrings(c.Locals("permissions")) {
		if p == "*" {
			return true
		}
		granted[p] = struct{}{}
	}

	_, ok := granted[perm]
	return ok
}

// permissionStrings normalises the several shapes a claims array can take.
func permissionStrings(raw any) []string {
	switch v := raw.(type) {
	case []interface{}:
		out := make([]string, 0, len(v))
		for _, item := range v {
			if s, ok := item.(string); ok {
				out = append(out, s)
			}
		}
		return out
	case []string:
		return v
	case string:
		if v == "" {
			return nil
		}
		return strings.Split(v, ",")
	default:
		return nil
	}
}
