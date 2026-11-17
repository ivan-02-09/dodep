import { useAuthStore } from "@/stores/auth-store";

// Relative by default: the browser talks to the same origin, and nginx (or the
// Next.js server) proxies /api to the Go services. A hard-coded localhost here
// means "the user's own machine" in production, not the backend host.
const DEFAULT_API_BASE = "/api";
// One origin prefix for every service. API paths in this codebase are written
// with their own service prefix ("/api/v1/auth/login"), so the auth client must
// share the same base — "/api/auth" produced /api/auth/api/v1/auth/login.
const DEFAULT_AUTH_API_BASE = "/api";

type PublicEnv = Record<string, string | undefined>;

export function resolveApiBaseUrl(env: PublicEnv = process.env): string {
  return env.NEXT_PUBLIC_API_URL || DEFAULT_API_BASE;
}

/**
 * Joins a base URL (absolute or relative) with a path, tolerating a trailing
 * slash on the base and a leading slash on the path. Returns a relative href
 * when the base is relative, which is what the browser needs behind nginx.
 */
export function joinUrl(base: string, path: string): string {
  if (!base) return path.startsWith("/") ? path : `/${path}`;
  const trimmedBase = base.endsWith("/") ? base.slice(0, -1) : base;
  const trimmedPath = path.startsWith("/") ? path : `/${path}`;
  return `${trimmedBase}${trimmedPath}`;
}

/**
 * Build the request href from a base and a path that may already carry the same
 * prefix.
 *
 * API paths are written absolute ("/api/v1/bets") while the base may already be
 * "/api" (the production default), which used to produce "/api/api/v1/bets" and
 * a 404 behind nginx. A path that starts with the base's last segment is
 * therefore rebased onto the base instead of duplicated.
 */
export function buildApiHref(base: string, path: string, query = ""): string {
  const trimmedBase = base.endsWith("/") ? base.slice(0, -1) : base;
  let trimmedPath = path.startsWith("/") ? path : `/${path}`;

  const lastSegment = trimmedBase.slice(trimmedBase.lastIndexOf("/"));
  if (lastSegment.length > 1) {
    if (
      trimmedPath === lastSegment ||
      trimmedPath.startsWith(`${lastSegment}/`)
    ) {
      trimmedPath = trimmedPath.slice(lastSegment.length);
    }
  }

  const href = joinUrl(trimmedBase, trimmedPath || "/");
  return query ? `${href}${href.includes("?") ? "&" : "?"}${query}` : href;
}

export function resolveAuthApiBaseUrl(env: PublicEnv = process.env): string {
  return (
    env.NEXT_PUBLIC_AUTH_API_URL ||
    env.NEXT_PUBLIC_API_URL ||
    DEFAULT_AUTH_API_BASE
  );
}

export interface ApiResponse<T> {
  data: T;
  meta?: {
    page?: number;
    total?: number;
    cursor?: string;
  };
}

export interface ApiError {
  code: string;
  message: string;
  details?: Record<string, unknown>;
  request_id: string;
}

function normalizeApiError(raw: unknown, fallbackMessage: string): ApiError {
  if (raw && typeof raw === "object") {
    const payload = raw as Record<string, unknown>;
    const nestedError =
      payload.error && typeof payload.error === "object"
        ? (payload.error as Record<string, unknown>)
        : null;
    const legacyError =
      typeof payload.error === "string" ? payload.error : null;
    const message =
      typeof nestedError?.message === "string"
        ? nestedError.message
        : typeof payload.message === "string"
          ? payload.message
          : legacyError ?? fallbackMessage;

    return {
      code:
        typeof nestedError?.code === "string"
          ? nestedError.code
          : typeof payload.code === "string"
            ? payload.code
            : "UNKNOWN_ERROR",
      message,
      details:
        nestedError?.details && typeof nestedError.details === "object"
          ? (nestedError.details as Record<string, unknown>)
          : payload.details && typeof payload.details === "object"
            ? (payload.details as Record<string, unknown>)
            : undefined,
      request_id:
        typeof payload.request_id === "string" ? payload.request_id : "unknown",
    };
  }

  return {
    code: "UNKNOWN_ERROR",
    message: fallbackMessage,
    request_id: "unknown",
  };
}

export class ApiClientError extends Error {
  constructor(
    public status: number,
    public error: ApiError,
  ) {
    super(error.message);
    this.name = "ApiClientError";
  }

