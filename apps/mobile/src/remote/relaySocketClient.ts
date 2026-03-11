import type {
  RelayMethod,
  RelayMethodParams,
  RelayMethodResult,
  RelayRequestEnvelope,
  RelayServerNotification,
  RelayWireEnvelope,
} from "./relayProtocol";

export type RelaySocketClientEvent =
  | {
      type: "connection/status";
      status: "connected" | "reconnecting";
    }
  | RelayServerNotification;

export interface RelaySocketLike {
  onclose: ((event: CloseEvent) => void) | null;
  onerror: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent<string>) => void) | null;
  onopen: ((event: Event) => void) | null;
  readyState: number;
  close(code?: number, reason?: string): void;
  send(message: string): void;
}

export interface RelaySocketClient {
  request<M extends RelayMethod>(
    method: M,
    ...params: RelayMethodParams[M] extends undefined
      ? []
      : [params: RelayMethodParams[M]]
  ): Promise<RelayMethodResult[M]>;
  subscribe(listener: (event: RelaySocketClientEvent) => void): () => void;
  close(): void;
}

type RelaySocketClientOptions = {
  url: string;
  reconnectDelayMs?: number;
  createSocket?: (url: string) => RelaySocketLike;
};

type PendingRequest = {
  reject: (error: Error) => void;
  resolve: (value: unknown) => void;
};

const DEFAULT_RECONNECT_DELAY_MS = 1_000;

export function createRelaySocketClient(
  options: RelaySocketClientOptions,
): RelaySocketClient {
  const createSocket =
    options.createSocket ?? ((url: string) => new WebSocket(url));
  const listeners = new Set<(event: RelaySocketClientEvent) => void>();
  const pendingRequests = new Map<string, PendingRequest>();
  const reconnectDelayMs =
    options.reconnectDelayMs ?? DEFAULT_RECONNECT_DELAY_MS;
  let connectPromise: Promise<RelaySocketLike> | null = null;
  let manuallyClosed = false;
  let nextRequestId = 0;
  let reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  let socket: RelaySocketLike | null = null;

  function rejectPendingRequests(message: string) {
    for (const [, pending] of pendingRequests) {
      pending.reject(new Error(message));
    }
    pendingRequests.clear();
  }

  function scheduleReconnect() {
    if (manuallyClosed || reconnectTimer || listeners.size === 0) {
      return;
    }
    reconnectTimer = setTimeout(() => {
      reconnectTimer = undefined;
      void ensureConnected();
    }, reconnectDelayMs);
  }

  function handleMessage(rawMessage: string) {
    const envelope = JSON.parse(rawMessage) as RelayWireEnvelope;
    if (envelope.type === "request") {
      return;
    }
    if (envelope.type === "response") {
      const pending = pendingRequests.get(envelope.requestId);
      if (!pending) {
        return;
      }
      pendingRequests.delete(envelope.requestId);
      if (envelope.ok) {
        pending.resolve(envelope.result);
      } else {
        pending.reject(new Error(envelope.error));
      }
      return;
    }

    for (const listener of listeners) {
      listener(envelope);
    }
  }

  function connectSocket(): Promise<RelaySocketLike> {
    if (socket?.readyState === 1) {
      return Promise.resolve(socket);
    }
    if (connectPromise) {
      return connectPromise;
    }

    connectPromise = new Promise<RelaySocketLike>((resolve, reject) => {
      const nextSocket = createSocket(options.url);
      socket = nextSocket;

      nextSocket.onopen = () => {
        connectPromise = null;
        for (const listener of listeners) {
          listener({
            status: "connected",
            type: "connection/status",
          });
        }
        resolve(nextSocket);
      };
      nextSocket.onmessage = (event: MessageEvent<string>) => {
        handleMessage(event.data);
      };
      nextSocket.onerror = () => {
        if (connectPromise) {
          connectPromise = null;
          reject(new Error("Relay socket connection failed."));
        }
      };
      nextSocket.onclose = (event: CloseEvent) => {
        socket = null;
        if (connectPromise) {
          connectPromise = null;
          reject(new Error("Relay socket closed before opening."));
          return;
        }
        rejectPendingRequests("Relay socket disconnected.");
        if (!manuallyClosed && !event.wasClean) {
          for (const listener of listeners) {
            listener({
              status: "reconnecting",
              type: "connection/status",
            });
          }
          scheduleReconnect();
        }
      };
    });

    return connectPromise;
  }

  async function ensureConnected(): Promise<RelaySocketLike> {
    try {
      return await connectSocket();
    } catch (error) {
      scheduleReconnect();
      throw error;
    }
  }

  return {
    close() {
      manuallyClosed = true;
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = undefined;
      }
      rejectPendingRequests("Relay socket closed.");
      socket?.close();
      socket = null;
      connectPromise = null;
    },
    async request<M extends RelayMethod>(
      method: M,
      ...params: RelayMethodParams[M] extends undefined
        ? []
        : [params: RelayMethodParams[M]]
    ): Promise<RelayMethodResult[M]> {
      const activeSocket = await ensureConnected();
      const requestId = `request-${nextRequestId}`;
      nextRequestId += 1;

      const result = new Promise<RelayMethodResult[M]>((resolve, reject) => {
        pendingRequests.set(requestId, {
          reject,
          resolve: (value) => {
            resolve(value as RelayMethodResult[M]);
          },
        });
      });

      const requestParams = params[0] as RelayMethodParams[M];
      const envelope = (
        requestParams === undefined
          ? {
              method,
              requestId,
              type: "request",
            }
          : {
              method,
              params: requestParams,
              requestId,
              type: "request",
            }
      ) as RelayRequestEnvelope<M>;

      activeSocket.send(JSON.stringify(envelope));

      return result;
    },
    subscribe(listener) {
      listeners.add(listener);
      if (!manuallyClosed) {
        void ensureConnected().catch(() => {});
      }
      return () => {
        listeners.delete(listener);
      };
    },
  };
}
