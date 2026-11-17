package middleware

import (
	"testing"

	"github.com/gofiber/fiber/v2"
	"github.com/valyala/fasthttp"
)

// newTestCtx builds a Fiber context carrying the locals that AdminAuth sets.
func newTestCtx(role string, permissions any) *fiber.Ctx {
	app := fiber.New()
	var captured *fiber.Ctx
	app.Get("/probe", func(c *fiber.Ctx) error {
		c.Locals("admin_role", role)
		c.Locals("permissions", permissions)
		captured = c
		return nil
	})

	req := &fasthttp.RequestCtx{}
	req.Request.Header.SetMethod("GET")
	req.Request.SetRequestURI("/probe")
	_ = app.Handler()(req)

	return captured
}

func TestRequirePermissionGrantsRolePermissions(t *testing.T) {
	// A finance admin may approve withdrawals even when the JWT carries no
	// explicit permissions array (the role is authoritative).
	c := newTestCtx("finance", nil)
	if !hasPermission(c, "withdrawals.approve") {
		t.Fatal("finance role should be allowed to approve withdrawals")
	}
}

func TestRequirePermissionBlocksMoneyMovementForSupport(t *testing.T) {
	// This is the bug the dead middleware allowed: everyone authenticated could
	// approve payouts.
	c := newTestCtx("support", nil)
	if hasPermission(c, "withdrawals.approve") {
		t.Fatal("support must not approve withdrawals")
	}
	if hasPermission(c, "players.adjust_balance") {
		t.Fatal("support must not adjust balances")
	}
	if !hasPermission(c, "players.read") {
		t.Fatal("support should still read players")
	}
}

func TestRequirePermissionUnderstandsJWTClaimShapes(t *testing.T) {
	// jwt v5 decodes the claim into []interface{}; a hand-built token can also
	// carry []string or a comma-separated string.
	shapes := map[string]any{
		"[]interface{}": []interface{}{"casino.write"},
		"[]string":      []string{"casino.write"},
		"string":        "casino.write",
	}
	for name, permissions := range shapes {
		c := newTestCtx("viewer", permissions)
		if !hasPermission(c, "casino.write") {
			t.Fatalf("%s: explicit permission should be honoured", name)
		}
	}
}

func TestSuperAdminWildcard(t *testing.T) {
	c := newTestCtx("super_admin", nil)
	if !hasPermission(c, "anything.at.all") {
		t.Fatal("super_admin must hold every permission")
	}
}

func TestRoleCaseIsNormalised(t *testing.T) {
	// Tokens minted for the UI carry SUPER_ADMIN, seeded rows carry super_admin.
	for _, role := range []string{"super_admin", "SUPER_ADMIN", " Super_Admin "} {
		c := newTestCtx(role, nil)
		if !hasPermission(c, "withdrawals.approve") {
			t.Fatalf("role %q should be treated as super_admin", role)
		}
	}
}

func TestUnknownRoleHasNoPermissions(t *testing.T) {
	c := newTestCtx("intern", nil)
	if hasPermission(c, "players.read") {
		t.Fatal("an unknown role must not inherit permissions")
	}
}

func TestEveryRoleIsDeclared(t *testing.T) {
	// Roles referenced by the seed data and admin UI must exist here, otherwise
	// the admin silently loses access after this change.
	for _, role := range []string{"super_admin", "admin", "support", "finance", "risk", "viewer"} {
		if _, ok := RolePermissions[role]; !ok {
			t.Fatalf("role %q has no permission mapping", role)
		}
	}
}
