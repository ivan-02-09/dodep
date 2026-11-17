import apiClient from "./api";
import type { ApiResponse, PaginatedResponse } from "@/types/api";
import type {
  Deposit,
  Withdrawal,
  Transaction,
  FinanceSearchParams,
} from "@/types/finance";

export const financeService = {
  async getDeposits(
    params: FinanceSearchParams,
  ): Promise<PaginatedResponse<Deposit>> {
    const response = await apiClient.get<PaginatedResponse<Deposit>>(
      "/admin/finance/deposits",
      { params },
    );
    return response.data;
  },

  async getDeposit(depositId: string): Promise<Deposit> {
    const response = await apiClient.get<ApiResponse<Deposit>>(
      `/admin/finance/deposits/${depositId}`,
    );
    return response.data.data;
  },

  async getWithdrawals(
    params: FinanceSearchParams,
  ): Promise<PaginatedResponse<Withdrawal>> {
    const response = await apiClient.get<PaginatedResponse<Withdrawal>>(
      "/admin/finance/withdrawals",
      { params },
    );
    return response.data;
  },

  // The withdrawal queue is served by handlers/withdrawals.go under
  // /admin/withdrawals — not /admin/finance/withdrawals, which only exposes the
  // read-only projection coming from the payment gRPC service. The old paths
  // answered 404 (approve/reject) or 405, so the queue buttons did nothing.
  async getWithdrawal(withdrawalId: string): Promise<Withdrawal> {
    const response = await apiClient.get<ApiResponse<Withdrawal>>(
      `/admin/withdrawals/${withdrawalId}`,
    );
    return response.data.data;
  },

  async approveWithdrawal(withdrawalId: string): Promise<void> {
    await apiClient.post(`/admin/withdrawals/${withdrawalId}/approve`);
  },

  async rejectWithdrawal(withdrawalId: string, reason: string): Promise<void> {
    // The BFF route is /decline and reads {"reason": "..."}.
    await apiClient.post(`/admin/withdrawals/${withdrawalId}/decline`, {
      reason,
    });
  },

  async getTransactions(
    params: FinanceSearchParams,
  ): Promise<PaginatedResponse<Transaction>> {
    const response = await apiClient.get<PaginatedResponse<Transaction>>(
      "/admin/finance/transactions",
      { params },
    );
    return response.data;
  },

  async adjustBalance(
    userId: string,
    data: {
      amount: string;
      currency: string;
      reason: string;
      type: "credit" | "debit";
    },
  ): Promise<void> {
    // Balance adjustments live under /admin/players/:id/adjust-balance; the
    // /admin/finance/users/... path never existed.
    await apiClient.post(`/admin/players/${userId}/adjust-balance`, data);
  },

  async getFinancialSummary(params?: {
    date_from?: string;
    date_to?: string;
  }): Promise<{
    total_deposits: string;
    total_withdrawals: string;
    net_revenue: string;
    ggr: string;
    pending_withdrawals_count: number;
    pending_withdrawals_amount: string;
  }> {
    const response = await apiClient.get("/admin/finance/summary", { params });
    return response.data.data;
  },
};
