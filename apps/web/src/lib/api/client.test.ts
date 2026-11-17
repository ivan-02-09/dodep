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

  test("default auth base stays on the same origin", () => {
    expect(resolveAuthApiBaseUrl({})).toBe("/api/auth");
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
