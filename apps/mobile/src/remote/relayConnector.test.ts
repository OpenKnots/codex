import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockGateway } from "./mockGateway";
import {
  createGatewayBackedRelayConnector,
  createLocalPreviewRelayConnector,
} from "./relayConnector";
import type {
  NativeCapabilities,
  RelayConnectorSnapshot,
} from "./types";

const bridge = vi.hoisted(() => ({
  listenRemoteConnectorSnapshots: vi.fn(),
  readRemoteConnectorSnapshot: vi.fn(),
  startRemoteConnectorStream: vi.fn(),
  stopRemoteConnectorStream: vi.fn(),
}));

vi.mock("../native/bridge", async () => {
  const actual = await vi.importActual("../native/bridge");
  return {
    ...actual,
    listenRemoteConnectorSnapshots: bridge.listenRemoteConnectorSnapshots,
    readRemoteConnectorSnapshot: bridge.readRemoteConnectorSnapshot,
    startRemoteConnectorStream: bridge.startRemoteConnectorStream,
    stopRemoteConnectorStream: bridge.stopRemoteConnectorStream,
  };
});

describe("relay connectors", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    bridge.listenRemoteConnectorSnapshots.mockReset();
    bridge.readRemoteConnectorSnapshot.mockReset();
    bridge.startRemoteConnectorStream.mockReset();
    bridge.stopRemoteConnectorStream.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("prefers native bootstrap events when the bridge supports streaming", async () => {
    let nativeListener:
      | ((snapshot: RelayConnectorSnapshot) => void)
      | undefined;
    const removeNativeListener = vi.fn();
    bridge.listenRemoteConnectorSnapshots.mockImplementation(
      async (listener: (snapshot: RelayConnectorSnapshot) => void) => {
        nativeListener = listener;
        return removeNativeListener;
      },
    );
    bridge.startRemoteConnectorStream.mockResolvedValue(undefined);
    bridge.stopRemoteConnectorStream.mockResolvedValue(undefined);

    const connector = createLocalPreviewRelayConnector(
      createSnapshot({
        session: {
          pairingCode: "PAIR-LOCAL",
        },
      }),
      createNativeCapabilities(),
    );
    const listener = vi.fn();

    const unsubscribe = connector.subscribe(listener);

    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({
        hosts: [expect.objectContaining({ relayStatus: "localPreview" })],
        session: expect.objectContaining({ pairingCode: "PAIR-LOCAL" }),
      }),
    );

    await vi.waitFor(() => {
      expect(bridge.startRemoteConnectorStream).toHaveBeenCalledTimes(1);
    });

    await vi.advanceTimersByTimeAsync(2_100);
    expect(bridge.readRemoteConnectorSnapshot).not.toHaveBeenCalled();

    nativeListener?.(
      createSnapshot({
        hosts: [
          {
            relayStatus: "relayConnected",
          },
        ],
        session: {
          pairingCode: "PAIR-EVENT",
        },
      }),
    );

    await vi.waitFor(() => {
      expect(listener).toHaveBeenLastCalledWith(
        expect.objectContaining({
          hosts: [expect.objectContaining({ relayStatus: "relayConnected" })],
          session: expect.objectContaining({ pairingCode: "PAIR-EVENT" }),
        }),
      );
    });

    unsubscribe();

    expect(removeNativeListener).toHaveBeenCalledTimes(1);
    expect(bridge.stopRemoteConnectorStream).toHaveBeenCalledTimes(1);
  });

  it("falls back to polling when native bootstrap streaming is unavailable", async () => {
    bridge.listenRemoteConnectorSnapshots.mockResolvedValue(() => {});
    bridge.startRemoteConnectorStream.mockRejectedValue(
      new Error("stream unavailable"),
    );

    const connector = createLocalPreviewRelayConnector(
      createSnapshot({
        session: {
          pairingCode: "PAIR-LOCAL",
        },
      }),
      createNativeCapabilities(),
    );
    const listener = vi.fn();
    bridge.readRemoteConnectorSnapshot.mockResolvedValue(
      createSnapshot({
        hosts: [
          {
            relayStatus: "relayConnected",
          },
        ],
        session: {
          pairingCode: "PAIR-UPDATED",
        },
      }),
    );

    const unsubscribe = connector.subscribe(listener);

    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({
        hosts: [expect.objectContaining({ relayStatus: "localPreview" })],
        session: expect.objectContaining({ pairingCode: "PAIR-LOCAL" }),
      }),
    );

    await vi.advanceTimersByTimeAsync(2_100);

    await vi.waitFor(() => {
      expect(listener).toHaveBeenLastCalledWith(
        expect.objectContaining({
          hosts: [expect.objectContaining({ relayStatus: "relayConnected" })],
          session: expect.objectContaining({ pairingCode: "PAIR-UPDATED" }),
        }),
      );
    });

    unsubscribe();
  });

  it("keeps optimistic device revocations when local preview snapshots refresh", async () => {
    const connector = createLocalPreviewRelayConnector(
      createSnapshot(),
      createNativeCapabilities(),
    );
    const listener = vi.fn();
    bridge.readRemoteConnectorSnapshot.mockResolvedValue(createSnapshot());

    const unsubscribe = connector.subscribe(listener);

    await connector.revokeDevice("host-local-preview", "device-local");
    await vi.advanceTimersByTimeAsync(2_100);

    await vi.waitFor(() => {
      expect(listener).toHaveBeenLastCalledWith(
        expect.objectContaining({
          deviceGroups: [
            expect.objectContaining({
              devices: [expect.objectContaining({ trust: "revoked" })],
            }),
          ],
        }),
      );
    });

    unsubscribe();
  });

  it("publishes bootstrap updates from gateway-backed connectors after local actions", async () => {
    const connector = createGatewayBackedRelayConnector(createMockGateway());
    const listener = vi.fn();

    const unsubscribe = connector.subscribe(listener);

    await vi.waitFor(() => {
      expect(listener.mock.calls[0]?.[0].deviceGroups[0]?.devices[0]?.trust).toBe(
        "trusted",
      );
    });

    await connector.revokeDevice("host-studio", "device-iphone-main");

    await vi.waitFor(() => {
      expect(
        listener.mock.calls.at(-1)?.[0].deviceGroups[0]?.devices[0]?.trust,
      ).toBe("revoked");
      expect(
        listener.mock.calls.at(-1)?.[0].deviceGroups[0]?.host.id,
      ).toBe("host-studio");
    });

    unsubscribe();
  });
});

function createSnapshot(overrides?: {
  hosts?: Array<Partial<RelayConnectorSnapshot["hosts"][number]>>;
  session?: Partial<RelayConnectorSnapshot["session"]>;
}): RelayConnectorSnapshot {
  return {
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
          relayStatus: overrides?.hosts?.[0]?.relayStatus ?? "localPreview",
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
        relayStatus: overrides?.hosts?.[0]?.relayStatus ?? "localPreview",
        status: "online",
      },
    ],
    session: {
      accountLabel: "Local preview",
      pairingCode: overrides?.session?.pairingCode ?? "PAIR-LOCAL",
      pairingUrl: "codex://remote/pair?code=PAIR-LOCAL",
      signedIn: true,
      workspaceLabel: "Val Dev Mac",
    },
  };
}

function createNativeCapabilities(): NativeCapabilities {
  return {
    fileImport: true,
    qrScanner: true,
    relaySockets: true,
    secureStore: true,
  };
}
