import { beforeEach, describe, expect, it, vi } from "vitest";
import type { RelayConnectorSnapshot } from "./types";

const bridge = vi.hoisted(() => ({
  interruptRemoteTurn: vi.fn(),
  listRemoteThreads: vi.fn(),
  readRemoteThreadRecord: vi.fn(),
  readNativeCapabilities: vi.fn(),
  readRemoteConnectorSnapshot: vi.fn(),
  sendRemotePrompt: vi.fn(),
}));
const config = vi.hoisted(() => ({
  readRemoteAppConfig: vi.fn(),
}));
const relay = vi.hoisted(() => ({
  createRelayWebSocketGateway: vi.fn(),
}));

vi.mock("../native/bridge", () => bridge);
vi.mock("./config", () => config);
vi.mock("./relayWebSocketGateway", () => relay);

import { createAppGateway } from "./appGateway";

describe("createAppGateway", () => {
  beforeEach(() => {
    bridge.listRemoteThreads.mockReset();
    bridge.readRemoteThreadRecord.mockReset();
    bridge.readNativeCapabilities.mockReset();
    bridge.readRemoteConnectorSnapshot.mockReset();
    config.readRemoteAppConfig.mockReset();
    relay.createRelayWebSocketGateway.mockReset();
    bridge.readNativeCapabilities.mockResolvedValue({
      fileImport: true,
      qrScanner: true,
      relaySockets: true,
      secureStore: true,
    });
    config.readRemoteAppConfig.mockReturnValue({});
  });

  it("uses the local preview connector when native host state is available", async () => {
    const localThreads = [
      {
        agentNickname: null,
        agentRole: null,
        cliVersion: "1.0.0",
        createdAt: 1720000000,
        cwd: "/workspace/codex",
        ephemeral: false,
        gitInfo: null,
        id: "thread-local-preview",
        modelProvider: "openai",
        name: "Local preview thread",
        path: null,
        preview: "Live host thread loaded from the app-server socket.",
        source: "appServer",
        status: { type: "idle" },
        turns: [],
        updatedAt: 1720000300,
      },
    ];
    const snapshot: RelayConnectorSnapshot = {
      connectorMode: "localPreview",
      deviceGroups: [
        {
          devices: [
            {
              id: "device-local",
              lastSeenAt: 1720000200,
              name: "Preview iPhone",
              pairedAt: 1720000000,
              transport: "Local preview",
              trust: "trusted",
            },
          ],
          host: {
            detail: "Host runtime ready for preview",
            id: "host-local-preview",
            lastSeenAt: 1720000300,
            name: "Val Dev Mac",
            pairedAt: 1720000000,
            platform: "macOS",
            relayStatus: "localPreview",
            status: "online",
          },
        },
      ],
      hosts: [
        {
          detail: "Host runtime ready for preview",
          id: "host-local-preview",
          lastSeenAt: 1720000300,
          name: "Val Dev Mac",
          pairedAt: 1720000000,
          platform: "macOS",
          relayStatus: "localPreview",
          status: "online",
        },
      ],
      session: {
        accountLabel: "Local preview",
        pairingCode: "PAIR-LOCAL",
        pairingUrl: "codex://remote/pair?code=PAIR-LOCAL",
        signedIn: true,
        workspaceLabel: "Val Dev Mac",
      },
    };
    bridge.readRemoteConnectorSnapshot.mockResolvedValue(snapshot);
    bridge.listRemoteThreads.mockResolvedValue(localThreads);

    const gateway = await createAppGateway();

    await expect(gateway.getSession()).resolves.toMatchObject({
      accountLabel: "Local preview",
      pairingCode: "PAIR-LOCAL",
      workspaceLabel: "Val Dev Mac",
      nativeCapabilities: {
        fileImport: true,
        qrScanner: true,
        relaySockets: true,
        secureStore: true,
      },
    });
    await expect(gateway.listHosts()).resolves.toEqual(snapshot.hosts);
    await expect(gateway.listDeviceGroups()).resolves.toEqual(
      snapshot.deviceGroups,
    );
    await expect(gateway.listThreads("host-local-preview")).resolves.toEqual(
      localThreads,
    );
  });

  it("falls back to the mock connector when native host state is unavailable", async () => {
    bridge.readRemoteConnectorSnapshot.mockResolvedValue(null);

    const gateway = await createAppGateway();
    const hosts = await gateway.listHosts();

    expect(hosts[0]?.id).toBe("host-studio");
    expect((await gateway.getSession()).accountLabel).toBe("val@openai.com");
  });

  it("uses the relay websocket gateway when a relay url is configured", async () => {
    bridge.readRemoteConnectorSnapshot.mockResolvedValue(null);
    const relayGateway = {
      getSession: vi.fn(),
      getThread: vi.fn(),
      inspect: vi.fn(),
      interruptTurn: vi.fn(),
      listDeviceGroups: vi.fn(),
      listHosts: vi.fn(),
      listThreads: vi.fn(),
      resolveApproval: vi.fn(),
      revokeDevice: vi.fn(),
      sendPrompt: vi.fn(),
      signIn: vi.fn(),
      subscribeToBootstrap: vi.fn(() => () => {}),
      subscribeToThread: vi.fn(() => () => {}),
    };
    config.readRemoteAppConfig.mockReturnValue({
      relayUrl: "wss://relay.example.test/mobile",
    });
    relay.createRelayWebSocketGateway.mockReturnValue(relayGateway);

    const gateway = await createAppGateway();

    expect(relay.createRelayWebSocketGateway).toHaveBeenCalledWith({
      nativeCapabilities: {
        fileImport: true,
        qrScanner: true,
        relaySockets: true,
        secureStore: true,
      },
      url: "wss://relay.example.test/mobile",
    });
    expect(gateway).toBe(relayGateway);
  });
});
