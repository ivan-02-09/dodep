package handlers

import (
	"errors"
	"math"
	"strconv"
	"time"

	"github.com/gofiber/fiber/v2"
	"github.com/opus-casino/admin-bff/internal/middleware"
	"github.com/opus-casino/admin-bff/internal/models"
	"github.com/opus-casino/admin-bff/internal/service"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

// withdrawalDecidable lists the statuses an operator may still act on.
//
// Approve/decline/hold used to update the row unconditionally: re-approving an
// already rejected withdrawal (or racing two operators) silently overwrote the
// decision and left no trace. The update now carries the expected status and
// reports a 409 when nothing matched.
var withdrawalDecidable = []string{"pending", "review", "held"}

func RegisterWithdrawalRoutes(router fiber.Router, db *gorm.DB, log *zap.Logger, auditSvc *service.AuditService) {
	withdrawals := router.Group("/withdrawals", middleware.RequirePermission("withdrawals.read"))

	// List withdrawals (queue) — default sort by wait time desc
	withdrawals.Get("/", func(c *fiber.Ctx) error {
		status := c.Query("status", "")
		playerID := c.Query("player_id", "")
		page := c.QueryInt("page", 1)
		ps := c.QueryInt("page_size", 50)
		if page < 1 {
			page = 1
		}
		if ps < 1 || ps > 200 {
			ps = 50
		}

		q := db.Model(&models.Withdrawal{})
		if status != "" {
			q = q.Where("status = ?", status)
		}
		if playerID != "" {
			q = q.Where("player_id = ?", playerID)
		}

		var total int64
		if err := q.Count(&total).Error; err != nil {
			log.Error("withdrawal count failed", zap.Error(err))
			return c.Status(500).JSON(fiber.Map{"error": "database error"})
		}
		var items []models.Withdrawal
		if err := q.Order("created_at ASC").Limit(ps).Offset((page - 1) * ps).Find(&items).Error; err != nil {
			log.Error("withdrawal list failed", zap.Error(err))
			return c.Status(500).JSON(fiber.Map{"error": "database error"})
		}

		return c.JSON(fiber.Map{
			"data": items,
			"pagination": fiber.Map{
				"page": page, "page_size": ps,
				"total": total, "total_pages": int(math.Ceil(float64(total) / float64(ps))),
			},
		})
	})

	// Get withdrawal detail with checklist
	withdrawals.Get("/:id", func(c *fiber.Ctx) error {
		var w models.Withdrawal
		if err := db.Where("id = ?", c.Params("id")).First(&w).Error; err != nil {
			if errors.Is(err, gorm.ErrRecordNotFound) {
				return c.Status(404).JSON(fiber.Map{"error": "not found"})
			}
			return c.Status(500).JSON(fiber.Map{"error": "database error"})
		}
		return c.JSON(fiber.Map{"data": w})
	})

	// Approve withdrawal
	withdrawals.Post("/:id/approve",
		middleware.RequirePermission("withdrawals.approve"),
		func(c *fiber.Ctx) error {
			return decideWithdrawal(c, db, log, auditSvc, "approved")
		})

	// Decline withdrawal
	withdrawals.Post("/:id/decline",
		middleware.RequirePermission("withdrawals.decline"),
		func(c *fiber.Ctx) error {
			var req struct {
				Reason string `json:"reason"`
			}
			if err := c.BodyParser(&req); err != nil {
				return c.Status(400).JSON(fiber.Map{"error": "invalid body"})
			}
			if req.Reason == "" {
				return c.Status(400).JSON(fiber.Map{"error": "reason is required"})
			}
			return decideWithdrawal(c, db, log, auditSvc, "rejected", req.Reason)
		})

	// Hold for review
	withdrawals.Post("/:id/hold",
		middleware.RequirePermission("withdrawals.approve"),
		func(c *fiber.Ctx) error {
			var req struct {
				Reason string `json:"reason"`
			}
			if err := c.BodyParser(&req); err != nil {
				return c.Status(400).JSON(fiber.Map{"error": "invalid body"})
			}
			return decideWithdrawal(c, db, log, auditSvc, "held", req.Reason)
		})

	// Auto-approve endpoint (for internal trigger)
	withdrawals.Post("/:id/auto-approve", func(c *fiber.Ctx) error {
		var w models.Withdrawal
		if err := db.Where("id = ?", c.Params("id")).First(&w).Error; err != nil {
			if errors.Is(err, gorm.ErrRecordNotFound) {
				return c.Status(404).JSON(fiber.Map{"error": "not found"})
			}
			return c.Status(500).JSON(fiber.Map{"error": "database error"})
		}
		// Check auto-approve conditions
		if w.RiskScore < 20 && w.KycStatus == "verified" && w.WagerStatus == "completed" && w.AmlStatus == "clear" {
			now := time.Now()
			res := db.Model(&models.Withdrawal{}).
				Where("id = ? AND status IN ?", c.Params("id"), withdrawalDecidable).
				Updates(map[string]any{
					"status":           "approved",
					"approved_by":      "auto",
					"approved_by_name": "auto",
					"approved_at":      now,
					"updated_at":       now,
				})
			if res.Error != nil {
				log.Error("auto-approve failed", zap.Error(res.Error))
				return c.Status(500).JSON(fiber.Map{"error": "database error"})
			}
			if res.RowsAffected == 0 {
				return c.Status(409).JSON(fiber.Map{"error": "withdrawal already decided"})
			}
			logAudit(auditSvc, c, "withdrawal.auto_approve", "withdrawal", c.Params("id"), fiber.Map{
				"risk_score": w.RiskScore, "amount": w.Amount,
			})
			return c.JSON(fiber.Map{"success": true, "auto_approved": true})
		}
		return c.Status(422).JSON(fiber.Map{"error": "auto-approve conditions not met"})
	})
}

// decideWithdrawal applies a terminal or intermediate decision exactly once.
//
// The UPDATE is guarded by the set of statuses that are still open, so a second
// operator (or a retried request) gets 409 instead of overwriting a decision,
// and the outcome is written to the audit log with the previous status.
func decideWithdrawal(
	c *fiber.Ctx,
	db *gorm.DB,
	log *zap.Logger,
	auditSvc *service.AuditService,
	status string,
	reason ...string,
) error {
	id := c.Params("id")
	if id == "" {
		return c.Status(400).JSON(fiber.Map{"error": "id is required"})
	}

	var current models.Withdrawal
	if err := db.Where("id = ?", id).First(&current).Error; err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			return c.Status(404).JSON(fiber.Map{"error": "not found"})
		}
		log.Error("withdrawal lookup failed", zap.Error(err), zap.String("id", id))
		return c.Status(500).JSON(fiber.Map{"error": "database error"})
	}

	now := time.Now()
	// `approved_by`/`rejected_by` are varchar(36): store the numeric admin id as
	// a string. Passing the raw claim (a float64 from jwt.MapClaims) made GORM
	// write "1234" for some drivers and fail for others.
	adminID := ""
	if id := adminIDInt64(c); id != nil {
		adminID = strconv.FormatInt(*id, 10)
	}
	adminName := ""
	if name, ok := c.Locals("admin_name").(string); ok {
		adminName = name
	}

	updates := map[string]any{"status": status, "updated_at": now}
	switch status {
	case "approved":
		updates["approved_by"] = adminID
		updates["approved_by_name"] = adminName
		updates["approved_at"] = now
	case "rejected":
		updates["rejected_by"] = adminID
		updates["rejected_by_name"] = adminName
		updates["rejected_at"] = now
		updates["reject_reason"] = firstOrEmpty(reason)
	case "held":
		updates["hold_reason"] = firstOrEmpty(reason)
	}

	res := db.Model(&models.Withdrawal{}).
		Where("id = ? AND status IN ?", id, withdrawalDecidable).
		Updates(updates)
	if res.Error != nil {
		log.Error("withdrawal update failed", zap.Error(res.Error), zap.String("id", id))
		return c.Status(500).JSON(fiber.Map{"error": "database error"})
	}
	if res.RowsAffected == 0 {
		return c.Status(409).JSON(fiber.Map{
			"error":           "withdrawal already decided",
			"current_status":  current.Status,
			"requested_status": status,
		})
	}

	logAudit(auditSvc, c, "withdrawal."+status, "withdrawal", id, fiber.Map{
		"from_status": current.Status,
		"to_status":   status,
		"amount":      current.Amount,
		"reason":      firstOrEmpty(reason),
	})

	return c.JSON(fiber.Map{"success": true, "status": status})
}

func firstOrEmpty(values []string) string {
	if len(values) == 0 {
		return ""
	}
	return values[0]
}
