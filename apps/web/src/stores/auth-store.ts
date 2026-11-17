import { create } from "zustand";
import { persist } from "zustand/middleware";
import { authApi, type AuthResult } from "@/lib/api/auth";
import { getErrorMessage } from "@/lib/api/errors";
import { createSingleFlight } from "@/lib/single-flight";

interface User {
  id: string;
  uuid: string;
  email: string;
  username: string;
  phone?: string;
  country_code: string;
  currency_code: string;
  kyc_level: number;
  status: "pending" | "active" | "blocked" | "suspended" | "self_excluded" | "closed";
  created_at: string;
  updated_at: string;
  last_login_at?: string;
}

interface AuthState {
  // State
  user: User | null;
  accessToken: string | null;
  refreshToken: string | null;
  isAuthenticated: boolean;
  isLoading: boolean;
  error: string | null;

  // Actions
  login: (identifier: string, password: string) => Promise<void>;
  register: (email: string, password: string, username: string, countryCode: string, currencyCode: string) => Promise<void>;
  logout: () => Promise<void>;
  fetchUser: () => Promise<void>;
  clearError: () => void;
  setTokens: (accessToken: string, refreshToken: string) => void;
  refreshTokens: () => Promise<void>;
}

const handleAuthResult = (result: AuthResult) => ({
  accessToken: result.tokens.access_token,
  refreshToken: result.tokens.refresh_token,
  isAuthenticated: true,
  isLoading: false,
  error: null,
});

const registerSingleFlight = createSingleFlight<void>();

export const useAuthStore = create<AuthState>()(
  persist(
    (set, get) => ({
      // Initial state
      user: null,
  accessToken: null,
  refreshToken: null,
  isAuthenticated: false,
  isLoading: false,
  error: null,

  setTokens: (accessToken: string, refreshToken: string) => {
    set({ accessToken, refreshToken, isAuthenticated: true });
  },

  login: async (identifier: string, password: string) => {
    set({ isLoading: true, error: null });
    try {
      // ✅ Генерируем уникальный device_id для сессии
      const deviceId = `web-${crypto.randomUUID()}`;
      
      const normalizedIdentifier = identifier.trim();
      const looksLikeEmail = normalizedIdentifier.includes("@");
      const result = await authApi.login({
        identifier: normalizedIdentifier,
        username: normalizedIdentifier,
        email: looksLikeEmail ? normalizedIdentifier : undefined,
        password,
        device_id: deviceId,
        deviceId,
      });
      set(handleAuthResult(result));
      
      const user = await authApi.me();
      set({
        user,
        isLoading: false,
      });
    } catch (error) {
      set({
        isLoading: false,
        error: getErrorMessage(error),
      });
      throw error;
    }
  },

  register: async (email: string, password: string, username: string, countryCode: string, currencyCode: string) =>
    registerSingleFlight.run(async () => {
      set({ isLoading: true, error: null });
      try {
        // Prevent duplicate account creation when users double-click submit.
        const deviceId = `web-${crypto.randomUUID()}`;

        const result = await authApi.register({
          email,
          password,
          username,
          country_code: countryCode,
          countryCode,
          currency_code: currencyCode,
          currencyCode,
          device_id: deviceId,
          deviceId,
        });
        set(handleAuthResult(result));

        const user = await authApi.me();
        set({
          user,
          isLoading: false,
        });
      } catch (error) {
        set({
          isLoading: false,
          error: getErrorMessage(error),
        });
        throw error;
      }
    }),

  logout: async () => {
    try {
      await authApi.logout();
    } catch (error) {
      console.error("Logout error:", error);
    } finally {
      set({
        user: null,
        accessToken: null,
        refreshToken: null,
        isAuthenticated: false,
        error: null,
      });
    }
  },

  fetchUser: async () => {
    // The session lives in an HttpOnly cookie, so an empty in-memory token does
    // not mean "logged out" — ask the API instead.
    set({ isLoading: true });
    try {
      const user = await authApi.me();
      set({
        user,
        isAuthenticated: true,
        isLoading: false,
      });
    } catch (error) {
      set({
        isAuthenticated: false,
        user: null,
        isLoading: false,
      });
    }
  },

  refreshTokens: async () => {
    const { refreshToken } = get();

    try {
      // `refreshToken` may be null: the refresh endpoint reads the HttpOnly
      // cookie when the body carries no token.
      const tokens = await authApi.refresh(refreshToken ?? undefined);
      set({
        accessToken: tokens.access_token,
        refreshToken: tokens.refresh_token,
      });
    } catch (error) {
      set({
        accessToken: null,
        refreshToken: null,
        isAuthenticated: false,
      });
      throw error;
    }
  },

  clearError: () => set({ error: null }),
    }),
    {
      name: "auth-storage",
      // Tokens are delivered as HttpOnly cookies and must never be written to
      // localStorage: a single XSS could read them and keep the session alive.
      // Only the "was authenticated" flag is remembered, and it is re-validated
      // against the API by `fetchUser` on mount.
      partialize: (state) => ({
        isAuthenticated: state.isAuthenticated,
      }),
    }
  )
);
