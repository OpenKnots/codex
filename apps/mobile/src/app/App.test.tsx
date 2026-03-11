import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "./App";
import { createMockGateway } from "../remote/mockGateway";
import { relayMethods } from "../remote/relayProtocol";
import type {
  RelayMethod,
  RelayMethodParams,
  RelayMethodResult,
} from "../remote/relayProtocol";
import { createRelaySocketGateway } from "../remote/relaySocketGateway";
import type {
  RelaySocketClient,
  RelaySocketClientEvent,
} from "../remote/relaySocketClient";
import type { RemoteBootstrap, RemoteGateway } from "../remote/types";

describe("Codex remote mobile shell", () => {
  it("shows the sign-in and pairing flow before a session exists", async () => {
    render(
      <App
        gateway={createMockGateway({
          signedIn: false,
        })}
        initialEntries={["/"]}
      />,
    );

    expect(
      await screen.findByRole("heading", { name: /sign in to codex remote/i }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: /continue with openai/i }),
    ).toBeVisible();
    expect(screen.getByText(/scan a host pairing code/i)).toBeVisible();
  });

  it("navigates from hosts to threads and shows the live approval sheet", async () => {
    const user = userEvent.setup();

    render(<App gateway={createMockGateway()} initialEntries={["/hosts"]} />);

    await user.click(
      await screen.findByRole("link", { name: /studio macbook/i }),
    );
    await user.click(
      await screen.findByRole("link", { name: /remote ios daemon/i }),
    );

    expect(
      await screen.findByRole("heading", { name: /remote ios daemon/i }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: /interrupt turn/i }),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", { name: /approval required/i }),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", { name: /waiting on approval/i }),
    ).toBeVisible();
    expect(screen.getByText(/1 approval/i)).toBeVisible();
    expect(screen.getAllByText(/pnpm tauri ios dev/i)).toHaveLength(3);
  });

  it("routes approval and interrupt actions through the gateway", async () => {
    const user = userEvent.setup();
    const gateway = createMockGateway();

    render(
      <App
        gateway={gateway}
        initialEntries={["/hosts/host-studio/threads/thread-ios"]}
      />,
    );

    await waitFor(() =>
      expect(
        screen.getByRole("heading", { name: /remote ios daemon/i }),
      ).toBeVisible(),
    );

    await user.click(screen.getByRole("button", { name: /approve for turn/i }));
    await user.click(screen.getByRole("button", { name: /interrupt turn/i }));

    expect(gateway.inspect().approvalsResolved).toEqual([
      {
        decision: "accept",
        requestId: "approval-turn-shell",
      },
    ]);
    expect(gateway.inspect().interrupts).toEqual(["thread-ios"]);
  });

  it("disables steering when the thread is ready for a new turn", async () => {
    render(
      <App
        gateway={createMockGateway()}
        initialEntries={["/hosts/host-studio/threads/thread-review"]}
      />,
    );

    expect(
      await screen.findByRole("heading", { name: /ready for input/i }),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: /steer in-flight/i })).toBeDisabled();
  });

  it("refreshes host bootstrap surfaces when the gateway pushes connector updates", async () => {
    const gateway = await createBootstrapGateway();

    render(<App gateway={gateway.gateway} initialEntries={["/hosts"]} />);

    expect(await screen.findByText(/relay protected/i)).toBeVisible();

    await act(async () => {
      gateway.pushBootstrap((bootstrap) => ({
        ...bootstrap,
        deviceGroups: bootstrap.deviceGroups.map((group) => ({
          ...group,
          host:
            group.host.id === "host-studio"
              ? {
                  ...group.host,
                  relayStatus: "Relay reconnecting",
                }
              : group.host,
        })),
        hosts: bootstrap.hosts.map((host) => ({
          ...host,
          relayStatus:
            host.id === "host-studio" ? "Relay reconnecting" : host.relayStatus,
        })),
      }));
    });

    expect(await screen.findByText(/relay reconnecting/i)).toBeVisible();
  });

  it("renders relay approval updates through the same approval sheet flow", async () => {
    const user = userEvent.setup();
    const relay = createRelayAppGateway();

    render(
      <App
        gateway={relay.gateway}
        initialEntries={["/hosts/host-relay/threads/thread-relay"]}
      />,
    );

    expect(
      await screen.findByRole("heading", { name: /relay thread/i }),
    ).toBeVisible();

    await act(async () => {
      relay.emit({
        hostId: "host-relay",
        record: {
          ...createRelayThreadRecord(),
          approvals: [
            {
              decisions: [
                "accept",
                "acceptForSession",
                "decline",
                "cancel",
              ],
              params: {
                additionalPermissions: null,
                approvalId: null,
                availableDecisions: [
                  "accept",
                  "acceptForSession",
                  "decline",
                  "cancel",
                ],
                command: "pnpm tauri ios dev",
                commandActions: [],
                cwd: "/workspace/codex/apps/mobile",
                itemId: "item-relay-command",
                reason:
                  "Restart the relay-managed iOS shell from the phone.",
                proposedExecpolicyAmendment: null,
                proposedNetworkPolicyAmendments: null,
                skillMetadata: null,
                threadId: "thread-relay",
                turnId: "turn-relay-1",
              },
              requestId: "approval-relay-command",
              type: "command",
            },
          ],
          runtime: {
            composerMode: "steer",
            connection: "online",
            phase: "waitingOnApproval",
            statusCopy: "Waiting on approval from the relay-connected phone.",
          },
          thread: {
            ...createRelayThreadRecord().thread,
            status: { type: "active", activeFlags: ["waitingOnApproval"] },
          },
        },
        threadId: "thread-relay",
        type: "thread/update",
      });
    });

    expect(
      await screen.findByRole("heading", { name: /approval required/i }),
    ).toBeVisible();
    expect(screen.getAllByText(/pnpm tauri ios dev/i)).toHaveLength(2);

    await user.click(screen.getByRole("button", { name: /approve for session/i }));

    expect(relay.requests).toContainEqual([
      relayMethods.approvalResolve,
      {
        hostId: "host-relay",
        resolution: {
          decision: "acceptForSession",
          requestId: "approval-relay-command",
        },
        threadId: "thread-relay",
      },
    ]);
  });
});

