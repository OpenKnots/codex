import { describe, expect, it, vi } from "vitest";
import { createRelaySocketClient } from "./relaySocketClient";
import type {
  RelaySocketClientEvent,
  RelaySocketEnvelope,
  RelaySocketLike,
} from "./relaySocketClient";

describe("relay socket client", () => {
  it("sends typed requests and resolves typed responses", async () => {
    const sockets: FakeSocket[] = [];
    const client = createRelaySocketClient({
      createSocket(url) {
        const socket = new FakeSocket(url);
        sockets.push(socket);
        return socket;
      },
      reconnectDelayMs: 5,
      url: "wss://relay.example.test/mobile",
    });

    const request = client.request<{ ok: boolean }>("bootstrap/get", {
      includeThreads: false,
    });

    expect(sockets).toHaveLength(1);
    sockets[0]?.open();

    await vi.waitFor(() => {
      expect(sockets[0]?.lastSent()).toMatchObject({
        method: "bootstrap/get",
        params: {
          includeThreads: false,
        },
        type: "request",
      });
    });
    const sent = sockets[0]?.lastSent();
    if (!sent || sent.type !== "request") {
      throw new Error("expected a relay request envelope");
    }

    sockets[0]?.message({
      ok: true,
      requestId: sent?.requestId,
      result: {
        ok: true,
      },
      type: "response",
    });

    await expect(request).resolves.toEqual({
      ok: true,
    });
  });

  it("publishes notifications to subscribers and reconnects after an unexpected close", async () => {
    const sockets: FakeSocket[] = [];
    const client = createRelaySocketClient({
      createSocket(url) {
        const socket = new FakeSocket(url);
        sockets.push(socket);
        return socket;
      },
      reconnectDelayMs: 5,
      url: "wss://relay.example.test/mobile",
    });
    const listener = vi.fn<(event: RelaySocketClientEvent) => void>();

    const unsubscribe = client.subscribe(listener);

    expect(sockets).toHaveLength(1);
    sockets[0]?.open();
    expect(listener).toHaveBeenCalledWith({
      status: "connected",
      type: "connection/status",
    });
    sockets[0]?.message({
      bootstrap: {
        deviceGroups: [],
        hosts: [],
        session: {
          accountLabel: "relay@openai.com",
          nativeCapabilities: {
            fileImport: true,
            qrScanner: true,
            relaySockets: true,
            secureStore: true,
          },
          pairingCode: "PAIR-RELAY",
          pairingUrl: "codex://remote/pair?code=PAIR-RELAY",
          signedIn: true,
          workspaceLabel: "Relay Workspace",
        },
      },
      type: "bootstrap/update",
    });

    expect(listener).toHaveBeenCalledWith({
      bootstrap: expect.objectContaining({
        session: expect.objectContaining({ pairingCode: "PAIR-RELAY" }),
      }),
      type: "bootstrap/update",
    });

    sockets[0]?.disconnect({
      code: 1006,
      wasClean: false,
    });
    expect(listener).toHaveBeenCalledWith({
      status: "reconnecting",
      type: "connection/status",
    });

    await vi.waitFor(() => {
      expect(sockets).toHaveLength(2);
    });

    sockets[1]?.open();
    expect(listener).toHaveBeenCalledWith({
      status: "connected",
      type: "connection/status",
    });
    sockets[1]?.message({
      hostId: "host-relay",
      record: {
        approvals: [],
        hostId: "host-relay",
        runtime: {
          composerMode: "newTurn",
          connection: "online",
          phase: "completed",
          statusCopy: "Relay recovered.",
        },
        thread: {
          agentNickname: null,
          agentRole: null,
          cliVersion: "1.0.0",
          createdAt: 1720000000,
          cwd: "/workspace/codex",
          ephemeral: false,
          gitInfo: null,
          id: "thread-relay",
          modelProvider: "openai",
          name: "Relay thread",
          path: null,
          preview: "Recovered from reconnect.",
          source: "appServer",
          status: { type: "idle" },
          turns: [],
          updatedAt: 1720000300,
        },
      },
      threadId: "thread-relay",
      type: "thread/update",
    });

    expect(listener).toHaveBeenLastCalledWith({
      hostId: "host-relay",
      record: expect.objectContaining({
        runtime: expect.objectContaining({ statusCopy: "Relay recovered." }),
      }),
      threadId: "thread-relay",
      type: "thread/update",
    });

    unsubscribe();
    client.close();
  });
});

class FakeSocket implements RelaySocketLike {
  onclose: ((event: CloseEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent<string>) => void) | null = null;
  onopen: ((event: Event) => void) | null = null;
  readyState = 0;
  readonly sent: RelaySocketEnvelope[] = [];

  constructor(readonly url: string) {}

  close() {
    this.readyState = 3;
    this.onclose?.({
      code: 1000,
      reason: "",
      wasClean: true,
    } as CloseEvent);
  }

  disconnect(event?: { code: number; wasClean: boolean }) {
    this.readyState = 3;
    this.onclose?.({
      code: event?.code ?? 1000,
      reason: "",
      wasClean: event?.wasClean ?? true,
    } as CloseEvent);
  }

  lastSent() {
    return this.sent.at(-1);
  }

  message(payload: RelaySocketEnvelope) {
    this.onmessage?.({
      data: JSON.stringify(payload),
    } as MessageEvent<string>);
  }

  open() {
    this.readyState = 1;
    this.onopen?.({} as Event);
  }

  send(message: string) {
    this.sent.push(JSON.parse(message) as RelaySocketEnvelope);
  }
}
