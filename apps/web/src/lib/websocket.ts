import { useEffect, useRef, useCallback, useState } from "react";
import { useAuthStore } from "@/stores/auth-store";

interface WsMessage {
  type: string;
  channel: string;
  data: unknown;
  timestamp: string;
}

interface WsClientOptions {
  url?: string;
  reconnectDelay?: number;
  maxReconnectAttempts?: number;
}

class WebSocketManager {
  private ws: WebSocket | null = null;
  private subscriptions = new Map<string, Set<(data: unknown) => void>>();
  private reconnectAttempts = 0;
  private maxReconnectAttempts: number;
  private reconnectDelay: number;
  private url: string;
  private heartbeatInterval: NodeJS.Timeout | null = null;
  private token: string | null = null;
  /** True while the socket is expected to stay connected. */
  private shouldReconnect = false;

  constructor(options: WsClientOptions = {}) {
    this.url = normalizeWsUrl(options.url || process.env.NEXT_PUBLIC_WS_URL);
    this.reconnectDelay = options.reconnectDelay || 1000;
    this.maxReconnectAttempts = options.maxReconnectAttempts || 10;
  }

  /**
   * Open the socket.
   *
   * `token` is optional: a browser session authenticates with its HttpOnly
   * cookie, which the browser attaches to the handshake automatically. Passing a
   * token in the query string keeps native clients working, but it must never be
   * the only path — the cookie is unreadable from JavaScript by design.
   */
  connect(token?: string) {
    if (this.ws?.readyState === WebSocket.OPEN) return;

    this.token = token ?? null;
    this.shouldReconnect = true;
    const wsUrl = new URL(this.url);
    if (token) {
      wsUrl.searchParams.set("token", token);
    }

    this.ws = new WebSocket(wsUrl.toString());

    this.ws.onopen = () => {
      this.reconnectAttempts = 0;
      this.startHeartbeat();
      // Resubscribe to all channels
      this.subscriptions.forEach((_, channel) => {
        this.send({ action: "subscribe", channel });
      });
    };

    this.ws.onmessage = (event) => {
      try {
        const msg: WsMessage = JSON.parse(event.data);
        if (msg.type === "pong") return;

        const handlers = this.subscriptions.get(msg.channel);
        handlers?.forEach((handler) => handler(msg.data));
      } catch (error) {
        console.error("Failed to parse WebSocket message:", error);
      }
    };

    this.ws.onclose = () => {
      this.stopHeartbeat();
      this.tryReconnect();
    };

    this.ws.onerror = (error) => {
      console.error("WebSocket error:", error);
    };
  }

  subscribe<T = unknown>(
    channel: string,
    handler: (data: T) => void,
  ): () => void {
    if (!this.subscriptions.has(channel)) {
      this.subscriptions.set(channel, new Set());
      if (this.ws?.readyState === WebSocket.OPEN) {
        this.send({ action: "subscribe", channel });
      }
    }
    this.subscriptions.get(channel)!.add(handler as (data: unknown) => void);

    // Return unsubscribe function
    return () => {
      const handlers = this.subscriptions.get(channel);
      if (handlers) {
        handlers.delete(handler as (data: unknown) => void);
        if (handlers.size === 0) {
          this.subscriptions.delete(channel);
          if (this.ws?.readyState === WebSocket.OPEN) {
            this.send({ action: "unsubscribe", channel });
          }
        }
      }
    };
  }

  send(data: Record<string, unknown>) {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(data));
    }
  }

  private startHeartbeat() {
    this.heartbeatInterval = setInterval(() => {
      this.send({ type: "ping" });
    }, 30000);
  }

  private stopHeartbeat() {
    if (this.heartbeatInterval) {
      clearInterval(this.heartbeatInterval);
      this.heartbeatInterval = null;
    }
  }

  private tryReconnect() {
    // The token is optional (browser sessions rely on the HttpOnly cookie), so
    // reconnection must not be gated on it — otherwise a cookie-authenticated
    // socket never comes back after a network blip.
    if (!this.shouldReconnect) return;
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      console.error("Max WebSocket reconnection attempts reached");
      return;
    }

    const delay = Math.min(
      this.reconnectDelay * Math.pow(2, this.reconnectAttempts),
      30000,
    );
    this.reconnectAttempts++;

    setTimeout(() => {
      if (this.shouldReconnect) {
        this.connect(this.token ?? undefined);
      }
    }, delay);
  }

  disconnect() {
    this.shouldReconnect = false;
    this.stopHeartbeat();
    this.ws?.close();
    this.ws = null;
    this.subscriptions.clear();
    this.token = null;
  }
}

/**
 * Resolve the WebSocket endpoint to an absolute ws:// or wss:// URL.
 *
 * `NEXT_PUBLIC_WS_URL` is built as a path in the Docker image (`/ws`) and as a
 * full URL in docker-compose. `new WebSocket("/ws")` throws in the browser, so a
 * relative value is expanded against the current origin here instead of failing
 * at runtime. Port 8080 remains the local development default.
 */
export function normalizeWsUrl(raw?: string): string {
  const candidate = (raw || "").trim();
  if (candidate.startsWith("ws://") || candidate.startsWith("wss://")) {
    return candidate;
  }
  if (candidate.startsWith("/") && typeof window !== "undefined") {
    const scheme = window.location.protocol === "https:" ? "wss:" : "ws:";
    return `${scheme}//${window.location.host}${candidate}`;
  }
  return "ws://localhost:8080";
}

// Singleton instance
let wsManager: WebSocketManager | null = null;

export function getWebSocketManager(): WebSocketManager {
  if (!wsManager) {
    wsManager = new WebSocketManager();
  }
  return wsManager;
}

/**
 * React hook for using WebSocket
 */
export function useWebSocket<T = unknown>(
  channel: string,
  onMessage: (data: T) => void,
  enabled: boolean = true,
) {
  const wsRef = useRef<WebSocketManager | null>(null);
  const unsubscribeRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    if (!enabled) return;

    wsRef.current = getWebSocketManager();
    const ws = wsRef.current;

    // Browser sessions authenticate with the HttpOnly `access_token` cookie,
    // which the browser attaches to the WebSocket handshake on its own (the
    // cookie is not readable from JavaScript). A token is only forwarded when a
    // native client holds one explicitly — the old `document.cookie` lookup for
    // "auth_token" read a cookie nobody ever set, so subscriptions silently
    // never connected.
    ws.connect(useAuthStore.getState().accessToken ?? undefined);

    unsubscribeRef.current = ws.subscribe<T>(channel, onMessage);

    return () => {
      unsubscribeRef.current?.();
    };
  }, [channel, onMessage, enabled]);

  const sendMessage = useCallback((data: Record<string, unknown>) => {
    wsRef.current?.send(data);
  }, []);

  return { sendMessage };
}
