jest.mock("@/stores/auth-store", () => ({
  useAuthStore: {
    getState: () => ({
      accessToken: null,
      refreshTokens: jest.fn(),
      logout: jest.fn(),
    }),
  },
}));

import {
  ApiClientError,
  buildApiHref,
  createApiClient,
  joinUrl,
  resolveApiBaseUrl,
  resolveAuthApiBaseUrl,
} from "./client";

describe("api client configuration", () => {
  test("prefers explicit auth API url when provided", () => {
    const base = resolveAuthApiBaseUrl({
      NEXT_PUBLIC_AUTH_API_URL: "http://localhost:18083",
      NEXT_PUBLIC_API_URL: "http://localhost:8080",
    });
    expect(base).toBe("http://localhost:18083");
  });

  test("falls back to generic API url for auth when auth-specific url is missing", () => {
    const base = resolveAuthApiBaseUrl({
      NEXT_PUBLIC_API_URL: "http://localhost:8080",
    });
    expect(base).toBe("http://localhost:8080");
  });

  test("defaults to a same-origin /api base when env is empty", () => {
    // The browser must never be pointed at localhost in production: behind
    // nginx the API lives on the same origin under /api.
    expect(resolveApiBaseUrl({})).toBe("/api");
  });

  test("default auth base stays on the same origin under /api", () => {
    // Every path in lib/api is written with its full service prefix
    // ("/api/v1/auth/login"), so the base must be the origin prefix alone.
    // A "/api/auth" base produced /api/auth/api/v1/auth/login and a 404.
    expect(resolveAuthApiBaseUrl({})).toBe("/api");
  });
});

describe("relative URL handling", () => {
  test("joins a relative base without throwing", () => {
    // Previously `new URL("/api/v1/auth/login")` threw
    // "TypeError: Invalid URL" before the request was even attempted.
    expect(joinUrl("/api", "/v1/auth/login")).toBe("/api/v1/auth/login");
  });

  test("joins an absolute base and tolerates slashes", () => {
    expect(joinUrl("http://localhost:8080/", "v1/auth/login")).toBe(
      "http://localhost:8080/v1/auth/login"
    );
  });
});

describe("href building", () => {
  test("does not duplicate the /api prefix", () => {
    // The whole bug in one assertion: base "/api" + path "/api/v1/bets".
    expect(buildApiHref("/api", "/api/v1/bets")).toBe("/api/v1/bets");
    expect(buildApiHref("/api", "/api/v1/auth/login")).toBe("/api/v1/auth/login");
  });

  test("keeps absolute bases and paths intact", () => {
    expect(buildApiHref("http://localhost:8080", "/api/v1/bets")).toBe(
      "http://localhost:8080/api/v1/bets"
    );
  });

  test("rebases a path when the base has a different prefix", () => {
    expect(buildApiHref("/admin-api", "/api/v1/bets")).toBe("/admin-api/api/v1/bets");
  });

  test("appends the query string once", () => {
    expect(buildApiHref("/api", "/api/v1/wallet/balance", "currency=USD")).toBe(
      "/api/v1/wallet/balance?currency=USD"
    );
  });
});

describe("relative-base requests", () => {
  test("issues the request against a same-origin path", async () => {
    const client = createApiClient("/api");
    const fetchMock = jest
      .spyOn(global, "fetch")
      // jsdom does not expose the fetch Response constructor, so a minimal
      // stand-in is used instead.
      .mockResolvedValue({
        ok: true,
        status: 200,
        statusText: "OK",
        json: async () => ({ ok: true }),
      } as unknown as Response);

    try {
      await client.get("/v1/wallet/balance", { currency: "USD" });
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/v1/wallet/balance?currency=USD",
        expect.objectContaining({ method: "GET" })
      );
    } finally {
      fetchMock.mockRestore();
    }
  });
});

describe("session credentials and idempotency", () => {
  test("sends cookies and one idempotency key per mutating request", async () => {
    const client = createApiClient("/api");
    const fetchMock = jest
      .spyOn(global, "fetch")
      .mockResolvedValue({
        ok: true,
        status: 200,
        statusText: "OK",
        json: async () => ({ ok: true }),
      } as unknown as Response);

    try {
      await client.post("/api/v1/wallet/deposit", { amount: 10 });

      const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
      // The HttpOnly session cookie is the credential; without credentials the
      // browser would drop it on a cross-origin deployment.
      expect(init.credentials).toBe("include");
      const headers = init.headers as Record<string, string>;
      expect(headers["X-Idempotency-Key"]).toMatch(
        /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
      );
    } finally {
      fetchMock.mockRestore();
    }
  });

  test("does not send an idempotency key on reads", async () => {
    const client = createApiClient("/api");
    const fetchMock = jest
      .spyOn(global, "fetch")
      .mockResolvedValue({
        ok: true,
        status: 200,
        statusText: "OK",
        json: async () => [],
      } as unknown as Response);

    try {
      await client.get("/api/v1/bets/active");
      const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
      const headers = init.headers as Record<string, string>;
      expect(headers["X-Idempotency-Key"]).toBeUndefined();
    } finally {
      fetchMock.mockRestore();
    }
  });
});

describe("network diagnostics", () => {
  test("converts fetch connection failure to structured ApiClientError", async () => {
    const client = createApiClient("http://localhost:8080");
    const fetchMock = jest
      .spyOn(global, "fetch")
      .mockRejectedValue(new TypeError("fetch failed"));

    let error: unknown;
    try {
      await client.post("/api/v1/auth/register", { email: "broken@example.com" });
    } catch (caught) {
      error = caught;
    } finally {
      fetchMock.mockRestore();
    }

    expect(error).toBeInstanceOf(ApiClientError);
    const apiError = error as ApiClientError;
    expect(apiError.status).toBe(0);
    expect(apiError.error.code).toBe("NETWORK_CONNECTION_REFUSED");
    expect(apiError.error.message).toContain("http://localhost:8080/api/v1/auth/register");
  });
});
