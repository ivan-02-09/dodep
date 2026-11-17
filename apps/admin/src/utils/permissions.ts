import type { AdminRole, Permission } from "@/types/admin";

/** Every permission the UI knows about. */
export const ALL_PERMISSIONS = (): Permission[] => [
  "user.view",
  "user.edit",
  "user.block",
  "user.delete",
  "user.merge",
  "bet.view",
  "bet.void",
  "transaction.view",
  "transaction.adjust",
  "withdrawal.approve_small",
  "withdrawal.approve_large",
  "finance.balance_sheet",
  "finance.crypto_wallet",
  "fraud.review",
  "fraud.rule_builder",
  "fraud.screening",
  "bonus.create",
  "bonus.edit",
  "bonus.grant",
  "casino.manage",
  "casino.rtp_config",
  "sportsbook.manage",
  "sportsbook.trading_terminal",
  "affiliate.manage",
  "content.manage",
  "crm.campaign",
  "crm.segment",
  "communication.manage",
  "kyc.review",
  "kyc.sof_review",
  "reports.view",
  "reports.export",
  "system.config",
  "system.maintenance",
  "admin.manage",
  "audit.view",
];

/**
 * Permission set per role.
 *
 * The backend is authoritative: see `RolePermissions` in
 * services/go/admin-bff/internal/middleware/admin_auth.go. This map mirrors it
 * so the UI hides exactly what the API will reject.
 */
const ROLE_PERMISSIONS: Record<AdminRole, Permission[]> = {
  SUPER_ADMIN: ALL_PERMISSIONS(),
  ADMIN: [
    "user.view",
    "user.edit",
    "user.block",
    "user.merge",
    "bet.view",
    "bet.void",
    "transaction.view",
    "transaction.adjust",
    "withdrawal.approve_small",
    "withdrawal.approve_large",
    "finance.balance_sheet",
    "finance.crypto_wallet",
    "fraud.review",
    "fraud.screening",
    "bonus.create",
    "bonus.edit",
    "bonus.grant",
    "casino.manage",
    "sportsbook.manage",
    "affiliate.manage",
    "crm.campaign",
    "crm.segment",
    "communication.manage",
    "kyc.review",
    "kyc.sof_review",
    "reports.view",
    "reports.export",
    "system.config",
    "audit.view",
    "admin.manage",
  ],
  // Finance moves money but does not manage admins or casino content.
  FINANCE: [
    "user.view",
    "transaction.view",
    "transaction.adjust",
    "withdrawal.approve_small",
    "withdrawal.approve_large",
    "finance.balance_sheet",
    "finance.crypto_wallet",
    "reports.view",
    "reports.export",
    "audit.view",
  ],
  RISK: [
    "user.view",
    "user.block",
    "bet.view",
    "bet.void",
    "transaction.view",
    "withdrawal.approve_small",
    "fraud.review",
    "fraud.rule_builder",
    "fraud.screening",
    "reports.view",
    "audit.view",
  ],
  // Support looks, but never moves money or blocks accounts.
  SUPPORT: [
    "user.view",
    "bet.view",
    "transaction.view",
    "communication.manage",
    "kyc.review",
    "reports.view",
  ],
  VIEWER: ["user.view", "bet.view", "transaction.view", "reports.view"],
};

/**
 * Resolve the permissions for a role.
 *
 * The API issues snake_case (`super_admin`), the UI stores UPPER_SNAKE; both
 * resolve to the same set.
 */
export function getPermissionsForRole(role: AdminRole | string): Permission[] {
  const key = String(role || "")
    .trim()
    .toUpperCase()
    .replace(/[\s-]+/g, "_");
  return ROLE_PERMISSIONS[key as AdminRole] || [];
}

export function hasPermission(
  permissions: Permission[],
  required: Permission,
): boolean {
  return permissions.includes(required);
}

export function hasAnyPermission(
  permissions: Permission[],
  required: Permission[],
): boolean {
  return required.some((p) => permissions.includes(p));
}

export function hasAllPermissions(
  permissions: Permission[],
  required: Permission[],
): boolean {
  return required.every((p) => permissions.includes(p));
}
