import { describe, expect, it, vi } from "vitest";
import { relayMethods } from "./relayProtocol";
import type {
  RelayMethod,
  RelayMethodParams,
  RelayMethodResult,
} from "./relayProtocol";
import { createRelaySocketGateway } from "./relaySocketGateway";
import type {
  RelaySocketClient,
  RelaySocketClientEvent,
} from "./relaySocketClient";
import type {
  ApprovalResolution,
  RemoteBootstrap,
  RemoteThreadRecord,
  SendTurnInput,
} from "./types";

describe("relay socket gateway", () => {
  it("hydrates bootstrap state from the relay client and pushes updates to listeners", async () => {
    const client = createMockRelayClient();
    const gateway = createRelaySocketGateway({
      client,
    });
    const listener = vi.fn();

    const unsubscribe = gateway.subscribeToBootstrap(listener);

    await vi.waitFor(() => {
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({
          session: expect.objectContaining({ pairingCode: "PAIR-RELAY" }),
        }),
      );
    });

    client.emit({
      status: "reconnecting",
      type: "connection/status",
    });

    expect(listener).toHaveBeenLastCalledWith(
      expect.objectContaining({
        hosts: [expect.objectContaining({ relayStatus: "Relay reconnecting" })],
      }),
    );

    client.emit({
      status: "connected",
      type: "connection/status",
    });

    expect(listener).toHaveBeenLastCalledWith(
      expect.objectContaining({
        hosts: [expect.objectContaining({ relayStatus: "Relay protected" })],
      }),
    );

    client.emit({
      bootstrap: {
        ...createBootstrap(),
        hosts: [
          {
            ...createBootstrap().hosts[0],
            relayStatus: "Relay reconnecting",
          },
        ],
      },
      type: "bootstrap/update",
    });

    expect(listener).toHaveBeenLastCalledWith(
      expect.objectContaining({
        hosts: [expect.objectContaining({ relayStatus: "Relay reconnecting" })],
      }),
    );

    unsubscribe();
  });

  it("hydrates thread state, manages relay subscriptions, and routes thread actions through relay requests", async () => {
    const client = createMockRelayClient();
    const gateway = createRelaySocketGateway({
      client,
    });
    const listener = vi.fn();

    const unsubscribe = gateway.subscribeToThread(
      "host-relay",
      "thread-relay",
      listener,
    );

    await vi.waitFor(() => {
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({
          runtime: expect.objectContaining({ statusCopy: "Thread is active on the relay host." }),
        }),
      );
    });

    client.emit({
      hostId: "host-relay",
      record: {
        ...createThreadRecord(),
        runtime: {
          ...createThreadRecord().runtime,
          statusCopy: "Relay thread update delivered.",
        },
      },
      threadId: "thread-relay",
      type: "thread/update",
    });

    expect(listener).toHaveBeenLastCalledWith(
      expect.objectContaining({
        runtime: expect.objectContaining({
          statusCopy: "Relay thread update delivered.",
        }),
      }),
    );

    const resolution: ApprovalResolution = {
      decision: "accept",
      requestId: "request-1",
    };
    await gateway.resolveApproval("host-relay", "thread-relay", resolution);
    await gateway.interruptTurn("host-relay", "thread-relay");
    const input: SendTurnInput = {
      mode: "steer",
      text: "Keep the reconnect path visible.",
    };
    await gateway.sendPrompt("host-relay", "thread-relay", input);

    client.emit({
      status: "reconnecting",
      type: "connection/status",
    });
    client.emit({
      status: "connected",
      type: "connection/status",
    });

    unsubscribe();

    expect(client.requests).toEqual([
      [
        relayMethods.threadSubscribe,
        {
          hostId: "host-relay",
          threadId: "thread-relay",
        },
      ],
      [
        relayMethods.threadRead,
        {
          hostId: "host-relay",
          threadId: "thread-relay",
        },
      ],
      [
        relayMethods.approvalResolve,
        {
          hostId: "host-relay",
          resolution,
          threadId: "thread-relay",
        },
      ],
      [
        relayMethods.turnInterrupt,
        {
          hostId: "host-relay",
          threadId: "thread-relay",
        },
      ],
      [
        relayMethods.turnPrompt,
        {
          hostId: "host-relay",
          input,
          threadId: "thread-relay",
        },
      ],
      [
        relayMethods.threadSubscribe,
        {
          hostId: "host-relay",
          threadId: "thread-relay",
        },
      ],
      [
        relayMethods.threadRead,
        {
          hostId: "host-relay",
          threadId: "thread-relay",
        },
      ],
      [
        relayMethods.threadUnsubscribe,
        {
          hostId: "host-relay",
          threadId: "thread-relay",
        },
      ],
    ]);
  });
});

function createMockRelayClient(): RelaySocketClient & {
  emit: (event: RelaySocketClientEvent) => void;
  requests: Array<[string, unknown]>;
} {
  const listeners = new Set<(event: RelaySocketClientEvent) => void>();
  const requests: Array<[string, unknown]> = [];
  const bootstrap = createBootstrap();
  const record = createThreadRecord();

  return {
    close() {},
    emit(event) {
      for (const listener of listeners) {
        listener(event);
      }
    },
    async request<M extends RelayMethod>(
      method: M,
      ...params: RelayMethodParams[M] extends undefined
        ? []
        : [params: RelayMethodParams[M]]
    ): Promise<RelayMethodResult[M]> {
      const requestParams = params[0] as RelayMethodParams[M];
      requests.push([method, requestParams]);
      if (method === relayMethods.bootstrapGet) {
        return bootstrap as RelayMethodResult[M];
      }
      if (method === relayMethods.threadRead) {
        return record as RelayMethodResult[M];
      }
      return undefined as RelayMethodResult[M];
    },
    requests,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

function createBootstrap(): RemoteBootstrap {
  return {
    deviceGroups: [
      {
        devices: [
          {
            id: "device-relay",
            lastSeenAt: 1720000200,
            name: "Relay iPhone",
            pairedAt: 1720000000,
            transport: "Relay + E2EE",
            trust: "trusted",
          },
        ],
        host: {
          detail: "Relay host online",
          id: "host-relay",
          lastSeenAt: 1720000300,
          name: "Relay MacBook",
          pairedAt: 1720000000,
          platform: "macOS",
          relayStatus: "Relay protected",
          status: "online",
        },
      },
    ],
    hosts: [
      {
        detail: "Relay host online",
        id: "host-relay",
        lastSeenAt: 1720000300,
        name: "Relay MacBook",
        pairedAt: 1720000000,
        platform: "macOS",
        relayStatus: "Relay protected",
        status: "online",
      },
    ],
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
  };
}

function createThreadRecord(): RemoteThreadRecord {
  return {
    approvals: [],
    hostId: "host-relay",
    runtime: {
      composerMode: "steer",
      connection: "online",
      phase: "running",
      statusCopy: "Thread is active on the relay host.",
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
      preview: "Relay managed thread.",
      source: "appServer",
      status: { type: "active", activeFlags: [] },
      turns: [],
      updatedAt: 1720000300,
    },
  };
}