async function createBootstrapGateway(): Promise<{
  gateway: RemoteGateway;
  pushBootstrap(
    updater: (bootstrap: RemoteBootstrap) => RemoteBootstrap,
  ): void;
}> {
  const base = createMockGateway();
  let currentBootstrap: RemoteBootstrap = {
    deviceGroups: await base.listDeviceGroups(),
    hosts: await base.listHosts(),
    session: await base.getSession(),
  };
  const listeners = new Set<(bootstrap: RemoteBootstrap) => void>();

  return {
    gateway: {
      ...base,
      async getSession() {
        return clone(currentBootstrap.session);
      },
      async listHosts() {
        return clone(currentBootstrap.hosts);
      },
      async listDeviceGroups() {
        return clone(currentBootstrap.deviceGroups);
      },
      subscribeToBootstrap(listener) {
        listeners.add(listener);
        listener(clone(currentBootstrap));
        return () => {
          listeners.delete(listener);
        };
      },
    },
    pushBootstrap(updater) {
      currentBootstrap = updater(currentBootstrap);
      for (const listener of listeners) {
        listener(clone(currentBootstrap));
      }
    },
  };
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function createRelayAppGateway(): {
  emit(event: RelaySocketClientEvent): void;
  gateway: RemoteGateway;
  requests: Array<[string, unknown]>;
} {
  const listeners = new Set<(event: RelaySocketClientEvent) => void>();
  const requests: Array<[string, unknown]> = [];
  const bootstrap = createRelayBootstrap();
  const threadRecord = createRelayThreadRecord();
  const client: RelaySocketClient = {
    close() {},
    async request<M extends RelayMethod>(
      method: M,
      ...params: RelayMethodParams[M] extends undefined
        ? []
        : [params: RelayMethodParams[M]]
    ): Promise<RelayMethodResult[M]> {
      const requestParams = params[0] as RelayMethodParams[M];
      requests.push([method, requestParams]);
      if (method === relayMethods.bootstrapGet) {
        return clone(bootstrap) as RelayMethodResult[M];
      }
      if (method === relayMethods.threadRead) {
        return clone(threadRecord) as unknown as RelayMethodResult[M];
      }
      if (method === relayMethods.threadList) {
        return [clone(threadRecord.thread)] as unknown as RelayMethodResult[M];
      }
      return undefined as RelayMethodResult[M];
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };

  return {
    emit(event) {
      for (const listener of listeners) {
        listener(event);
      }
    },
    gateway: createRelaySocketGateway({
      client,
    }),
    requests,
  };
}

function createRelayBootstrap(): RemoteBootstrap {
  return {
    deviceGroups: [],
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

function createRelayThreadRecord() {
  return {
    approvals: [],
    hostId: "host-relay",
    runtime: {
      composerMode: "newTurn" as const,
      connection: "online" as const,
      phase: "completed" as const,
      statusCopy: "Relay thread ready for input.",
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
      preview: "Relay-managed approval test.",
      source: "appServer" as const,
      status: { type: "idle" as const },
      turns: [],
      updatedAt: 1720000300,
    },
  };
}