  get isNotFound() {
    return this.status === 404;
  }
  get isUnauthorized() {
    return this.status === 401;
  }
  get isForbidden() {
    return this.status === 403;
  }
  get isValidation() {
    return this.status === 422;
  }
  get isRateLimited() {
    return this.status === 429;
  }
}

class ApiClient {
  private baseUrl: string;

  constructor(baseUrl: string) {
    this.baseUrl = baseUrl;
  }

  private async request<T>(
    method: string,
    path: string,
    options: {
      body?: unknown;
      params?: Record<string, string>;
      headers?: Record<string, string>;
    } = {},
  ): Promise<T> {
    // `new URL("/api/v1/...")` throws TypeError: Invalid URL, because the base
    // is relative in production (NEXT_PUBLIC_API_URL=/api). Build the href by
    // hand and never depend on window.location during SSR/static rendering.
    const query = new URLSearchParams();
    if (options.params) {
      Object.entries(options.params).forEach(([k, v]) => query.set(k, v));
    }
    const href = buildApiHref(this.baseUrl, path, query.toString());

    const { accessToken, refreshTokens, logout } = useAuthStore.getState();

    let token = accessToken;

    // One idempotency key per logical request, reused by the retry after a
    // session refresh: regenerating it inside `makeRequest` would make a failed
    // first attempt and its retry look like two different deposits.
    const idempotencyKey =
      method === "GET" || method === "HEAD" ? undefined : crypto.randomUUID();

    const makeRequest = async (t: string | null): Promise<T> => {
      const requestId = crypto.randomUUID();
      const startedAt = Date.now();
      let response: Response;
      try {
        response = await fetch(href, {
          method,
          // Send the HttpOnly session cookies. They are the primary credential
          // for browser sessions; the Authorization header below is kept for
          // clients that still hold a token (and for the refresh flow).
          credentials: "include",
          headers: {
            "Content-Type": "application/json",
            ...(t ? { Authorization: `Bearer ${t}` } : {}),
            "X-Request-ID": requestId,
            ...(idempotencyKey ? { "X-Idempotency-Key": idempotencyKey } : {}),
            ...options.headers,
          },
          body: options.body ? JSON.stringify(options.body) : undefined,
        });
      } catch (error) {
        const message =
          `Network error while calling ${method} ${href}. ` +
          "Backend is unreachable or refused the connection.";
        console.error("[api] network_error", {
          requestId,
          method,
          url: href,
          elapsedMs: Date.now() - startedAt,
          error,
        });
        throw new ApiClientError(0, {
          code: "NETWORK_CONNECTION_REFUSED",
          message,
          details: {
            method,
            url: href,
            elapsed_ms: Date.now() - startedAt,
          },
          request_id: requestId,
        });
      }

      if (!response.ok) {
        const payload = await response.json().catch(() => null);
        const error = normalizeApiError(payload, response.statusText);
        throw new ApiClientError(response.status, error);
      }

      return response.json();
    };

    try {
      return await makeRequest(token);
    } catch (error) {
      // Auto-refresh on 401. Without a token in memory (cookie session) the
      // refresh endpoint still works: the refresh token travels in its HttpOnly
      // cookie, which is why the condition no longer requires `token`.
      if (error instanceof ApiClientError && error.isUnauthorized) {
        try {
          await refreshTokens();
          token = useAuthStore.getState().accessToken;
          return await makeRequest(token);
        } catch {
          logout();
          if (typeof window !== "undefined") {
            window.location.href = "/login";
          }
          throw new ApiClientError(401, {
            code: "AUTH_TOKEN_EXPIRED",
            message: "Session expired",
            request_id: "unknown",
          });
        }
      }
      throw error;
    }
  }

  get<T>(path: string, params?: Record<string, string>) {
    return this.request<T>("GET", path, { params });
  }

  post<T>(path: string, body?: unknown) {
    return this.request<T>("POST", path, { body });
  }

  patch<T>(path: string, body?: unknown) {
    return this.request<T>("PATCH", path, { body });
  }

  delete<T>(path: string) {
    return this.request<T>("DELETE", path);
  }
}

export function createApiClient(baseUrl: string) {
  return new ApiClient(baseUrl);
}

export const api = createApiClient(resolveApiBaseUrl());
export const authApiClient = createApiClient(resolveAuthApiBaseUrl());
